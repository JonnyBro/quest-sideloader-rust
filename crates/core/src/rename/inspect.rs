//! Everything worth knowing about an APK, for the Inspector tab. Lives here
//! because it reads the same formats renaming does.

use std::io::Read;
use std::path::{Path, PathBuf};

use md5::Md5;
use sha2::{Digest, Sha256};

use super::analyze::{DEBUGGABLE, PLATFORM_LOADER};
use super::arsc::ResourceTable;
use super::axml::{Attribute, Axml, Element};
use super::der;
use super::keys::KeyStore;
use super::sign::V2_BLOCK_ID;
use super::verify::{signing_block_ids, verify_v2};
use crate::sideload::{InstallPlan, SideloadError, Source, read_apk_meta};

const V3_BLOCK_ID: u32 = 0xf053_68c0;
const V31_BLOCK_ID: u32 = 0x1b93_ad61;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feature {
	pub name: String,
	pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
	/// Common name and organization, as far as the certificate has them.
	pub subject: String,
	pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
	/// "v1", "v2", "v3", "v3.1".
	pub schemes: Vec<&'static str>,
	/// The v2 check, when the APK has a v2 signature.
	pub v2_valid: Option<Result<(), String>>,
	pub certificates: Vec<Certificate>,
	/// The copy's package, when the APK is signed with one of our keys.
	pub ours: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
	pub apk: PathBuf,
	pub size: u64,
	pub package: String,
	pub label: Option<String>,
	pub version_code: Option<u64>,
	pub version_name: Option<String>,
	pub min_sdk: Option<String>,
	pub target_sdk: Option<String>,
	pub compile_sdk: Option<String>,
	pub gl_es: Option<String>,
	pub debuggable: bool,
	pub extract_native_libs: Option<bool>,
	/// From `com.oculus.supportedDevices`, like `quest2`, `quest3`.
	pub supported_devices: Vec<String>,
	pub features: Vec<Feature>,
	pub permissions: Vec<String>,
	/// `(kind, count)` for activities, services, receivers and providers.
	pub components: Vec<(&'static str, usize)>,
	pub abis: Vec<String>,
	pub locales: Vec<String>,
	pub dex_files: usize,
	pub native_libraries: usize,
	pub entries: usize,
	pub platform_loader: bool,
	pub firebase: bool,
	pub sha256: String,
	pub md5: String,
	pub signature: Signature,
}

const MIN_SDK: u32 = 0x0101_020c;
const TARGET_SDK: u32 = 0x0101_0270;
const COMPILE_SDK: u32 = 0x0101_0572;
const NAME: u32 = 0x0101_0003;
const VALUE: u32 = 0x0101_0024;
const REQUIRED: u32 = 0x0101_028e;
const GL_ES: u32 = 0x0101_0281;
const EXTRACT_NATIVE_LIBS: u32 = 0x0101_04ea;

fn find<'a>(element: &'a Element, name: &str, id: u32) -> Option<&'a Attribute> {
	element.attributes.iter().find(|a| a.is_android(name, id))
}

