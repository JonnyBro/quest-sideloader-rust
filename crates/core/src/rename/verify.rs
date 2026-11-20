//! Checks an APK's v2 signature the way Android does, so a broken copy is
//! caught on the computer instead of failing to install.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use rsa::pkcs8::DecodePublicKey;
use sha2::Sha256;

use super::chunk::Reader;
use super::sign::{BLOCK_MAGIC, ContentDigest, RSA_PKCS1_SHA256, V2_BLOCK_ID};

struct SigningBlock {
	start: u64,
	central_offset: u64,
	end_offset: u64,
	/// The zip end record.
	end: Vec<u8>,
	/// `(id, value)` of each entry: v2, v3 and so on.
	pairs: Vec<(u32, Vec<u8>)>,
}

/// Finds the APK signing block before the central directory. `Ok(None)`
/// when the APK has none (signed with v1 only, or not at all).
fn read_signing_block(file: &mut std::fs::File) -> Result<Option<SigningBlock>, String> {
	let len = file.metadata().map_err(|e| e.to_string())?.len();

	// The end record is the last 22 bytes when there is no comment, which is
	// how APKs are written; a comment would need a search.
	let tail_len = len.min(22 + 0xffff);
	let mut tail = vec![0u8; tail_len as usize];
	file.seek(SeekFrom::Start(len - tail_len))
		.map_err(|e| e.to_string())?;
	file.read_exact(&mut tail).map_err(|e| e.to_string())?;
	let end_at = (0..=tail.len().saturating_sub(22))
		.rev()
		.find(|&i| {
			tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]
				&& usize::from(u16::from_le_bytes([tail[i + 20], tail[i + 21]]))
					== tail.len() - i - 22
		})
		.ok_or("no zip end record")?;
	let end = tail[end_at..].to_vec();
	let end_offset = len - tail_len + end_at as u64;
	let central_offset = u64::from(u32::from_le_bytes([end[16], end[17], end[18], end[19]]));

	let Some(footer_at) = central_offset.checked_sub(24) else {
		return Ok(None);
	};
	let mut footer = [0u8; 24];
	file.seek(SeekFrom::Start(footer_at))
		.map_err(|e| e.to_string())?;
	file.read_exact(&mut footer).map_err(|e| e.to_string())?;
	if &footer[8..] != BLOCK_MAGIC {
		return Ok(None);
	}
	let size = u64::from_le_bytes(footer[..8].try_into().unwrap_or_default());
	let start = central_offset
		.checked_sub(size + 8)
		.ok_or("bad signing block size")?;
	let mut block = vec![0u8; (size + 8) as usize];
	file.seek(SeekFrom::Start(start))
		.map_err(|e| e.to_string())?;
	file.read_exact(&mut block).map_err(|e| e.to_string())?;
	if u64::from_le_bytes(block[..8].try_into().unwrap_or_default()) != size {
		return Err("signing block sizes differ".into());
	}

	let raw = &block[8..block.len() - 24];
	let mut pairs = Vec::new();
	let mut at = 0;
	while at + 12 <= raw.len() {
		let pair_len = u64::from_le_bytes(raw[at..at + 8].try_into().unwrap_or_default()) as usize;
		let body = raw
			.get(at + 8..at + 8 + pair_len)
			.ok_or("bad pair length")?;
		let id = u32::from_le_bytes(body[..4].try_into().unwrap_or_default());
		pairs.push((id, body[4..].to_vec()));
		at += 8 + pair_len;
	}
	Ok(Some(SigningBlock {
		start,
		central_offset,
		end_offset,
		end,
		pairs,
	}))
}

/// Ids of the entries in the APK's signing block, empty without one.
pub fn signing_block_ids(path: &Path) -> Result<Vec<u32>, String> {
	let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
	Ok(read_signing_block(&mut file)?
		.map(|b| b.pairs.iter().map(|(id, _)| *id).collect())
		.unwrap_or_default())
}

/// Returns the signer's certificate if the v2 signature is valid.
pub fn verify_v2(path: &Path) -> Result<Vec<u8>, String> {
	let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
	let SigningBlock {
		start: block_start,
		central_offset,
		end_offset,
		mut end,
		pairs,
	} = read_signing_block(&mut file)?.ok_or("no APK signing block")?;
	let v2 = pairs
		.iter()
		.find(|(id, _)| *id == V2_BLOCK_ID)
		.map(|(_, v)| v.as_slice())
		.ok_or("no v2 signature")?;

	let signers = items(prefixed_at(v2, 0)?)?;
	let signer = signers.first().ok_or("no signer")?;
	let signed_data = prefixed_at(signer, 0)?;
	let signatures = items(prefixed_at(signer, 4 + signed_data.len())?)?;
	let public_key = prefixed_at(
		signer,
		8 + signed_data.len() + raw_len(signer, 4 + signed_data.len())?,
	)?;

	let key = rsa::PublicKey::from_public_key_der(public_key).map_err(|e| e.to_string())?;
	let signature = signatures
		.iter()
		.find(|s| algorithm(s) == Some(RSA_PKCS1_SHA256))
		.ok_or("no RSA/SHA-256 signature")?;
	key.pkcs1v15_verify::<Sha256>(signed_data, prefixed_at(signature, 4)?)
		.map_err(|_| "the signature doesn't match")?;

	let digests = items(prefixed_at(signed_data, 0)?)?;
	let digest_entry = digests
		.iter()
		.find(|d| algorithm(d) == Some(RSA_PKCS1_SHA256))
		.ok_or("no SHA-256 digest")?;
	let expected = prefixed_at(digest_entry, 4)?;
	let certs_raw = prefixed_at(signed_data, 4 + raw_len(signed_data, 0)?)?;
	let certificate = items(certs_raw)?.first().ok_or("no certificate")?.to_vec();
	if !certificate
		.windows(public_key.len())
		.any(|w| w == public_key)
	{
		return Err("the certificate is for another key".into());
	}

	// As signed, the end record pointed at the signing block.
	end[16..20].copy_from_slice(&(block_start as u32).to_le_bytes());
	let mut digest = ContentDigest::default();
	let io = |e: std::io::Error| e.to_string();
	file.seek(SeekFrom::Start(0)).map_err(io)?;
	digest
		.section(&mut (&mut file).take(block_start), block_start)
		.map_err(io)?;
	file.seek(SeekFrom::Start(central_offset)).map_err(io)?;
	let central_len = end_offset - central_offset;
	digest
		.section(&mut (&mut file).take(central_len), central_len)
		.map_err(io)?;
	digest
		.section(&mut end.as_slice(), end.len() as u64)
		.map_err(io)?;
	if digest.finish().as_slice() != expected {
		return Err("the APK changed after signing".into());
	}
	Ok(certificate)
}

fn raw_len(data: &[u8], at: usize) -> Result<usize, String> {
	Ok(Reader::new(data).u32(at).map_err(|e| e.to_string())? as usize)
}

/// The length-prefixed field at `at`.
fn prefixed_at(data: &[u8], at: usize) -> Result<&[u8], String> {
	let len = raw_len(data, at)?;
	data.get(at + 4..at + 4 + len)
		.ok_or_else(|| "field runs past its parent".to_owned())
}

/// The length-prefixed fields that fill `data`.
fn items(data: &[u8]) -> Result<Vec<&[u8]>, String> {
	let mut out = Vec::new();
	let mut at = 0;
	while at < data.len() {
		let item = prefixed_at(data, at)?;
		at += 4 + item.len();
		out.push(item);
	}
	Ok(out)
}

fn algorithm(item: &[u8]) -> Option<u32> {
	item.get(..4)
		.map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
