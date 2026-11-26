//! Signing keys for renamed copies, one per copy's package. Android only
//! installs an update over an app signed with the same key, so losing the
//! key means the copy can't be updated, only removed and installed again.

use base64::Engine as _;
use rust_i18n::t;
use std::path::{Path, PathBuf};

use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::{Generate, PrivateKey};
use sha2::{Digest, Sha256};

use super::der;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
	#[error("{}", t!("err.key_bad", error = .0))]
	Bad(String),

	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
}

pub struct SigningKey {
	key: PrivateKey,
	certificate: Vec<u8>,
	public_key: Vec<u8>,
}

const CERT_BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const HEADER: &str = "Quest Sideloader signing key for ";
const CERT_END: &str = "-----END CERTIFICATE-----";

impl SigningKey {
	/// Takes a second or two.
	pub fn generate(package: &str) -> Result<Self, KeyError> {
		Self::with_key(PrivateKey::generate(), package)
	}

	fn with_key(key: PrivateKey, package: &str) -> Result<Self, KeyError> {
		let public_key = key
			.to_public_key()
			.to_public_key_der()
			.map_err(|e| KeyError::Bad(e.to_string()))?
			.into_vec();
		// Derived from the key, so it is unique per key without a random source.
		let mut serial = Sha256::digest(&public_key)[..16].to_vec();
		serial[0] &= 0x7f;
		let name = der::seq(&[&der::set(&[&der::seq(&[
			&der::oid(der::COMMON_NAME),
			&der::tlv(der::UTF8_STRING, package.as_bytes()),
		])])]);
		let validity = der::seq(&[
			&not_before(),
			&der::tlv(der::GENERALIZED_TIME, b"99991231235959Z"),
		]);
		let tbs = der::seq(&[
			&der::explicit(0, &der::uint(&[2])),
			&der::uint(&serial),
			&der::algorithm(der::SHA256_WITH_RSA),
			&name,
			&validity,
			&name,
			&public_key,
		]);
		let signature = sign_with(&key, &tbs)?;
		let certificate = der::seq(&[
			&tbs,
			&der::algorithm(der::SHA256_WITH_RSA),
			&der::tlv(der::BIT_STRING, &[&[0u8][..], &signature].concat()),
		]);
		Ok(Self {
			key,
			certificate,
			public_key,
		})
	}

	pub fn certificate(&self) -> &[u8] {
		&self.certificate
	}

	/// `SubjectPublicKeyInfo`, as the v2 signature block wants it.
	pub fn public_key(&self) -> &[u8] {
		&self.public_key
	}

	/// RSA PKCS#1 v1.5 over SHA-256 of `data`.
	pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>, KeyError> {
		sign_with(&self.key, data)
	}

	/// The certificate's issuer and serial number, which a PKCS#7 signer
	/// names its certificate by.
	pub fn issuer_and_serial(&self) -> Result<(Vec<u8>, Vec<u8>), KeyError> {
		let bad = |e: super::chunk::FormatError| KeyError::Bad(e.to_string());
		let cert = der::read(&self.certificate).map_err(bad)?;
		let parts = der::children(cert.content).map_err(bad)?;
		let tbs = der::children(
			parts
				.first()
				.ok_or_else(|| KeyError::Bad("empty certificate".into()))?
				.content,
		)
		.map_err(bad)?;
		match (tbs.get(1), tbs.get(3)) {
			(Some(serial), Some(issuer)) => Ok((issuer.raw.to_vec(), serial.raw.to_vec())),
			_ => Err(KeyError::Bad("short certificate".into())),
		}
	}

	/// SHA-256 of the certificate, as `apksigner` and Android show it.
	pub fn fingerprint(&self) -> String {
		Sha256::digest(&self.certificate)
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect()
	}

	pub fn to_pem(&self, package: &str) -> Result<String, KeyError> {
		let key = self
			.key
			.to_pkcs8_pem(LineEnding::LF)
			.map_err(|e| KeyError::Bad(e.to_string()))?;
		let cert = base64::engine::general_purpose::STANDARD.encode(&self.certificate);
		let mut out = format!("{HEADER}{package}\n{}", key.as_str());
		out.push_str(CERT_BEGIN);
		out.push('\n');
		for line in cert.as_bytes().chunks(64) {
			out.push_str(&String::from_utf8_lossy(line));
			out.push('\n');
		}
		out.push_str(CERT_END);
		out.push('\n');
		Ok(out)
	}

