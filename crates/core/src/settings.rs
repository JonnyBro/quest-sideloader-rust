use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::paths::AppPaths;

pub const MAX_CONCURRENT_DOWNLOADS: u8 = 5;

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
	pub theme: Theme,
	/// `None` means `adb` from `PATH`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub adb_path: Option<PathBuf>,
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
		if self
			.adb_path
			.as_ref()
			.is_some_and(|p| p.as_os_str().is_empty())
		{
			self.adb_path = None;
		}
	}
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			downloads_dir: PathBuf::new(),
			max_concurrent_downloads: 2,
			theme: Theme::default(),
			adb_path: None,
		}
	}
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).at(parent)?;
	}
	let tmp = path.with_extension("toml.tmp");
	std::fs::write(&tmp, bytes).at(&tmp)?;
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
			theme: Theme::Dark,
			adb_path: Some("/opt/platform-tools/adb".into()),
		};
		settings.save(&paths).unwrap();
		assert_eq!(Settings::load(&paths).unwrap(), settings);
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
}