/// Reads an APK, or the first APK of a release folder. `keys` tells our own
/// copies apart.
pub fn inspect(path: &Path, keys: &KeyStore) -> Result<Inspection, SideloadError> {
	let apk = match Source::classify(path)? {
		Source::Apk(apk) => apk,
		Source::Folder(dir) => InstallPlan::from_folder(&dir)?
			.apks
			.first()
			.map(|a| a.path.clone())
			.ok_or_else(|| SideloadError::NothingToInstall(path.to_path_buf()))?,
		Source::Zip(_) => return Err(SideloadError::Unsupported(path.to_path_buf())),
	};
	let meta = read_apk_meta(&apk)?;
	let apk_err = |message: String| SideloadError::Apk {
		path: apk.clone(),
		message,
	};

	let file = std::fs::File::open(&apk).map_err(crate::sideload::io_err(&apk))?;
	let mut zip =
		zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| apk_err(e.to_string()))?;
	let mut read = |name: &str| -> Result<Option<Vec<u8>>, SideloadError> {
		let mut entry = match zip.by_name(name) {
			Ok(entry) => entry,
			Err(zip::result::ZipError::FileNotFound) => return Ok(None),
			Err(e) => return Err(apk_err(e.to_string())),
		};
		let mut data = Vec::new();
		entry
			.read_to_end(&mut data)
			.map_err(|e| apk_err(e.to_string()))?;
		Ok(Some(data))
	};

	let manifest = read("AndroidManifest.xml")?
		.ok_or_else(|| apk_err(rust_i18n::t!("ren.no_manifest").into_owned()))?;
	let xml = Axml::parse(&manifest).map_err(|e| apk_err(e.to_string()))?;
	let elements = xml.elements().map_err(|e| apk_err(e.to_string()))?;
	let first = |name: &str| elements.iter().find(|e| e.name == name);

	let sdk = |id: u32, name: &str| {
		first("uses-sdk")
			.and_then(|e| find(e, name, id))
			.map(Attribute::text)
	};
	let compile_sdk = first("manifest")
		.and_then(|e| find(e, "compileSdkVersion", COMPILE_SDK))
		.map(Attribute::text)
		.or_else(|| {
			first("manifest").and_then(|e| {
				e.attributes
					.iter()
					.find(|a| a.name == "platformBuildVersionCode")
					.map(Attribute::text)
			})
		});
	let application = first("application");
	let app_bool = |name: &str, id: u32| {
		application
			.and_then(|e| find(e, name, id))
			.and_then(Attribute::as_bool)
	};

	let mut features = Vec::new();
	let mut gl_es = None;
	let mut permissions = Vec::new();
	let mut supported_devices = Vec::new();
	let mut components = vec![
		("activity", 0),
		("service", 0),
		("receiver", 0),
		("provider", 0),
	];
	for element in &elements {
		match element.name.as_str() {
			"uses-feature" => {
				if let Some(version) =
					find(element, "glEsVersion", GL_ES).and_then(Attribute::as_int)
				{
					gl_es = Some(format!("{}.{}", version >> 16, version & 0xffff));
				}
				if let Some(name) = find(element, "name", NAME).map(Attribute::text) {
					let required = find(element, "required", REQUIRED)
						.and_then(Attribute::as_bool)
						.unwrap_or(true);
					features.push(Feature { name, required });
				}
			}
			"uses-permission" | "uses-permission-sdk-23" => {
				if let Some(name) = find(element, "name", NAME).map(Attribute::text) {
					permissions.push(name);
				}
			}
			"meta-data" => {
				let name = find(element, "name", NAME).map(Attribute::text);
				if name.as_deref() == Some("com.oculus.supportedDevices")
					&& let Some(value) = find(element, "value", VALUE).map(Attribute::text)
				{
					supported_devices = value.split('|').map(|d| d.trim().to_owned()).collect();
				}
			}
			"activity" | "activity-alias" | "service" | "receiver" | "provider" => {
				let kind = if element.name == "activity-alias" {
					"activity"
				} else {
					element.name.as_str()
				};
				if let Some(count) = components.iter_mut().find(|(k, _)| *k == kind) {
					count.1 += 1;
				}
			}
			_ => {}
		}
	}
	permissions.sort();

	let (locales, firebase) = match read("resources.arsc")? {
		Some(data) => {
			let table = ResourceTable::parse(&data).map_err(|e| apk_err(e.to_string()))?;
			let mut locales: Vec<String> = table
				.packages
				.iter()
				.flat_map(|p| p.locales.iter().cloned())
				.collect();
			locales.sort();
			locales.dedup();
			(locales, table.has_key("google_app_id"))
		}
		None => (Vec::new(), false),
	};
	drop(read);

	let names: Vec<String> = zip
		.file_names()
		.filter_map(|n| n.ok().map(|n| n.into_owned()))
		.collect();
	let mut abis: Vec<String> = names
		.iter()
		.filter_map(|n| {
			n.strip_prefix("lib/")?
				.split_once('/')
				.map(|(abi, _)| abi.to_owned())
		})
		.collect();
	abis.sort();
	abis.dedup();
	let dex_files = names
		.iter()
		.filter(|n| !n.contains('/') && n.starts_with("classes") && n.ends_with(".dex"))
		.count();
	let native_libraries = names
		.iter()
		.filter(|n| n.starts_with("lib/") && n.ends_with(".so"))
		.count();
	let entries = names.len();
	let platform_loader = names.iter().any(|n| n == PLATFORM_LOADER);
	drop(zip);

	let (sha256, md5) = hash_file(&apk).map_err(crate::sideload::io_err(&apk))?;
	let signature = signature(&apk, &names, keys);

	Ok(Inspection {
		size: meta.size,
		package: meta.package,
		label: meta.label,
		version_code: meta.version_code,
		version_name: meta.version_name,
		min_sdk: sdk(MIN_SDK, "minSdkVersion"),
		target_sdk: sdk(TARGET_SDK, "targetSdkVersion"),
		compile_sdk,
		gl_es,
		debuggable: app_bool("debuggable", DEBUGGABLE).unwrap_or(false),
		extract_native_libs: app_bool("extractNativeLibs", EXTRACT_NATIVE_LIBS),
		supported_devices,
		features,
		permissions,
		components,
		abis,
		locales,
		dex_files,
		native_libraries,
		entries,
		platform_loader,
		firebase,
		sha256,
		md5,
		signature,
		apk,
	})
}