	pub fn from_pem(text: &str) -> Result<Self, KeyError> {
		let bad = |message: &str| KeyError::Bad(message.to_owned());
		let key_start = text
			.find("-----BEGIN PRIVATE KEY-----")
			.ok_or_else(|| bad("no private key"))?;
		let key_end = text
			.find("-----END PRIVATE KEY-----")
			.ok_or_else(|| bad("no private key"))?
			+ "-----END PRIVATE KEY-----".len();
		let key = PrivateKey::from_pkcs8_pem(text.get(key_start..key_end).unwrap_or_default())
			.map_err(|e| KeyError::Bad(e.to_string()))?;
		let cert_start =
			text.find(CERT_BEGIN).ok_or_else(|| bad("no certificate"))? + CERT_BEGIN.len();
		let cert_end = text.find(CERT_END).ok_or_else(|| bad("no certificate"))?;
		let body: String = text
			.get(cert_start..cert_end)
			.unwrap_or_default()
			.split_whitespace()
			.collect();
		let certificate = base64::engine::general_purpose::STANDARD
			.decode(body)
			.map_err(|e| KeyError::Bad(e.to_string()))?;
		let public_key = key
			.to_public_key()
			.to_public_key_der()
			.map_err(|e| KeyError::Bad(e.to_string()))?
			.into_vec();
		// A certificate for another key would make every signature invalid.
		let cert = der::read(&certificate).map_err(|e| KeyError::Bad(e.to_string()))?;
		if !cert
			.content
			.windows(public_key.len())
			.any(|w| w == public_key)
		{
			return Err(bad("the certificate is for another key"));
		}
		Ok(Self {
			key,
			certificate,
			public_key,
		})
	}
}

fn sign_with(key: &PrivateKey, data: &[u8]) -> Result<Vec<u8>, KeyError> {
	key.pkcs1v15_sign::<Sha256>(data)
		.map_err(|e| KeyError::Bad(e.to_string()))
}

