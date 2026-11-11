use std::path::{Path, PathBuf};

use super::{SideloadError, io_err};
use crate::adb::list_files;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
	Apk(PathBuf),
	Folder(PathBuf),
	Zip(PathBuf),
}

impl Source {
	pub fn classify(path: &Path) -> Result<Self, SideloadError> {
		let metadata = std::fs::metadata(path).map_err(io_err(path))?;
		if metadata.is_dir() {
			return Ok(Self::Folder(path.to_path_buf()));
		}
		match extension(path).as_deref() {
			Some("apk") => Ok(Self::Apk(path.to_path_buf())),
			Some("zip") => Ok(Self::Zip(path.to_path_buf())),
			_ => Err(SideloadError::Unsupported(path.to_path_buf())),
		}
	}

	pub fn path(&self) -> &Path {
		match self {
			Self::Apk(p) | Self::Folder(p) | Self::Zip(p) => p,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApkMeta {
	pub path: PathBuf,
	pub size: u64,
	pub package: String,
	pub version_code: Option<u64>,
	pub version_name: Option<String>,
	pub label: Option<String>,
}

/// Only the APK's central directory and manifest are read, not the whole file.
pub fn read_apk_meta(path: &Path) -> Result<ApkMeta, SideloadError> {
	let apk_err = |message: String| SideloadError::Apk {
		path: path.to_path_buf(),
		message,
	};
	let size = std::fs::metadata(path).map_err(io_err(path))?.len();
	let apk = apk_info::Apk::new(path).map_err(|e| apk_err(e.to_string()))?;
	let package = apk
		.get_package_name()
		.filter(|p| is_package_name(p))
		.ok_or_else(|| apk_err("manifest has no valid package name".into()))?;
	Ok(ApkMeta {
		path: path.to_path_buf(),
		size,
		package,
		version_code: apk.get_version_code().and_then(|v| v.parse().ok()),
		version_name: apk.get_version_name(),
		label: apk.get_application_label().filter(|l| !l.trim().is_empty()),
	})
}

/// A folder pushed as-is to `/sdcard/Android/obb/<package>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObbFolder {
	pub path: PathBuf,
	pub package: String,
	pub size: u64,
}

impl ObbFolder {
	pub fn remote_dir(&self) -> String {
		format!("/sdcard/Android/obb/{}", self.package)
	}

	fn probe(path: &Path, package: &str) -> Result<Option<Self>, SideloadError> {
		let files = list_files(path).map_err(io_err(path))?;
		if files.is_empty() {
			return Ok(None);
		}
		Ok(Some(Self {
			path: path.to_path_buf(),
			package: package.to_owned(),
			size: files.iter().map(|f| f.size).sum(),
		}))
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
	/// Folder that relative paths in install.txt are resolved against.
	pub root: PathBuf,
	pub title: String,
	pub apks: Vec<ApkMeta>,
	pub obbs: Vec<ObbFolder>,
	/// When present, the script replaces the default APK + OBB steps.
	pub script: Option<PathBuf>,
}

impl InstallPlan {
	pub fn package(&self) -> Option<&str> {
		self.apks
			.first()
			.map(|a| a.package.as_str())
			.or_else(|| self.obbs.first().map(|o| o.package.as_str()))
	}

	pub fn default_steps_bytes(&self) -> u64 {
		self.apks.iter().map(|a| a.size).sum::<u64>()
			+ self.obbs.iter().map(|o| o.size).sum::<u64>()
	}

	/// A single APK, plus a sibling folder named after its package if there is one.
	pub fn from_apk(path: &Path) -> Result<Self, SideloadError> {
		let meta = read_apk_meta(path)?;
		let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
		let sibling = root.join(&meta.package);
		let obbs = if sibling.is_dir() {
			ObbFolder::probe(&sibling, &meta.package)?
				.into_iter()
				.collect()
		} else {
			Vec::new()
		};
		Ok(Self {
			title: title_for(&meta, path),
			root,
			apks: vec![meta],
			obbs,
			script: None,
		})
	}

	pub fn from_folder(dir: &Path) -> Result<Self, SideloadError> {
		let Some(game_dir) = find_game_dir(dir)? else {
			// A bare OBB folder, e.g. "com.example.game" with only .obb files inside.
			if let Some(package) = file_name(dir).filter(|n| is_package_name(n))
				&& let Some(obb) = ObbFolder::probe(dir, &package)?
			{
				return Ok(Self {
					root: dir.to_path_buf(),
					title: package,
					apks: Vec::new(),
					obbs: vec![obb],
					script: None,
				});
			}
			return Err(SideloadError::NothingToInstall(dir.to_path_buf()));
		};

		let mut apks = Vec::new();
		let mut obbs = Vec::new();
		let mut script = None;
		for entry in sorted_entries(&game_dir)? {
			let path = entry.path();
			let file_type = entry.file_type().map_err(io_err(&path))?;
			let name = entry.file_name().to_string_lossy().into_owned();
			if file_type.is_file() {
				if name.eq_ignore_ascii_case("install.txt") {
					script = Some(path);
				} else if extension(&path).as_deref() == Some("apk") {
					apks.push(read_apk_meta(&path)?);
				}
			} else if file_type.is_dir()
				&& is_package_name(&name)
				&& let Some(obb) = ObbFolder::probe(&path, &name)?
			{
				obbs.push(obb);
			}
		}

		let title = apks
			.first()
			.map(|a| title_for(a, &a.path))
			.or_else(|| file_name(&game_dir))
			.unwrap_or_else(|| game_dir.display().to_string());
		Ok(Self {
			root: game_dir,
			title,
			apks,
			obbs,
			script,
		})
	}
}

/// The folder itself if it holds an APK or install.txt, otherwise its only
/// subfolder that does (archives often wrap the release in one directory).
fn find_game_dir(dir: &Path) -> Result<Option<PathBuf>, SideloadError> {
	if has_payload(dir)? {
		return Ok(Some(dir.to_path_buf()));
	}
	let mut candidates = Vec::new();
	for entry in sorted_entries(dir)? {
		let path = entry.path();
		if entry.file_type().map_err(io_err(&path))?.is_dir() && has_payload(&path)? {
			candidates.push(path);
		}
	}
	Ok(if candidates.len() == 1 {
		candidates.pop()
	} else {
		None
	})
}

fn has_payload(dir: &Path) -> Result<bool, SideloadError> {
	for entry in sorted_entries(dir)? {
		let path = entry.path();
		if !entry.file_type().map_err(io_err(&path))?.is_file() {
			continue;
		}
		let name = entry.file_name();
		let name = name.to_string_lossy();
		if name.eq_ignore_ascii_case("install.txt") || extension(&path).as_deref() == Some("apk") {
			return Ok(true);
		}
	}
	Ok(false)
}

fn sorted_entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>, SideloadError> {
	let mut entries = std::fs::read_dir(dir)
		.map_err(io_err(dir))?
		.collect::<Result<Vec<_>, _>>()
		.map_err(io_err(dir))?;
	entries.sort_by_key(|e| e.file_name());
	Ok(entries)
}

fn title_for(meta: &ApkMeta, path: &Path) -> String {
	meta.label
		.clone()
		.or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
		.unwrap_or_else(|| meta.package.clone())
}

fn extension(path: &Path) -> Option<String> {
	path.extension()
		.map(|e| e.to_string_lossy().to_ascii_lowercase())
}

fn file_name(path: &Path) -> Option<String> {
	path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Java-style package name: at least two dot-separated segments, each starting
/// with a letter and containing only letters, digits and underscores.
pub fn is_package_name(name: &str) -> bool {
	let mut segments = 0;
	for segment in name.split('.') {
		let mut chars = segment.chars();
		let Some(first) = chars.next() else {
			return false;
		};
		if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
			return false;
		}
		segments += 1;
	}
	segments >= 2
}

/// Extracts a ZIP into `dest`; entries with paths escaping `dest` are rejected by the zip crate.
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<(), SideloadError> {
	let zip_err = |message: String| SideloadError::Zip {
		path: zip.to_path_buf(),
		message,
	};
	let file = std::fs::File::open(zip).map_err(io_err(zip))?;
	let mut archive =
		zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| zip_err(e.to_string()))?;
	std::fs::create_dir_all(dest).map_err(io_err(dest))?;
	archive.extract(dest).map_err(|e| zip_err(e.to_string()))
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use super::*;

	#[test]
	fn package_names() {
		for ok in [
			"com.example.game",
			"com.Armature.VR4",
			"a.b",
			"org.meumeu.wivrn",
			"x_1.y2",
		] {
			assert!(is_package_name(ok), "{ok}");
		}
		for bad in [
			"",
			"game",
			"com..game",
			".com.game",
			"com.1game",
			"com.game-x",
			"../etc",
			"com/game",
		] {
			assert!(!is_package_name(bad), "{bad}");
		}
	}

	#[test]
	fn classifies_sources() {
		let dir = tempfile::tempdir().unwrap();
		let apk = dir.path().join("Game.APK");
		let zip = dir.path().join("r.zip");
		let txt = dir.path().join("notes.txt");
		for p in [&apk, &zip, &txt] {
			std::fs::write(p, b"x").unwrap();
		}
		assert_eq!(Source::classify(&apk).unwrap(), Source::Apk(apk.clone()));
		assert_eq!(Source::classify(&zip).unwrap(), Source::Zip(zip.clone()));
		assert_eq!(
			Source::classify(dir.path()).unwrap(),
			Source::Folder(dir.path().to_path_buf())
		);
		assert!(matches!(
			Source::classify(&txt),
			Err(SideloadError::Unsupported(_))
		));
	}

	#[test]
	fn bare_obb_folder() {
		let dir = tempfile::tempdir().unwrap();
		let obb = dir.path().join("com.example.game");
		std::fs::create_dir(&obb).unwrap();
		std::fs::write(obb.join("main.1.com.example.game.obb"), b"0123456789").unwrap();

		let plan = InstallPlan::from_folder(&obb).unwrap();
		assert!(plan.apks.is_empty());
		assert_eq!(plan.obbs.len(), 1);
		assert_eq!(
			plan.obbs[0].remote_dir(),
			"/sdcard/Android/obb/com.example.game"
		);
		assert_eq!(plan.obbs[0].size, 10);
		assert_eq!(plan.package(), Some("com.example.game"));
	}

	#[test]
	fn script_folder_inside_wrapper() {
		let dir = tempfile::tempdir().unwrap();
		let release = dir.path().join("Some Game v1");
		std::fs::create_dir_all(release.join("com.example.game")).unwrap();
		std::fs::write(release.join("Install.txt"), b"adb push x /sdcard/").unwrap();
		std::fs::write(release.join("com.example.game/main.obb"), b"o").unwrap();
		std::fs::create_dir(release.join("not a package")).unwrap();

		let plan = InstallPlan::from_folder(dir.path()).unwrap();
		assert_eq!(plan.root, release);
		assert_eq!(plan.script, Some(release.join("Install.txt")));
		assert_eq!(plan.obbs.len(), 1);
		assert_eq!(plan.title, "Some Game v1");
	}

	#[test]
	fn empty_folder_has_nothing() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("readme.md"), b"hi").unwrap();
		assert!(matches!(
			InstallPlan::from_folder(dir.path()),
			Err(SideloadError::NothingToInstall(_))
		));
	}

	#[test]
	fn ambiguous_wrapper_is_rejected() {
		let dir = tempfile::tempdir().unwrap();
		for name in ["a", "b"] {
			std::fs::create_dir(dir.path().join(name)).unwrap();
			std::fs::write(dir.path().join(name).join("install.txt"), b"").unwrap();
		}
		assert!(matches!(
			InstallPlan::from_folder(dir.path()),
			Err(SideloadError::NothingToInstall(_))
		));
	}

	#[test]
	fn zip_extracts_and_rejects_bad_archives() {
		let dir = tempfile::tempdir().unwrap();
		let zip_path = dir.path().join("release.zip");
		{
			let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
			let options = zip::write::SimpleFileOptions::default();
			writer.start_file("Game/install.txt", options).unwrap();
			writer.write_all(b"adb install game.apk").unwrap();
			writer.finish().unwrap();
		}
		let out = dir.path().join("out");
		extract_zip(&zip_path, &out).unwrap();
		assert!(out.join("Game/install.txt").is_file());

		let broken = dir.path().join("broken.zip");
		std::fs::write(&broken, b"not a zip").unwrap();
		assert!(matches!(
			extract_zip(&broken, &out),
			Err(SideloadError::Zip { .. })
		));
	}
}
