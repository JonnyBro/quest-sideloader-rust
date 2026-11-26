//! Writes a signed APK: the source's entries, some replaced, with APK
//! signature schemes v1 (JAR) and v2. Quest firmware wants v2; v1 is for
//! tools that only know the old scheme.

use base64::Engine as _;
use std::collections::HashMap;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::der;
use super::keys::{KeyError, SigningKey};
use super::zipw::{self, EntryInfo, ZipWriteError, ZipWriter};

pub const V2_BLOCK_ID: u32 = 0x7109_871a;
pub const RSA_PKCS1_SHA256: u32 = 0x0103;
pub const BLOCK_MAGIC: &[u8; 16] = b"APK Sig Block 42";
const CHUNK: usize = 1 << 20;
const SIGNER: &str = "CERT";
/// 1981-01-01 00:00, as `apksigner` dates the files it adds.
const DOS_DATE: u16 = (1 << 9) | (1 << 5) | 1;

#[derive(Debug, thiserror::Error)]
pub enum SignError {
	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
	#[error("{path}: {message}")]
	Zip { path: PathBuf, message: String },
	#[error(transparent)]
	Key(#[from] KeyError),
}

/// Old signatures, which would no longer match.
fn is_signature_file(name: &str) -> bool {
	let Some(file) = name.strip_prefix("META-INF/") else {
		return false;
	};
	if file.contains('/') {
		return false;
	}
	let upper = file.to_ascii_uppercase();
	upper == "MANIFEST.MF"
		|| [".SF", ".RSA", ".DSA", ".EC"]
			.iter()
			.any(|ext| upper.ends_with(ext))
		|| upper.starts_with("SIG-")
}

/// Entries in `replace` are stored uncompressed instead of the source's.
/// `progress` gets `(done, total)` in bytes.
pub fn write_signed_apk(
	source: &Path,
	dest: &Path,
	replace: &HashMap<String, Vec<u8>>,
	key: &SigningKey,
	progress: &dyn Fn(u64, u64),
) -> Result<(), SignError> {
	let io_src = |source_err| SignError::Io {
		path: source.to_path_buf(),
		source: source_err,
	};
	let io_dest = |source_err| SignError::Io {
		path: dest.to_path_buf(),
		source: source_err,
	};
	let zip_err = |e: zip::result::ZipError| SignError::Zip {
		path: source.to_path_buf(),
		message: e.to_string(),
	};
	let write_err = |e: ZipWriteError| match e {
		ZipWriteError::Io(source) => SignError::Io {
			path: dest.to_path_buf(),
			source,
		},
		other => SignError::Zip {
			path: dest.to_path_buf(),
			message: other.to_string(),
		},
	};

	let file = std::fs::File::open(source).map_err(io_src)?;
	let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
	let mut names = Vec::with_capacity(archive.len());
	let mut total = 0u64;
	for i in 0..archive.len() {
		let entry = archive.by_index_raw(i).map_err(zip_err)?;
		let name = entry.name().map_err(zip_err)?.into_owned();
		if is_signature_file(&name) {
			continue;
		}
		// Hashed once uncompressed, copied once compressed; replaced ones
		// aren't read at all.
		if !replace.contains_key(&name) {
			total += entry.size() + entry.compressed_size();
		}
		names.push((i, name));
	}
	let done = std::cell::Cell::new(0u64);
	let advance = |n: u64| {
		done.set(done.get() + n);
		progress(done.get(), total);
	};

	// v1 needs every file's digest before anything is written.
	let mut digests: Vec<(String, [u8; 32])> = Vec::new();
	for (i, name) in &names {
		if name.ends_with('/') {
			continue;
		}
		let digest = match replace.get(name) {
			Some(data) => Sha256::digest(data).into(),
			None => {
				let mut entry = archive.by_index(*i).map_err(zip_err)?;
				let size = entry.size();
				let digest = hash_reader(&mut entry).map_err(io_src)?;
				advance(size);
				digest
			}
		};
		digests.push((name.clone(), digest));
	}
	for (name, data) in replace {
		if !names.iter().any(|(_, n)| n == name) {
			digests.push((name.clone(), Sha256::digest(data).into()));
		}
	}
	let (manifest, sf) = jar_manifests(&digests);
	let pkcs7 = pkcs7_signature(key, &sf)?;

	// Read back for the v2 digest, so not write-only.
	let out = std::fs::OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(true)
		.open(dest)
		.map_err(io_dest)?;
	let mut zip = ZipWriter::new(std::io::BufWriter::new(out)).map_err(io_dest)?;
	type Out = ZipWriter<std::io::BufWriter<std::fs::File>>;
	let stored = |zip: &mut Out, name: &str, data: &[u8], time: (u16, u16)| {
		zip.start(&EntryInfo {
			name: name.to_owned(),
			method: zipw::STORED,
			crc32: crc32(data),
			compressed_size: data.len() as u64,
			size: data.len() as u64,
			dos_time: time.0,
			dos_date: time.1,
		})
		.map_err(write_err)?;
		zip.data(data).map_err(io_dest)
	};
	for (i, name) in &names {
		let mut entry = archive.by_index_raw(*i).map_err(zip_err)?;
		let time = entry
			.last_modified()
			.map_or((0, DOS_DATE), |t| (t.timepart(), t.datepart()));
		if let Some(data) = replace.get(name) {
			stored(&mut zip, name, data, time)?;
			continue;
		}
		let method = match entry.compression() {
			zip::CompressionMethod::Stored => zipw::STORED,
			zip::CompressionMethod::Deflated => 8,
			other => {
				return Err(SignError::Zip {
					path: source.to_path_buf(),
					message: format!("{name}: unsupported compression {other:?}"),
				});
			}
		};
		zip.start(&EntryInfo {
			name: name.clone(),
			method,
			crc32: entry.crc32(),
			compressed_size: entry.compressed_size(),
			size: entry.size(),
			dos_time: time.0,
			dos_date: time.1,
		})
		.map_err(write_err)?;
		zip.copy(&mut entry).map_err(io_src)?;
		advance(entry.compressed_size());
	}
	for (name, data) in replace {
		if !names.iter().any(|(_, n)| n == name) {
			stored(&mut zip, name, data, (0, DOS_DATE))?;
		}
	}
	stored(&mut zip, "META-INF/MANIFEST.MF", &manifest, (0, DOS_DATE))?;
	stored(
		&mut zip,
		&format!("META-INF/{SIGNER}.SF"),
		&sf,
		(0, DOS_DATE),
	)?;
	stored(
		&mut zip,
		&format!("META-INF/{SIGNER}.RSA"),
		&pkcs7,
		(0, DOS_DATE),
	)?;

	let (writer, central_offset, central, mut end) = zip.finish().map_err(write_err)?;
	let mut out = writer.into_inner().map_err(|e| io_dest(e.into_error()))?;
	out.flush().map_err(io_dest)?;

	// The v2 digest covers the entries, the central directory and the end
	// record with its offset pointing at where the signing block will go,
	// which is where the central directory starts now.
	let mut digest = ContentDigest::default();
	out.seek(SeekFrom::Start(0)).map_err(io_dest)?;
	digest
		.section(&mut (&mut out).take(central_offset), central_offset)
		.map_err(io_dest)?;
	digest
		.section(&mut central.as_slice(), central.len() as u64)
		.map_err(io_dest)?;
	digest
		.section(&mut end.as_slice(), end.len() as u64)
		.map_err(io_dest)?;
	let block = v2_block(key, &digest.finish())?;

	out.seek(SeekFrom::Start(central_offset)).map_err(io_dest)?;
	zipw::set_central_offset(&mut end, central_offset + block.len() as u64).map_err(write_err)?;
	out.write_all(&block).map_err(io_dest)?;
	out.write_all(&central).map_err(io_dest)?;
	out.write_all(&end).map_err(io_dest)?;
	out.set_len(central_offset + (block.len() + central.len() + end.len()) as u64)
		.map_err(io_dest)?;
	out.sync_all().map_err(io_dest)
}

fn hash_reader(reader: &mut impl Read) -> std::io::Result<[u8; 32]> {
	let mut hasher = Sha256::new();
	let mut buf = vec![0u8; 256 * 1024];
	loop {
		let n = reader.read(&mut buf)?;
		if n == 0 {
			return Ok(hasher.finalize().into());
		}
		hasher.update(&buf[..n]);
	}
}

fn crc32(data: &[u8]) -> u32 {
	let mut crc = !0u32;
	for &byte in data {
		crc ^= u32::from(byte);
		for _ in 0..8 {
			crc = if crc & 1 != 0 {
				(crc >> 1) ^ 0xedb8_8320
			} else {
				crc >> 1
			};
		}
	}
	!crc
}

fn b64(bytes: &[u8]) -> String {
	base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A manifest line, wrapped at 72 bytes with continuation lines starting
/// with a space, as the JAR format wants.
fn push_line(out: &mut Vec<u8>, line: &str) {
	let bytes = line.as_bytes();
	let first = bytes.len().min(70);
	out.extend_from_slice(&bytes[..first]);
	out.extend_from_slice(b"\r\n");
	for chunk in bytes[first..].chunks(69) {
		out.push(b' ');
		out.extend_from_slice(chunk);
		out.extend_from_slice(b"\r\n");
	}
}

fn jar_manifests(digests: &[(String, [u8; 32])]) -> (Vec<u8>, Vec<u8>) {
	let mut sorted: Vec<&(String, [u8; 32])> = digests.iter().collect();
	sorted.sort_by(|a, b| a.0.cmp(&b.0));

	let mut manifest = Vec::new();
	push_line(&mut manifest, "Manifest-Version: 1.0");
	push_line(&mut manifest, "Created-By: Quest Sideloader");
	manifest.extend_from_slice(b"\r\n");
	let mut sections = Vec::new();
	for (name, digest) in &sorted {
		let mut section = Vec::new();
		push_line(&mut section, &format!("Name: {name}"));
		push_line(&mut section, &format!("SHA-256-Digest: {}", b64(digest)));
		section.extend_from_slice(b"\r\n");
		sections.push((name, Sha256::digest(&section)));
		manifest.extend_from_slice(&section);
	}

	let mut sf = Vec::new();
	push_line(&mut sf, "Signature-Version: 1.0");
	push_line(&mut sf, "Created-By: Quest Sideloader");
	push_line(
		&mut sf,
		&format!(
			"SHA-256-Digest-Manifest: {}",
			b64(&Sha256::digest(&manifest))
		),
	);
	// Tells verifiers that know v2 to insist on it, so stripping the v2
	// block can't downgrade the APK to v1 only.
	push_line(&mut sf, "X-Android-APK-Signed: 2");
	sf.extend_from_slice(b"\r\n");
	for (name, digest) in sections {
		push_line(&mut sf, &format!("Name: {name}"));
		push_line(&mut sf, &format!("SHA-256-Digest: {}", b64(&digest)));
		sf.extend_from_slice(b"\r\n");
	}
	(manifest, sf)
}

/// PKCS#7 `SignedData` over the signature file, without the content.
fn pkcs7_signature(key: &SigningKey, sf: &[u8]) -> Result<Vec<u8>, KeyError> {
	let (issuer, serial) = key.issuer_and_serial()?;
	let signature = key.sign(sf)?;
	let signer = der::seq(&[
		&der::uint(&[1]),
		&der::seq(&[&issuer, &serial]),
		&der::algorithm(der::SHA256),
		&der::algorithm(der::RSA),
		&der::tlv(der::OCTET_STRING, &signature),
	]);
	let signed_data = der::seq(&[
		&der::uint(&[1]),
		&der::set(&[&der::algorithm(der::SHA256)]),
		&der::seq(&[&der::oid(der::PKCS7_DATA)]),
		&der::explicit(0, key.certificate()),
		&der::set(&[&signer]),
	]);
	Ok(der::seq(&[
		&der::oid(der::PKCS7_SIGNED_DATA),
		&der::explicit(0, &signed_data),
	]))
}

/// The v2 content digest: SHA-256 over 1 MiB chunks of each section, then
/// over the chunk digests.
#[derive(Default)]
pub struct ContentDigest {
	chunks: Vec<u8>,
	count: u32,
}

impl ContentDigest {
	pub fn section(&mut self, reader: &mut impl Read, len: u64) -> std::io::Result<()> {
		let mut buf = vec![0u8; CHUNK];
		let mut left = len;
		while left > 0 {
			let n = left.min(CHUNK as u64) as usize;
			reader.read_exact(&mut buf[..n])?;
			let mut hasher = Sha256::new();
			hasher.update([0xa5]);
			hasher.update((n as u32).to_le_bytes());
			hasher.update(&buf[..n]);
			self.chunks.extend_from_slice(&hasher.finalize());
			self.count += 1;
			left -= n as u64;
		}
		Ok(())
	}

	pub fn finish(&self) -> [u8; 32] {
		let mut hasher = Sha256::new();
		hasher.update([0x5a]);
		hasher.update(self.count.to_le_bytes());
		hasher.update(&self.chunks);
		hasher.finalize().into()
	}
}

/// `bytes` with its length in front, the way every v2 field is written.
pub fn prefixed(bytes: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(bytes.len() + 4);
	out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
	out.extend_from_slice(bytes);
	out
}

fn v2_block(key: &SigningKey, digest: &[u8; 32]) -> Result<Vec<u8>, KeyError> {
	let mut digest_entry = RSA_PKCS1_SHA256.to_le_bytes().to_vec();
	digest_entry.extend(prefixed(digest));
	let signed_data = [
		prefixed(&prefixed(&digest_entry)),
		prefixed(&prefixed(key.certificate())),
		prefixed(&[]),
	]
	.concat();
	let mut signature = RSA_PKCS1_SHA256.to_le_bytes().to_vec();
	signature.extend(prefixed(&key.sign(&signed_data)?));
	let signer = [
		prefixed(&signed_data),
		prefixed(&prefixed(&signature)),
		prefixed(key.public_key()),
	]
	.concat();
	let value = prefixed(&prefixed(&signer));

	let mut pair = ((value.len() + 4) as u64).to_le_bytes().to_vec();
	pair.extend_from_slice(&V2_BLOCK_ID.to_le_bytes());
	pair.extend_from_slice(&value);
	// The size fields count everything after the first one.
	let size = (pair.len() + 8 + BLOCK_MAGIC.len()) as u64;
	let mut block = size.to_le_bytes().to_vec();
	block.extend_from_slice(&pair);
	block.extend_from_slice(&size.to_le_bytes());
	block.extend_from_slice(BLOCK_MAGIC);
	Ok(block)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rename::keys::tests::test_key;
	use crate::rename::verify::verify_v2;
	use rsa::pkcs8::DecodePublicKey;

	fn source_apk(dir: &Path) -> PathBuf {
		let path = dir.join("source.apk");
		let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
		let deflated = zip::write::SimpleFileOptions::default()
			.compression_method(zip::CompressionMethod::Deflated);
		let stored = zip::write::SimpleFileOptions::default()
			.compression_method(zip::CompressionMethod::Stored);
		zip.start_file("AndroidManifest.xml", deflated).unwrap();
		zip.write_all(b"old manifest").unwrap();
		zip.start_file("classes.dex", deflated).unwrap();
		zip.write_all(&[7u8; 5000]).unwrap();
		zip.start_file("lib/arm64-v8a/libgame.so", stored).unwrap();
		zip.write_all(&[1u8; 3000]).unwrap();
		zip.start_file("resources.arsc", stored).unwrap();
		zip.write_all(b"arsc").unwrap();
		zip.start_file("META-INF/MANIFEST.MF", deflated).unwrap();
		zip.write_all(b"Manifest-Version: 1.0\r\n").unwrap();
		zip.start_file("META-INF/OLD.RSA", deflated).unwrap();
		zip.write_all(b"old").unwrap();
		zip.start_file("META-INF/services/x", deflated).unwrap();
		zip.write_all(b"kept").unwrap();
		zip.finish().unwrap();
		path
	}

	fn signed(dir: &Path) -> PathBuf {
		let source = source_apk(dir);
		let dest = dir.join("signed.apk");
		let replace = HashMap::from([("AndroidManifest.xml".to_owned(), b"new manifest".to_vec())]);
		let last = std::cell::Cell::new((0, 0));
		write_signed_apk(&source, &dest, &replace, test_key(), &|d, t| {
			last.set((d, t))
		})
		.unwrap();
		let (done, total) = last.get();
		assert_eq!(done, total);
		dest
	}

	#[test]
	fn writes_a_v2_signed_aligned_apk() {
		let dir = tempfile::tempdir().unwrap();
		let dest = signed(dir.path());
		let cert = verify_v2(&dest).unwrap();
		assert_eq!(cert, test_key().certificate());

		let mut archive = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
		let mut names: Vec<String> = archive
			.file_names()
			.map(|n| n.unwrap().into_owned())
			.collect();
		names.sort();
		assert_eq!(
			names,
			[
				"AndroidManifest.xml",
				"META-INF/CERT.RSA",
				"META-INF/CERT.SF",
				"META-INF/MANIFEST.MF",
				"META-INF/services/x",
				"classes.dex",
				"lib/arm64-v8a/libgame.so",
				"resources.arsc",
			]
		);
		let mut read = |name: &str| {
			let mut data = Vec::new();
			archive
				.by_name(name)
				.unwrap()
				.read_to_end(&mut data)
				.unwrap();
			data
		};
		assert_eq!(read("AndroidManifest.xml"), b"new manifest");
		assert_eq!(read("classes.dex"), vec![7u8; 5000]);
		for (name, align) in [("lib/arm64-v8a/libgame.so", 4096), ("resources.arsc", 4)] {
			let entry = archive.by_name(name).unwrap();
			assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
			assert_eq!(entry.data_start().unwrap() % align, 0, "{name}");
		}
	}

	#[test]
	fn a_changed_byte_breaks_the_v2_signature() {
		let dir = tempfile::tempdir().unwrap();
		let dest = signed(dir.path());
		let mut data = std::fs::read(&dest).unwrap();
		data[40] ^= 1;
		std::fs::write(&dest, data).unwrap();
		assert!(verify_v2(&dest).is_err());
	}

	#[test]
	fn v1_signature_matches_the_files() {
		let dir = tempfile::tempdir().unwrap();
		let dest = signed(dir.path());
		let mut archive = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
		let mut read = |name: &str| {
			let mut data = Vec::new();
			archive
				.by_name(name)
				.unwrap()
				.read_to_end(&mut data)
				.unwrap();
			data
		};
		let manifest = String::from_utf8(read("META-INF/MANIFEST.MF")).unwrap();
		let sf = read("META-INF/CERT.SF");
		let pkcs7 = read("META-INF/CERT.RSA");

		for section in manifest.split("\r\n\r\n").skip(1).filter(|s| !s.is_empty()) {
			let unwrapped = section.replace("\r\n ", "");
			let mut lines = unwrapped.lines();
			let name = lines.next().unwrap().strip_prefix("Name: ").unwrap();
			let digest = lines
				.next()
				.unwrap()
				.strip_prefix("SHA-256-Digest: ")
				.unwrap();
			assert_eq!(digest, b64(&Sha256::digest(read(name))), "{name}");
		}
		let sf_text = String::from_utf8(sf.clone()).unwrap();
		assert!(sf_text.contains(&format!(
			"SHA-256-Digest-Manifest: {}",
			b64(&Sha256::digest(manifest.as_bytes()))
		)));

		// ContentInfo -> [0] -> SignedData -> last element is the signer set.
		let content_info = der::read(&pkcs7).unwrap();
		let outer = der::children(content_info.content).unwrap();
		let signed_data = der::read(outer[1].content).unwrap();
		let fields = der::children(signed_data.content).unwrap();
		let signer = der::read(fields.last().unwrap().content).unwrap();
		let signer_fields = der::children(signer.content).unwrap();
		let signature = signer_fields.last().unwrap().content;
		rsa::PublicKey::from_public_key_der(test_key().public_key())
			.unwrap()
			.pkcs1v15_verify::<Sha256>(&sf, signature)
			.unwrap();
	}

	#[test]
	fn long_names_wrap_at_72_bytes() {
		let mut out = Vec::new();
		push_line(&mut out, &format!("Name: {}", "a".repeat(200)));
		let text = String::from_utf8(out).unwrap();
		assert!(text.split("\r\n").all(|l| l.len() <= 70));
		assert_eq!(
			text.replace("\r\n ", "").trim_end(),
			format!("Name: {}", "a".repeat(200))
		);
	}

	#[test]
	fn crc_matches_zip() {
		assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
	}

	/// `apksigner` from the Android SDK, when it's on PATH, as a second opinion.
	#[test]
	fn apksigner_accepts_it() {
		if std::process::Command::new("apksigner")
			.arg("--version")
			.output()
			.is_err()
		{
			eprintln!("skipped: no apksigner");
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let dest = signed(dir.path());
		let out = std::process::Command::new("apksigner")
			.args(["verify", "-v", "--min-sdk-version", "29"])
			.arg(&dest)
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}