fn hash_file(path: &Path) -> std::io::Result<(String, String)> {
	let mut file = std::fs::File::open(path)?;
	let mut sha = Sha256::new();
	let mut md5 = Md5::new();
	let mut buf = vec![0u8; 1 << 20];
	loop {
		let n = file.read(&mut buf)?;
		if n == 0 {
			break;
		}
		sha.update(&buf[..n]);
		md5.update(&buf[..n]);
	}
	Ok((hex(&sha.finalize()), hex(&md5.finalize())))
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn signature(apk: &Path, names: &[String], keys: &KeyStore) -> Signature {
	let mut schemes = Vec::new();
	let v1_file = names.iter().find(|n| {
		let upper = n.to_ascii_uppercase();
		upper.starts_with("META-INF/")
			&& !upper["META-INF/".len()..].contains('/')
			&& [".RSA", ".DSA", ".EC"]
				.iter()
				.any(|ext| upper.ends_with(ext))
	});
	if v1_file.is_some() {
		schemes.push("v1");
	}
	let ids = signing_block_ids(apk).unwrap_or_default();
	for (id, name) in [
		(V2_BLOCK_ID, "v2"),
		(V3_BLOCK_ID, "v3"),
		(V31_BLOCK_ID, "v3.1"),
	] {
		if ids.contains(&id) {
			schemes.push(name);
		}
	}
	let mut certificates = Vec::new();
	let v2_valid = ids.contains(&V2_BLOCK_ID).then(|| match verify_v2(apk) {
		Ok(cert) => {
			certificates.push(cert);
			Ok(())
		}
		Err(e) => Err(e),
	});
	if certificates.is_empty()
		&& let Some(name) = v1_file
		&& let Some(cert) = v1_certificate(apk, name)
	{
		certificates.push(cert);
	}
	let certificates: Vec<Certificate> = certificates
		.iter()
		.map(|cert| Certificate {
			subject: subject(cert).unwrap_or_default(),
			sha256: hex(&Sha256::digest(cert)),
		})
		.collect();
	let ours = certificates.first().and_then(|cert| {
		keys.packages().into_iter().find(|package| {
			keys.load(package)
				.ok()
				.flatten()
				.is_some_and(|k| k.fingerprint() == cert.sha256)
		})
	});
	Signature {
		schemes,
		v2_valid,
		certificates,
		ours,
	}
}

/// ContentInfo -> [0] SignedData -> its `[0]` certificates -> the first one.
fn v1_certificate(apk: &Path, name: &str) -> Option<Vec<u8>> {
	let file = std::fs::File::open(apk).ok()?;
	let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
	let mut data = Vec::new();
	zip.by_name(name).ok()?.read_to_end(&mut data).ok()?;
	let content_info = der::read(&data).ok()?;
	let outer = der::children(content_info.content).ok()?;
	let signed_data = der::read(outer.get(1)?.content).ok()?;
	let fields = der::children(signed_data.content).ok()?;
	let certs = fields.iter().find(|f| f.raw.first() == Some(&0xa0))?;
	let first = der::children(certs.content).ok()?.into_iter().next()?;
	Some(first.raw.to_vec())
}

/// `CN=..., O=...` from the certificate's subject.
fn subject(cert: &[u8]) -> Option<String> {
	let cert = der::read(cert).ok()?;
	let tbs = der::children(der::children(cert.content).ok()?.first()?.content).ok()?;
	// With the explicit version first, the subject is the sixth field.
	let subject = tbs.get(5)?;
	let wanted = [(der::COMMON_NAME, "CN"), (&[2, 5, 4, 10][..], "O")];
	let mut parts = Vec::new();
	for rdn in der::children(subject.content).ok()? {
		for pair in der::children(rdn.content).ok()? {
			let fields = der::children(pair.content).ok()?;
			let (oid, value) = (fields.first()?, fields.get(1)?);
			if let Some((_, label)) = wanted.iter().find(|(arcs, _)| der::oid(arcs) == oid.raw) {
				parts.push(format!(
					"{label}={}",
					String::from_utf8_lossy(value.content)
				));
			}
		}
	}
	Some(parts.join(", "))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rename::keys::tests::test_key;

	#[test]
	fn reads_the_subject_of_our_certificates() {
		assert_eq!(
			subject(test_key().certificate()).as_deref(),
			Some("CN=com.example.copy")
		);
	}

	#[test]
	fn inspects_a_signed_apk() {
		use std::io::Write;
		let dir = tempfile::tempdir().unwrap();
		let manifest = crate::rename::axml::tests::manifest(&[
			("manifest", 0, &[(false, "package", "com.example.copy")]),
			(
				"uses-sdk",
				1,
				&[
					(true, "minSdkVersion", "29"),
					(true, "targetSdkVersion", "32"),
				],
			),
			(
				"uses-permission",
				1,
				&[(true, "name", "android.permission.INTERNET")],
			),
			(
				"uses-feature",
				1,
				&[(true, "name", "android.hardware.vr.headtracking")],
			),
			("application", 1, &[]),
			(
				"activity",
				2,
				&[(true, "name", "com.unity3d.player.UnityPlayerActivity")],
			),
			(
				"meta-data",
				2,
				&[
					(true, "name", "com.oculus.supportedDevices"),
					(true, "value", "quest2|quest3"),
				],
			),
		]);
		let source = dir.path().join("source.apk");
		let mut zip = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
		let options = zip::write::SimpleFileOptions::default();
		zip.start_file("AndroidManifest.xml", options).unwrap();
		zip.write_all(&manifest).unwrap();
		zip.start_file("classes.dex", options).unwrap();
		zip.write_all(b"dex").unwrap();
		zip.start_file("lib/arm64-v8a/libgame.so", options).unwrap();
		zip.write_all(b"so").unwrap();
		zip.finish().unwrap();
		let apk = dir.path().join("game.apk");
		crate::rename::sign::write_signed_apk(
			&source,
			&apk,
			&std::collections::HashMap::new(),
			test_key(),
			&|_, _| {},
		)
		.unwrap();
		let keys = KeyStore::new(dir.path().join("signing"));
		let pem = dir.path().join("key.pem");
		std::fs::write(&pem, test_key().to_pem("com.example.copy").unwrap()).unwrap();
		keys.import(&pem).unwrap();

		let info = inspect(&apk, &keys).unwrap();
		assert_eq!(info.package, "com.example.copy");
		assert_eq!(info.min_sdk.as_deref(), Some("29"));
		assert_eq!(info.target_sdk.as_deref(), Some("32"));
		assert_eq!(info.permissions, ["android.permission.INTERNET"]);
		assert_eq!(info.features[0].name, "android.hardware.vr.headtracking");
		assert!(info.features[0].required);
		assert_eq!(info.supported_devices, ["quest2", "quest3"]);
		assert_eq!(info.components[0], ("activity", 1));
		assert_eq!(info.abis, ["arm64-v8a"]);
		assert_eq!((info.dex_files, info.native_libraries), (1, 1));
		assert_eq!(info.signature.schemes, ["v1", "v2"]);
		assert_eq!(info.signature.v2_valid, Some(Ok(())));
		assert_eq!(
			info.signature.certificates[0].subject,
			"CN=com.example.copy"
		);
		assert_eq!(info.signature.ours.as_deref(), Some("com.example.copy"));
		assert_eq!(info.sha256.len(), 64);
	}
}
