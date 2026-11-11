use std::path::PathBuf;

use base64::Engine as _;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::CatalogError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
	pub name: String,
	pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MirrorKind {
	/// Plain HTTP server laid out like the VRP public mirror.
	Http { base_uri: String },
	/// An rclone remote with a `Quest Games` folder.
	Rclone {
		config_file: PathBuf,
		remote: String,
	},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mirror {
	pub id: String,
	pub name: String,
	#[serde(flatten)]
	pub kind: MirrorKind,
	/// Archive password, base64-encoded as in VRP configs.
	pub password: String,
	/// Sent with every request, e.g. an `X-API-Key` the mirror requires.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub headers: Vec<Header>,
}

impl Mirror {
	pub fn new_id() -> String {
		let nanos = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map_or(0, |d| d.as_nanos());
		format!("m{nanos:x}")
	}

	pub fn archive_password(&self) -> Result<String, CatalogError> {
		let bytes = base64::engine::general_purpose::STANDARD
			.decode(self.password.trim())
			.map_err(|_| CatalogError::Config("the password is not valid base64".into()))?;
		String::from_utf8(bytes)
			.map_err(|_| CatalogError::Config("the decoded password is not text".into()))
	}

	/// Parses a VRP-style config: `{"baseUri": "...", "password": "..."}`.
	pub fn from_vrp_json(name: &str, text: &str) -> Result<Self, CatalogError> {
		#[derive(Deserialize)]
		#[serde(rename_all = "camelCase")]
		struct VrpConfig {
			base_uri: String,
			password: String,
		}
		let config: VrpConfig = serde_json::from_str(text)
			.map_err(|e| CatalogError::Config(format!("invalid server config: {e}")))?;
		let mirror = Self {
			id: Self::new_id(),
			name: name.to_owned(),
			kind: MirrorKind::Http {
				base_uri: config.base_uri,
			},
			password: config.password,
			headers: Vec::new(),
		};
		mirror.validate()?;
		Ok(mirror)
	}

	pub fn validate(&self) -> Result<(), CatalogError> {
		if self.name.trim().is_empty() {
			return Err(CatalogError::Config("the mirror needs a name".into()));
		}
		self.archive_password()?;
		match &self.kind {
			MirrorKind::Http { base_uri } => {
				let url = reqwest::Url::parse(base_uri)
					.map_err(|e| CatalogError::Config(format!("invalid base URI: {e}")))?;
				if !matches!(url.scheme(), "http" | "https") {
					return Err(CatalogError::Config(
						"base URI must start with http:// or https://".into(),
					));
				}
			}
			MirrorKind::Rclone { remote, .. } => {
				if remote.trim().is_empty() {
					return Err(CatalogError::Config("choose an rclone remote".into()));
				}
			}
		}
		for header in &self.headers {
			reqwest::header::HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| {
				CatalogError::Config(format!("invalid header name {:?}", header.name))
			})?;
			reqwest::header::HeaderValue::from_str(&header.value).map_err(|_| {
				CatalogError::Config(format!("invalid value for header {}", header.name))
			})?;
		}
		Ok(())
	}
}

/// Remote names (`[section]` headers) defined in an rclone config.
pub fn rclone_remotes(config: &str) -> Vec<String> {
	config
		.lines()
		.filter_map(|line| {
			let name = line.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
			(!name.is_empty()).then(|| name.to_owned())
		})
		.collect()
}

/// Folder name of a release on HTTP mirrors: md5 of the release name plus a newline.
pub fn release_hash(release_name: &str) -> String {
	let digest = Md5::digest(format!("{release_name}\n").as_bytes());
	digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn vrp_config() {
		let mirror = Mirror::from_vrp_json(
			"Main",
			r#"{"baseUri":"https://example.org/files/","password":"aHVudGVyMg=="}"#,
		)
		.unwrap();
		assert_eq!(mirror.archive_password().unwrap(), "hunter2");
		assert!(
			matches!(&mirror.kind, MirrorKind::Http { base_uri, .. } if base_uri == "https://example.org/files/")
		);
	}

	#[test]
	fn invalid_configs() {
		for (json, why) in [
			(r#"{"baseUri":"ftp://x/","password":"aGk="}"#, "scheme"),
			(
				r#"{"baseUri":"https://x/","password":"not base64!"}"#,
				"password",
			),
			(r#"{"baseUri":"https://x/"}"#, "missing password"),
			("nonsense", "not json"),
		] {
			assert!(Mirror::from_vrp_json("m", json).is_err(), "{why}");
		}
	}

	#[test]
	fn settings_roundtrip_keeps_kind() {
		let mirror = Mirror {
			id: "m1".into(),
			name: "Mine".into(),
			kind: MirrorKind::Rclone {
				config_file: "/cfg/r.conf".into(),
				remote: "VRP".into(),
			},
			password: "aGk=".into(),
			headers: vec![Header {
				name: "X-Api-Key".into(),
				value: "k".into(),
			}],
		};
		#[derive(Serialize, Deserialize)]
		struct Wrap {
			mirrors: Vec<Mirror>,
		}
		let text = toml::to_string(&Wrap {
			mirrors: vec![mirror.clone()],
		})
		.unwrap();
		assert!(text.contains("type = \"rclone\""), "{text}");
		let back: Wrap = toml::from_str(&text).unwrap();
		assert_eq!(back.mirrors, [mirror]);
	}

	/// Settings written before headers moved out of the HTTP variant.
	#[test]
	fn reads_older_http_mirror_with_headers() {
		let text = r#"
			[[mirrors]]
			id = "m1"
			name = "Old"
			type = "http"
			base_uri = "https://e.org/"
			password = "aGk="

			[[mirrors.headers]]
			name = "X-Api-Key"
			value = "secret"
		"#;
		#[derive(Deserialize)]
		struct Wrap {
			mirrors: Vec<Mirror>,
		}
		let wrap: Wrap = toml::from_str(text).unwrap();
		assert_eq!(wrap.mirrors[0].headers[0].value, "secret");
		assert!(
			matches!(&wrap.mirrors[0].kind, MirrorKind::Http { base_uri } if base_uri == "https://e.org/")
		);
	}

	#[test]
	fn invalid_header_is_rejected_for_any_kind() {
		let mirror = Mirror {
			id: "m".into(),
			name: "r".into(),
			kind: MirrorKind::Rclone {
				config_file: "/c".into(),
				remote: "vrp".into(),
			},
			password: "aGk=".into(),
			headers: vec![Header {
				name: "Bad Name".into(),
				value: "v".into(),
			}],
		};
		assert!(mirror.validate().is_err());
	}

	#[test]
	fn remotes_from_rclone_config() {
		let conf = "[VRP-mirror01]\ntype = ftp\nhost = x\n\n[ backup ]\ntype = s3\n[]\n";
		assert_eq!(rclone_remotes(conf), ["VRP-mirror01", "backup"]);
	}

	#[test]
	fn hash_matches_md5_with_newline() {
		// Same as `printf "abc\n" | md5sum`.
		assert_eq!(release_hash("abc"), "0bee89b07a248e27c83fc3d5951213c1");
	}
}
