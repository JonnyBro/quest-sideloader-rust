use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::Mirror;
use crate::error::{Error, IoContext, Result};
use crate::paths::AppPaths;

pub const MAX_CONCURRENT_DOWNLOADS: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedUnit {
	/// 1000 bytes.
	#[default]
	KB,
	/// 1 000 000 bytes.
	MB,
	/// 1024 bytes. Older settings files have it; [`Settings::normalize`]
	/// turns it into KB.
	KiB,
}

impl SpeedUnit {
	pub fn bytes(self) -> u64 {
		match self {
			Self::KB => 1000,
			Self::MB => 1_000_000,
			Self::KiB => 1024,
		}
	}

	pub fn label(self) -> &'static str {
		match self {
			Self::KB => "KB/s",
			Self::MB => "MB/s",
			Self::KiB => "KiB/s",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
	#[default]
	System,
	English,
	Russian,
	Ukrainian,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
	pub downloads_dir: PathBuf,
	pub max_concurrent_downloads: u8,
	pub download_limit: SpeedLimit,
	pub theme: Theme,
	/// Empty means the default scheme; the core doesn't know the schemes.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub dark_scheme: String,
	#[serde(skip_serializing_if = "String::is_empty")]
	pub light_scheme: String,
	pub language: Language,
	pub keep_downloads: bool,
	pub notifications: bool,
	pub check_app_updates: bool,
	pub backup_before_update: bool,
	/// `None` means `adb` from `PATH`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub adb_path: Option<PathBuf>,
	/// `None` means `rclone` from `PATH`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rclone_path: Option<PathBuf>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub active_mirror: Option<String>,
	#[serde(
		skip_serializing_if = "Vec::is_empty",
		deserialize_with = "skip_unknown_mirrors"
	)]
	pub mirrors: Vec<Mirror>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub contribute_ignored: Vec<String>,
	/// For example to keep mods that need an older version.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub update_ignored: Vec<String>,
	/// Packages starred in the catalog.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub favorite_games: Vec<String>,
	/// Packages the catalog leaves out unless asked to show them.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub hidden_games: Vec<String>,
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
		Self::load_from(paths, paths.settings_file())
	}

	/// Like [`Self::load`], but never fails: a broken file is moved to
	/// [`AppPaths::settings_broken_file`] and the backup from the last save is
	/// used, or the defaults when there is none.
	pub fn load_or_recover(paths: &AppPaths) -> (Self, Option<LoadProblem>) {
		let error = match Self::load(paths) {
			Ok(settings) => return (settings, None),
			Err(e) => e,
		};
		let broken = paths.settings_broken_file();
		let kept = std::fs::copy(paths.settings_file(), &broken)
			.is_ok()
			.then_some(broken);
		match Self::load_from(paths, paths.settings_backup_file()) {
			Ok(settings) if paths.settings_backup_file().exists() => (
				settings,
				Some(LoadProblem {
					error: error.to_string(),
					from_backup: true,
					kept,
				}),
			),
			_ => (
				Self::defaults_for(paths),
				Some(LoadProblem {
					error: error.to_string(),
					from_backup: false,
					kept,
				}),
			),
		}
	}

	fn load_from(paths: &AppPaths, path: PathBuf) -> Result<Self> {
		let text = match std::fs::read_to_string(&path) {
			Ok(text) => text,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				return Ok(Self::defaults_for(paths));
			}
			Err(e) => return Err(e).at(path),
		};
		let mut settings: Self =
			toml::from_str(&text).map_err(|source| Error::SettingsParse { path, source })?;
		if let Some(root) = paths.portable_root() {
			settings.map_paths(|p| {
				if p.is_relative() && !p.as_os_str().is_empty() {
					root.join(p)
				} else {
					p.to_path_buf()
				}
			});
		}
		if settings.downloads_dir.as_os_str().is_empty() {
			settings.downloads_dir = paths.default_downloads_dir();
		}
		settings.normalize();
		Ok(settings)
	}

	pub fn save(&self, paths: &AppPaths) -> Result<()> {
		let text = match paths.portable_root() {
			Some(root) => {
				let mut relative = self.clone();
				relative.map_paths(|p| p.strip_prefix(root).unwrap_or(p).to_path_buf());
				toml::to_string_pretty(&relative)?
			}
			None => toml::to_string_pretty(self)?,
		};
		let file = paths.settings_file();
		// A file that doesn't parse would replace the good backup.
		if let Ok(old) = std::fs::read_to_string(&file)
			&& old != text
			&& toml::from_str::<Self>(&old).is_ok()
		{
			let backup = paths.settings_backup_file();
			if let Err(e) = write_private(&backup, old.as_bytes()) {
				tracing::warn!("could not back up {}: {e}", file.display());
			}
		}
		write_atomically(&file, text.as_bytes())
	}

	pub fn normalize(&mut self) {
		if self.download_limit.unit == SpeedUnit::KiB {
			let bytes = self.download_limit.bytes_per_sec();
			self.download_limit = SpeedLimit {
				value: u32::try_from(bytes.div_ceil(1000)).unwrap_or(u32::MAX),
				unit: SpeedUnit::KB,
			};
		}
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

	fn map_paths(&mut self, f: impl Fn(&Path) -> PathBuf) {
		self.downloads_dir = f(&self.downloads_dir);
		for path in [&mut self.adb_path, &mut self.rclone_path]
			.into_iter()
			.flatten()
		{
			*path = f(path);
		}
		for mirror in &mut self.mirrors {
			if let crate::catalog::MirrorKind::Rclone { config_file, .. } = &mut mirror.kind {
				*config_file = f(config_file);
			}
		}
	}

	pub fn active_mirror(&self) -> Option<&Mirror> {
		let id = self.active_mirror.as_ref()?;
		self.mirrors.iter().find(|m| &m.id == id)
	}

	/// Adds or removes `package`; returns whether `list` changed.
	pub fn set_listed(list: &mut Vec<String>, package: &str, on: bool) -> bool {
		let listed = list.iter().any(|p| p == package);
		if on && !listed {
			list.push(package.to_owned());
		} else if !on && listed {
			list.retain(|p| p != package);
		} else {
			return false;
		}
		true
	}
}

