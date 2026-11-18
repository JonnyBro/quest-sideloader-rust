use rust_i18n::t;
use std::path::PathBuf;

use base64::Engine as _;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::CatalogError;

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
			.map_err(|_| CatalogError::Config(t!("mir.bad_base64").into_owned()))?;
		String::from_utf8(bytes).map_err(|_| CatalogError::Config(t!("mir.not_text").into_owned()))
	}

	/// Parses a VRP-style config: `{"baseUri": "...", "password": "..."}`.
	pub fn from_vrp_json(name: &str, text: &str) -> Result<Self, CatalogError> {
		#[derive(Deserialize)]
		#[serde(rename_all = "camelCase")]
		struct VrpConfig {
			base_uri: String,
			password: String,
		}
		let config: VrpConfig = serde_json::from_str(text).map_err(|e| {
			CatalogError::Config(t!("mir.bad_server_config", error = e).into_owned())
		})?;
		let mirror = Self {
			id: Self::new_id(),
			name: name.to_owned(),
			kind: MirrorKind::Http {
				base_uri: config.base_uri,
			},
			password: config.password,
		};
		mirror.validate()?;
		Ok(mirror)
	}

	pub fn validate(&self) -> Result<(), CatalogError> {
		if self.name.trim().is_empty() {
			return Err(CatalogError::Config(t!("mir.needs_name").into_owned()));
		}
		self.archive_password()?;
		match &self.kind {
			MirrorKind::Http { base_uri } => {
				let url = reqwest::Url::parse(base_uri).map_err(|e| {
					CatalogError::Config(t!("mir.bad_base_uri", error = e).into_owned())
				})?;
				if !matches!(url.scheme(), "http" | "https") {
					return Err(CatalogError::Config(t!("mir.base_uri_scheme").into_owned()));
				}
			}
			MirrorKind::Rclone { remote, .. } => {
				if remote.trim().is_empty() {
					return Err(CatalogError::Config(t!("mir.choose_remote").into_owned()));
				}
			}
		}
		Ok(())
	}
}

/// Remote names (`[section]` headers) defined in an rclone config.
pub fn rclone_remotes(config: &str) -> Vec<String> {
	config
		.lines()
		.filter_map(|line| {
			let line = line.trim_start_matches('\u{feff}').trim();
			let name = line.strip_prefix('[')?.strip_suffix(']')?.trim();
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

	/// Older settings could carry custom headers per mirror; they are ignored now.
	#[test]
	fn reads_older_mirror_with_headers() {
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
		assert!(
			matches!(&wrap.mirrors[0].kind, MirrorKind::Http { base_uri } if base_uri == "https://e.org/")
		);
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
