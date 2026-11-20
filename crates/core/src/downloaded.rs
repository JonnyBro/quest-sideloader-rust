//! Releases that finished downloading and sit in the downloads folder.

use rust_i18n::t;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedRelease {
	pub name: String,
	pub path: PathBuf,
	pub size: u64,
	/// Unix seconds of the folder's last change.
	pub modified: Option<u64>,
}

/// Release folders in `root`, newest first. Hidden entries (like the
/// `.incomplete` staging folder) and loose files are left out.
/// Free bytes on the drive holding `path`, if it can be read.
pub fn free_space(path: &Path) -> Option<u64> {
	fs4::available_space(path).ok()
}

pub fn list(root: &Path) -> Vec<DownloadedRelease> {
	let Ok(entries) = std::fs::read_dir(root) else {
		return Vec::new();
	};
	let mut releases: Vec<DownloadedRelease> = entries
		.filter_map(|e| e.ok())
		.filter_map(|entry| {
			let name = entry.file_name().into_string().ok()?;
			if name.starts_with('.') || !entry.file_type().ok()?.is_dir() {
				return None;
			}
			let modified = entry
				.metadata()
				.and_then(|m| m.modified())
				.ok()
				.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
				.map(|d| d.as_secs());
			let path = entry.path();
			Some(DownloadedRelease {
				size: dir_size(&path),
				name,
				path,
				modified,
			})
		})
		.collect();
	releases.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
	releases
}

fn dir_size(dir: &Path) -> u64 {
	let mut total = 0;
	let mut stack = vec![dir.to_path_buf()];
	while let Some(dir) = stack.pop() {
		let Ok(entries) = std::fs::read_dir(&dir) else {
			continue;
		};
		for entry in entries.filter_map(|e| e.ok()) {
			let Ok(kind) = entry.file_type() else {
				continue;
			};
			if kind.is_dir() {
				stack.push(entry.path());
			} else if kind.is_file() {
				total += entry.metadata().map(|m| m.len()).unwrap_or(0);
			}
		}
	}
	total
}

/// Deletes the release folder `name` from `root`; refuses anything that is
/// not a plain release folder name.
pub fn delete(root: &Path, name: &str) -> std::io::Result<()> {
	let plain = !name.is_empty()
		&& !name.starts_with('.')
		&& Path::new(name).components().count() == 1
		&& !name.contains(['/', '\\']);
	if !plain {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			t!("dld.not_release", name = name),
		));
	}
	std::fs::remove_dir_all(root.join(name))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lists_release_folders_and_deletes_them() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path();
		std::fs::create_dir_all(root.join("Game v1 +1/com.game")).unwrap();
		std::fs::write(root.join("Game v1 +1/game.apk"), [0u8; 100]).unwrap();
		std::fs::write(root.join("Game v1 +1/com.game/main.obb"), [0u8; 50]).unwrap();
		std::fs::create_dir_all(root.join(".incomplete/Other v2")).unwrap();
		std::fs::write(root.join("loose.apk"), [0u8; 10]).unwrap();

		let releases = list(root);
		assert_eq!(releases.len(), 1);
		assert_eq!(releases[0].name, "Game v1 +1");
		assert_eq!(releases[0].size, 150);

		assert!(delete(root, "..").is_err());
		assert!(delete(root, ".incomplete").is_err());
		assert!(delete(root, "Game v1 +1/com.game").is_err());
		delete(root, "Game v1 +1").unwrap();
		assert!(list(root).is_empty());
		assert!(root.join(".incomplete").exists());
	}
}