impl Default for Settings {
	fn default() -> Self {
		Self {
			downloads_dir: PathBuf::new(),
			max_concurrent_downloads: 2,
			download_limit: SpeedLimit::default(),
			theme: Theme::default(),
			dark_scheme: String::new(),
			light_scheme: String::new(),
			language: Language::default(),
			keep_downloads: false,
			notifications: true,
			check_app_updates: true,
			backup_before_update: true,
			adb_path: None,
			rclone_path: None,
			active_mirror: None,
			mirrors: Vec::new(),
			contribute_ignored: Vec::new(),
			update_ignored: Vec::new(),
			favorite_games: Vec::new(),
			hidden_games: Vec::new(),
		}
	}
}

/// Why [`Settings::load_or_recover`] couldn't use the settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadProblem {
	pub error: String,
	/// The backup from the last save was used instead of the defaults.
	pub from_backup: bool,
	/// Where the unreadable file was copied to.
	pub kept: Option<PathBuf>,
}

/// Mirrors of a kind this version doesn't know (or that are broken) are
/// dropped with a warning instead of making the whole file unreadable.
fn skip_unknown_mirrors<'de, D>(deserializer: D) -> std::result::Result<Vec<Mirror>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	let raw = Vec::<toml::Value>::deserialize(deserializer)?;
	Ok(raw
		.into_iter()
		.filter_map(|value| match value.try_into::<Mirror>() {
			Ok(mirror) => Some(mirror),
			Err(e) => {
				tracing::warn!("skipping a mirror in the settings: {e}");
				None
			}
		})
		.collect())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).at(parent)?;
	}
	let tmp = path.with_extension("toml.tmp");
	// Mirror passwords and API keys live here.
	write_private(&tmp, bytes).at(&tmp)?;
	std::fs::rename(&tmp, path).at(path)
}

