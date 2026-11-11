use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{Error, IoContext, Result};

#[derive(Debug, Clone)]
pub struct AppPaths {
	config_dir: PathBuf,
	data_dir: PathBuf,
	cache_dir: PathBuf,
}

impl AppPaths {
	pub fn from_platform() -> Result<Self> {
		let dirs = ProjectDirs::from("", "", "quest-sideloader").ok_or(Error::NoPlatformDirs)?;
		Ok(Self {
			config_dir: dirs.config_dir().to_path_buf(),
			data_dir: dirs.data_dir().to_path_buf(),
			cache_dir: dirs.cache_dir().to_path_buf(),
		})
	}

	/// Puts everything under one root; used by tests and portable mode.
	pub fn under(root: impl AsRef<Path>) -> Self {
		let root = root.as_ref();
		Self {
			config_dir: root.join("config"),
			data_dir: root.join("data"),
			cache_dir: root.join("cache"),
		}
	}

	pub fn ensure_exist(&self) -> Result<()> {
		for dir in [&self.config_dir, &self.cache_dir, &self.logs_dir()] {
			std::fs::create_dir_all(dir).at(dir)?;
		}
		Ok(())
	}

	pub fn settings_file(&self) -> PathBuf {
		self.config_dir.join("settings.toml")
	}

	pub fn logs_dir(&self) -> PathBuf {
		self.data_dir.join("logs")
	}

	pub fn default_downloads_dir(&self) -> PathBuf {
		self.data_dir.join("downloads")
	}

	pub fn catalog_dir(&self) -> PathBuf {
		self.data_dir.join("catalog")
	}

	/// Copies of imported rclone configs, one per mirror.
	pub fn rclone_dir(&self) -> PathBuf {
		self.config_dir.join("rclone")
	}

	pub fn cache_dir(&self) -> &Path {
		&self.cache_dir
	}
}
