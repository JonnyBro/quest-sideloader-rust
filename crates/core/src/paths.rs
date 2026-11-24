use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::{Error, IoContext, Result};

/// The program to run for a tool the settings leave unset. Apps started from
/// Finder get a PATH without Homebrew's folders, so on macOS those are tried
/// when PATH has no such program.
pub fn default_tool(name: &str) -> PathBuf {
	#[cfg(target_os = "macos")]
	{
		let on_path = std::env::var_os("PATH")
			.is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()));
		if !on_path
			&& let Some(found) = ["/opt/homebrew/bin", "/usr/local/bin"]
				.iter()
				.map(|dir| Path::new(dir).join(name))
				.find(|candidate| candidate.is_file())
		{
			return found;
		}
	}
	PathBuf::from(name)
}

/// A file with this name next to the executable turns on portable mode.
pub const PORTABLE_MARKER: &str = "portable.txt";
/// Where portable mode keeps everything, next to the executable.
pub const PORTABLE_DIR: &str = "userdata";

#[derive(Debug, Clone)]
pub struct AppPaths {
	config_dir: PathBuf,
	data_dir: PathBuf,
	cache_dir: PathBuf,
	portable_root: Option<PathBuf>,
}

impl AppPaths {
	/// Portable mode when [`PORTABLE_MARKER`] sits next to the executable,
	/// the platform's user folders otherwise.
	pub fn detect() -> Result<Self> {
		let exe_dir = std::env::current_exe()
			.ok()
			.and_then(|exe| exe.parent().map(Path::to_path_buf));
		match exe_dir {
			Some(dir) if dir.join(PORTABLE_MARKER).is_file() => {
				Ok(Self::portable(dir.join(PORTABLE_DIR)))
			}
			_ => Self::from_platform(),
		}
	}

	pub fn from_platform() -> Result<Self> {
		let dirs = ProjectDirs::from("", "", "quest-sideloader").ok_or(Error::NoPlatformDirs)?;
		Ok(Self {
			config_dir: dirs.config_dir().to_path_buf(),
			data_dir: dirs.data_dir().to_path_buf(),
			cache_dir: dirs.cache_dir().to_path_buf(),
			portable_root: None,
		})
	}

	/// Puts everything under one root; used by tests.
	pub fn under(root: impl AsRef<Path>) -> Self {
		let root = root.as_ref();
		Self {
			config_dir: root.join("config"),
			data_dir: root.join("data"),
			cache_dir: root.join("cache"),
			portable_root: None,
		}
	}

	/// Everything under `root`, and paths in the settings stored relative to
	/// it so the folder can move (another drive letter, another computer).
	pub fn portable(root: impl AsRef<Path>) -> Self {
		Self {
			portable_root: Some(root.as_ref().to_path_buf()),
			..Self::under(root)
		}
	}

	pub fn portable_root(&self) -> Option<&Path> {
		self.portable_root.as_deref()
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

	/// Window size and view choices kept by the GUI between runs.
	pub fn ui_state_file(&self) -> PathBuf {
		self.config_dir.join("ui.json")
	}

	pub fn logs_dir(&self) -> PathBuf {
		self.data_dir.join("logs")
	}

	pub fn default_downloads_dir(&self) -> PathBuf {
		self.data_dir.join("downloads")
	}

	/// Keys of renamed copies; see [`crate::rename::KeyStore`].
	pub fn signing_dir(&self) -> PathBuf {
		self.data_dir.join("signing")
	}

	/// The install's random ID and the cached mirror key.
	pub fn mirror_key_file(&self) -> PathBuf {
		self.data_dir.join("mirror-key.json")
	}

	pub fn backups_dir(&self) -> PathBuf {
		self.data_dir.join("backups")
	}

	/// The user's Pictures folder, or the data folder when there is none or
	/// the app runs portable.
	pub fn screenshots_dir(&self) -> PathBuf {
		if self.portable_root.is_some() {
			return self.data_dir.join("screenshots");
		}
		directories::UserDirs::new()
			.and_then(|d| d.picture_dir().map(|p| p.join("Quest Sideloader")))
			.unwrap_or_else(|| self.data_dir.join("screenshots"))
	}

	/// adb downloaded by the app.
	pub fn platform_tools_dir(&self) -> PathBuf {
		self.data_dir.join("platform-tools")
	}

	/// rclone downloaded by the app.
	pub fn rclone_bin_dir(&self) -> PathBuf {
		self.data_dir.join("rclone-bin")
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