/// Readable only by this user on Unix.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	std::io::Write::write_all(&mut options.open(path)?, bytes)
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
	fn portable_paths_follow_the_folder() {
		let dir = tempfile::tempdir().unwrap();
		let old = dir.path().join("E/app/userdata");
		let paths = AppPaths::portable(&old);
		let mut settings = Settings::defaults_for(&paths);
		settings.adb_path = Some(paths.platform_tools_dir().join("adb"));
		let outside = PathBuf::from(if cfg!(windows) {
			r"C:\Tools\rclone.exe"
		} else {
			"/usr/bin/rclone"
		});
		settings.rclone_path = Some(outside.clone());
		settings.save(&paths).unwrap();

		let text = std::fs::read_to_string(paths.settings_file()).unwrap();
		assert!(!text.contains(&*old.to_string_lossy()), "{text}");

		let new = dir.path().join("F/app/userdata");
		std::fs::create_dir_all(new.parent().unwrap()).unwrap();
		std::fs::rename(&old, &new).unwrap();
		let moved = AppPaths::portable(&new);
		let loaded = Settings::load(&moved).unwrap();
		assert_eq!(loaded.downloads_dir, moved.default_downloads_dir());
		assert_eq!(
			loaded.adb_path,
			Some(moved.platform_tools_dir().join("adb"))
		);
		assert_eq!(loaded.rclone_path, Some(outside));
	}

	#[test]
	fn unknown_mirror_kinds_are_skipped() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		std::fs::create_dir_all(paths.settings_file().parent().unwrap()).unwrap();
		std::fs::write(
			paths.settings_file(),
			"active_mirror = \"gone\"\n\n\
			 [[mirrors]]\nid = \"gone\"\nname = \"Old\"\ntype = \"something-else\"\npassword = \"\"\n\n\
			 [[mirrors]]\nid = \"m1\"\nname = \"Main\"\ntype = \"http\"\nbase_uri = \"https://e.org/\"\npassword = \"aGk=\"\n",
		)
		.unwrap();
		let settings = Settings::load(&paths).unwrap();
		assert_eq!(settings.mirrors.len(), 1);
		assert_eq!(settings.mirrors[0].id, "m1");
		assert_eq!(settings.active_mirror.as_deref(), Some("m1"));
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
			dark_scheme: "Nord".into(),
			light_scheme: String::new(),
			language: Language::Russian,
			keep_downloads: true,
			notifications: false,
			check_app_updates: true,
			backup_before_update: true,
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
			}],
			contribute_ignored: vec!["com.discord".into()],
			update_ignored: vec!["com.beatgames.beatsaber".into()],
			favorite_games: vec!["com.beatgames.beatsaber".into()],
			hidden_games: vec!["com.example.hidden".into()],
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
	fn set_listed_reports_changes() {
		let mut list = Vec::new();
		assert!(Settings::set_listed(&mut list, "com.a", true));
		assert!(!Settings::set_listed(&mut list, "com.a", true));
		assert_eq!(list, ["com.a"]);
		assert!(Settings::set_listed(&mut list, "com.a", false));
		assert!(!Settings::set_listed(&mut list, "com.a", false));
		assert!(list.is_empty());
	}

	#[test]
	fn save_keeps_the_previous_settings() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		let mut settings = Settings::defaults_for(&paths);
		settings.save(&paths).unwrap();
		assert!(
			!paths.settings_backup_file().exists(),
			"nothing to keep yet"
		);

		settings.max_concurrent_downloads = 1;
		settings.save(&paths).unwrap();
		let first = std::fs::read_to_string(paths.settings_backup_file()).unwrap();
		settings.save(&paths).unwrap();
		assert_eq!(
			std::fs::read_to_string(paths.settings_backup_file()).unwrap(),
			first,
			"an unchanged save keeps the backup"
		);

		std::fs::write(paths.settings_file(), "theme = [").unwrap();
		settings.save(&paths).unwrap();
		assert_eq!(
			std::fs::read_to_string(paths.settings_backup_file()).unwrap(),
			first,
			"a broken file never replaces the backup"
		);
	}

	#[test]
	fn broken_file_falls_back_to_the_backup() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		let mut settings = Settings::defaults_for(&paths);
		settings.save(&paths).unwrap();
		settings.max_concurrent_downloads = 1;
		settings.save(&paths).unwrap();
		std::fs::write(paths.settings_file(), "theme = [").unwrap();

		let (loaded, problem) = Settings::load_or_recover(&paths);
		let problem = problem.expect("the broken file is reported");
		assert!(problem.from_backup);
		assert_eq!(loaded, Settings::defaults_for(&paths));
		assert_eq!(
			std::fs::read_to_string(problem.kept.unwrap()).unwrap(),
			"theme = ["
		);

		std::fs::remove_file(paths.settings_backup_file()).unwrap();
		let (loaded, problem) = Settings::load_or_recover(&paths);
		assert!(!problem.unwrap().from_backup);
		assert_eq!(loaded, Settings::defaults_for(&paths));
	}

	#[test]
	fn speed_limit_units() {
		let kb = SpeedLimit {
			value: 2,
			unit: SpeedUnit::KB,
		};
		let mb = SpeedLimit {
			value: 3,
			unit: SpeedUnit::MB,
		};
		assert_eq!(kb.bytes_per_sec(), 2000);
		assert_eq!(mb.bytes_per_sec(), 3_000_000);
		assert_eq!(SpeedLimit::default().bytes_per_sec(), 0);
		let text = toml::to_string(&Settings {
			download_limit: kb,
			..Settings::default()
		})
		.unwrap();
		assert!(text.contains("unit = \"kb\""), "{text}");
	}

	#[test]
	fn old_kib_limits_become_kb() {
		let mut settings = Settings {
			download_limit: SpeedLimit {
				value: 2,
				unit: SpeedUnit::KiB,
			},
			..Settings::default()
		};
		settings.normalize();
		assert_eq!(
			settings.download_limit,
			SpeedLimit {
				value: 3,
				unit: SpeedUnit::KB,
			}
		);
	}
}