/// Today, as UTCTime. Android doesn't check the validity of APK certificates,
/// but other tools show it.
fn not_before() -> Vec<u8> {
	let secs = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_secs());
	let (year, month, day) = civil_date(secs / 86_400);
	if (1950..2050).contains(&year) {
		let text = format!("{:02}{month:02}{day:02}000000Z", year % 100);
		der::tlv(der::UTC_TIME, text.as_bytes())
	} else {
		let text = format!("{year:04}{month:02}{day:02}000000Z");
		der::tlv(der::GENERALIZED_TIME, text.as_bytes())
	}
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_date(days: u64) -> (i64, u32, u32) {
	let z = days as i64 + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
	let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
	(yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Where copies' keys live: one PEM file per package.
pub struct KeyStore {
	dir: PathBuf,
}

impl KeyStore {
	pub fn new(dir: PathBuf) -> Self {
		Self { dir }
	}

	fn path(&self, package: &str) -> PathBuf {
		self.dir.join(format!("{package}.pem"))
	}

	pub fn has(&self, package: &str) -> bool {
		self.path(package).is_file()
	}

	/// Packages that have a key, so their apps are known to be our copies.
	pub fn packages(&self) -> Vec<String> {
		let Ok(entries) = std::fs::read_dir(&self.dir) else {
			return Vec::new();
		};
		let mut packages: Vec<String> = entries
			.filter_map(|e| e.ok())
			.filter_map(|e| {
				e.file_name()
					.to_str()?
					.strip_suffix(".pem")
					.map(str::to_owned)
			})
			.filter(|p| crate::sideload::is_package_name(p))
			.collect();
		packages.sort();
		packages
	}

	pub fn load(&self, package: &str) -> Result<Option<SigningKey>, KeyError> {
		let path = self.path(package);
		match std::fs::read_to_string(&path) {
			Ok(text) => SigningKey::from_pem(&text).map(Some),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
			Err(source) => Err(KeyError::Io { path, source }),
		}
	}

	pub fn load_or_create(&self, package: &str) -> Result<SigningKey, KeyError> {
		if let Some(key) = self.load(package)? {
			return Ok(key);
		}
		let key = SigningKey::generate(package)?;
		self.save(package, &key)?;
		Ok(key)
	}

	fn save(&self, package: &str, key: &SigningKey) -> Result<(), KeyError> {
		let io = |path: &Path| {
			let path = path.to_path_buf();
			move |source| KeyError::Io { path, source }
		};
		std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;
		let path = self.path(package);
		let tmp = path.with_extension("pem.tmp");
		crate::settings::write_private(&tmp, key.to_pem(package)?.as_bytes()).map_err(io(&tmp))?;
		std::fs::rename(&tmp, &path).map_err(io(&path))
	}

	pub fn export(&self, package: &str, dest: &Path) -> Result<(), KeyError> {
		let from = self.path(package);
		std::fs::copy(&from, dest)
			.map(|_| ())
			.map_err(|source| KeyError::Io { path: from, source })
	}

	pub fn remove(&self, package: &str) -> Result<(), KeyError> {
		let path = self.path(package);
		std::fs::remove_file(&path).map_err(|source| KeyError::Io { path, source })
	}

	/// The package comes from the key's first line; a different key already
	/// stored is never replaced. Returns the package.
	pub fn import(&self, file: &Path) -> Result<String, KeyError> {
		let text = std::fs::read_to_string(file).map_err(|source| KeyError::Io {
			path: file.to_path_buf(),
			source,
		})?;
		let package = text
			.lines()
			.next()
			.and_then(|l| l.trim().strip_prefix(HEADER))
			.filter(|p| crate::sideload::is_package_name(p))
			.ok_or_else(|| KeyError::Bad(t!("err.key_not_ours").into_owned()))?
			.to_owned();
		let key = SigningKey::from_pem(&text)?;
		if let Some(existing) = self.load(&package)?
			&& existing.certificate != key.certificate
		{
			return Err(KeyError::Bad(
				t!("err.key_exists", package = package).into_owned(),
			));
		}
		self.save(&package, &key)?;
		Ok(package)
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use std::sync::OnceLock;

	/// Keys are slow to make, so tests share one.
	pub fn test_key() -> &'static SigningKey {
		static KEY: OnceLock<SigningKey> = OnceLock::new();
		KEY.get_or_init(|| SigningKey::generate("com.example.copy").unwrap())
	}

	#[test]
	fn pem_round_trips_and_signatures_verify() {
		let key = test_key();
		let pem = key.to_pem("com.example.copy").unwrap();
		let again = SigningKey::from_pem(&pem).unwrap();
		assert_eq!(again.certificate(), key.certificate());
		assert_eq!(again.public_key(), key.public_key());
		let signature = again.sign(b"data").unwrap();
		key.key
			.to_public_key()
			.pkcs1v15_verify::<Sha256>(b"data", &signature)
			.unwrap();
		let (issuer, serial) = key.issuer_and_serial().unwrap();
		assert_eq!(issuer[0], der::SEQUENCE);
		assert_eq!(serial[0], der::INTEGER);
	}

	#[test]
	fn store_keeps_one_key_per_package() {
		let dir = tempfile::tempdir().unwrap();
		let store = KeyStore::new(dir.path().join("signing"));
		let pem = test_key().to_pem("com.a.copy").unwrap();
		let file = dir.path().join("exported.pem");
		std::fs::write(&file, pem).unwrap();
		assert_eq!(store.import(&file).unwrap(), "com.a.copy");
		assert!(store.has("com.a.copy"));
		assert_eq!(store.packages(), ["com.a.copy"]);
		let loaded = store.load_or_create("com.a.copy").unwrap();
		assert_eq!(loaded.certificate(), test_key().certificate());
		store
			.export("com.a.copy", &dir.path().join("out.pem"))
			.unwrap();
		assert!(store.load("com.b.copy").unwrap().is_none());
	}

	#[test]
	fn dates() {
		assert_eq!(civil_date(0), (1970, 1, 1));
		assert_eq!(civil_date(20_773), (2026, 11, 16));
	}
}
