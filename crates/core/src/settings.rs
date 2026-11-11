use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::Mirror;
use crate::error::{Error, IoContext, Result};
use crate::paths::AppPaths;

pub const MAX_CONCURRENT_DOWNLOADS: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedUnit {
	/// 1024 bytes.
	#[default]
	KiB,
	/// 1000 bytes.
	KB,
}

impl SpeedUnit {
	pub fn bytes(self) -> u64 {
		match self {
			Self::KiB => 1024,
			Self::KB => 1000,
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::KiB => "KiB/s",
			Self::KB => "KB/s",
		}
	}
}

/// Total download speed limit; a value of 0 means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeedLimit {
	pub value: u32,
	pub unit: SpeedUnit,
}

impl SpeedLimit {
	pub fn bytes_per_sec(self) -> u64 {
		u64::from(self.value) * self.unit.bytes()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
	#[default]
	System,
	Dark,
	Light,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
	pub downloads_dir: PathBuf,
	pub max_concurrent_downloads: u8,
	pub download_limit: SpeedLimit,
	pub theme: Theme,
	/// Keep unpacked releases after installing them, for reinstalling later.
	pub keep_downloads: bool,
	/// `None` means `adb` from `PATH`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub adb_path: Option<PathBuf>,
	/// `None` means `rclone` from `PATH`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rclone_path: Option<PathBuf>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub active_mirror: Option<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub mirrors: Vec<Mirror>,
	/// Packages the user doesn't want to be asked to upload again.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub contribute_ignored: Vec<String>,
}

impl Settings {
	pub fn defaults_for(paths: &AppPaths) -> Self {
		Self {
			downloads_dir: paths.default_downloads_dir(),
			..Self::default()
		}
	}

	/// A missing file yields defaults; a broken one is an error so the user's
	/// settings are never silently replaced.
	pub fn load(paths: &AppPaths) -> Result<Self> {
		let path = paths.settings_file();
		let text = match std::fs::read_to_string(&path) {
			Ok(text) => text,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				return Ok(Self::defaults_for(paths));
			}
			Err(e) => return Err(e).at(path),
		};
		let mut settings: Self =
			toml::from_str(&text).map_err(|source| Error::SettingsParse { path, source })?;
		if settings.downloads_dir.as_os_str().is_empty() {
			settings.downloads_dir = paths.default_downloads_dir();
		}
		settings.normalize();
		Ok(settings)
	}

	pub fn save(&self, paths: &AppPaths) -> Result<()> {
		let text = toml::to_string_pretty(self)?;
		write_atomically(&paths.settings_file(), text.as_bytes())
	}

	pub fn normalize(&mut self) {
		self.max_concurrent_downloads = self
			.max_concurrent_downloads
			.clamp(1, MAX_CONCURRENT_DOWNLOADS);
		for path in [&mut self.adb_path, &mut self.rclone_path] {
			if path.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
				*path = None;
			}
		}
		if self
			.active_mirror
			.as_ref()
			.is_some_and(|id| !self.mirrors.iter().any(|m| &m.id == id))
		{
			self.active_mirror = None;
		}
		if self.active_mirror.is_none() {
			self.active_mirror = self.mirrors.first().map(|m| m.id.clone());
		}
	}

	pub fn active_mirror(&self) -> Option<&Mirror> {
		let id = self.active_mirror.as_ref()?;
		self.mirrors.iter().find(|m| &m.id == id)
	}
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			downloads_dir: PathBuf::new(),
			max_concurrent_downloads: 2,
			download_limit: SpeedLimit::default(),
			theme: Theme::default(),
			keep_downloads: false,
			adb_path: None,
			rclone_path: None,
			active_mirror: None,
			mirrors: Vec::new(),
			contribute_ignored: Vec::new(),
		}
	}
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).at(parent)?;
	}
	let tmp = path.with_extension("toml.tmp");
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	// Mirror passwords and API keys live here.
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	std::io::Write::write_all(&mut options.open(&tmp).at(&tmp)?, bytes).at(&tmp)?;
	std::fs::rename(&tmp, path).at(path)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_file_gives_defaults() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		assert_eq!(
			Settings::load(&paths).unwrap(),
			Settings::defaults_for(&paths)
		);
	}

	#[test]
	fn roundtrip() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		let settings = Settings {
			downloads_dir: dir.path().join("dl"),
			max_concurrent_downloads: 4,
			download_limit: SpeedLimit {
				value: 2500,
				unit: SpeedUnit::KB,
			},
			theme: Theme::Dark,
			keep_downloads: true,
			adb_path: Some("/opt/platform-tools/adb".into()),
			rclone_path: None,
			active_mirror: Some("m1".into()),
			mirrors: vec![Mirror {
				id: "m1".into(),
				name: "Main".into(),
				kind: crate::catalog::MirrorKind::Http {
					base_uri: "https://example.org/".into(),
				},
				password: "aGk=".into(),
				headers: vec![crate::catalog::Header {
					name: "X-Api-Key".into(),
					value: "k".into(),
				}],
			}],
			contribute_ignored: vec!["com.discord".into()],
		};
		settings.save(&paths).unwrap();
		assert_eq!(Settings::load(&paths).unwrap(), settings);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let mode = std::fs::metadata(paths.settings_file())
				.unwrap()
				.permissions()
				.mode();
			assert_eq!(mode & 0o777, 0o600);
		}
	}

	#[test]
	fn active_mirror_falls_back_to_first() {
		let mut settings = Settings {
			active_mirror: Some("gone".into()),
			..Settings::default()
		};
		settings.normalize();
		assert_eq!(settings.active_mirror, None);
		settings.mirrors.push(Mirror {
			id: "a".into(),
			name: "A".into(),
			kind: crate::catalog::MirrorKind::Http {
				base_uri: "https://a/".into(),
			},
			password: "aGk=".into(),
			headers: Vec::new(),
		});
		settings.normalize();
		assert_eq!(settings.active_mirror().map(|m| m.name.as_str()), Some("A"));
	}

	#[test]
	fn partial_file_fills_defaults_and_clamps() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		std::fs::create_dir_all(paths.settings_file().parent().unwrap()).unwrap();
		std::fs::write(paths.settings_file(), "max_concurrent_downloads = 50\n").unwrap();
		let loaded = Settings::load(&paths).unwrap();
		assert_eq!(loaded.max_concurrent_downloads, MAX_CONCURRENT_DOWNLOADS);
		assert_eq!(loaded.downloads_dir, paths.default_downloads_dir());
		assert_eq!(loaded.theme, Theme::System);
	}

	#[test]
	fn broken_file_is_an_error() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		std::fs::create_dir_all(paths.settings_file().parent().unwrap()).unwrap();
		std::fs::write(paths.settings_file(), "theme = [").unwrap();
		assert!(matches!(
			Settings::load(&paths),
			Err(Error::SettingsParse { .. })
		));
	}

	#[test]
	fn speed_limit_units() {
		let kib = SpeedLimit {
			value: 2,
			unit: SpeedUnit::KiB,
		};
		let kb = SpeedLimit {
			value: 2,
			unit: SpeedUnit::KB,
		};
		assert_eq!(kib.bytes_per_sec(), 2048);
		assert_eq!(kb.bytes_per_sec(), 2000);
		assert_eq!(SpeedLimit::default().bytes_per_sec(), 0);
		let text = toml::to_string(&Settings {
			download_limit: kb,
			..Settings::default()
		})
		.unwrap();
		assert!(text.contains("unit = \"kb\""), "{text}");
	}
}
