//! Browsing shared storage for the file manager.

use std::path::{Path, PathBuf};

use super::{AdbError, Device};

/// Where the file manager starts; nothing outside it can be deleted.
pub const STORAGE_ROOT: &str = "/sdcard";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
	pub name: String,
	pub is_dir: bool,
	pub size: u64,
	/// Unix seconds.
	pub modified: u64,
}

/// `path` is `/sdcard` or inside it, with no `..` to climb out.
pub fn in_storage(path: &str) -> bool {
	let path = path.trim_end_matches('/');
	(path == STORAGE_ROOT || path.starts_with(&format!("{STORAGE_ROOT}/")))
		&& !path.split('/').any(|part| part == "..")
}

pub fn join(dir: &str, name: &str) -> String {
	format!("{}/{name}", dir.trim_end_matches('/'))
}

/// `stat -L -c '%F/%s/%Y/%n'` lines.
fn parse_listing(text: &str) -> Vec<RemoteEntry> {
	text.lines()
		.filter_map(|line| {
			let mut parts = line.splitn(4, '/');
			let kind = parts.next()?;
			let size = parts.next()?.parse().ok()?;
			let modified = parts.next()?.parse().ok()?;
			let name = parts.next()?.rsplit('/').next()?;
			if name.is_empty() {
				return None;
			}
			Some(RemoteEntry {
				name: name.to_owned(),
				is_dir: kind == "directory",
				size,
				modified,
			})
		})
		.collect()
}

impl Device {
	/// What's in `dir`, folders first, then by name.
	pub async fn list_dir(&self, dir: &str) -> Result<Vec<RemoteEntry>, AdbError> {
		let quoted = shell_words::quote(dir);
		// -H: /sdcard itself is a symlink. -L: show linked folders as folders.
		let out = self
			.shell(&format!(
				"find -H {quoted} -mindepth 1 -maxdepth 1 -exec stat -L -c '%F/%s/%Y/%n' {{}} +"
			))
			.await?;
		let mut entries = parse_listing(&out.stdout);
		if entries.is_empty() && !out.success() && !out.stderr.trim().is_empty() {
			return Err(AdbError::Server(out.stderr.trim().to_owned()));
		}
		entries.sort_by(|a, b| {
			b.is_dir
				.cmp(&a.is_dir)
				.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
		});
		Ok(entries)
	}

	/// Every regular file under `paths` (files or folders) paired with where
	/// it goes under `dest`, and the total size.
	pub async fn pull_plan(
		&self,
		paths: &[String],
		dest: &Path,
	) -> Result<(Vec<(String, PathBuf)>, u64), AdbError> {
		let mut plan = Vec::new();
		let mut total = 0;
		for path in paths {
			let path = path.trim_end_matches('/');
			let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
			let out = self
				.shell(&format!(
					"find -H {} -type f -exec stat -c '%s %n' {{}} +",
					shell_words::quote(path)
				))
				.await?;
			for line in out.stdout.lines() {
				let Some((size, file)) = line.split_once(' ') else {
					continue;
				};
				let Some(relative) = file.strip_prefix(parent) else {
					continue;
				};
				let mut local = dest.to_path_buf();
				local.extend(
					relative
						.split('/')
						.filter(|p| !p.is_empty() && *p != "." && *p != ".."),
				);
				total += size.parse::<u64>().unwrap_or(0);
				plan.push((file.to_owned(), local));
			}
		}
		Ok((plan, total))
	}

	/// Deletes files and folders inside [`STORAGE_ROOT`].
	pub async fn remove_paths(&self, paths: &[String]) -> Result<(), AdbError> {
		if let Some(bad) = paths
			.iter()
			.find(|p| !in_storage(p) || p.trim_end_matches('/') == STORAGE_ROOT)
		{
			return Err(AdbError::Protocol(format!("refusing to delete {bad}")));
		}
		let quoted: Vec<_> = paths.iter().map(|p| shell_words::quote(p)).collect();
		self.shell_ok(&format!("rm -rf {}", quoted.join(" ")))
			.await
			.map(drop)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_stat_lines() {
		let text = "directory/3452/1700000000//sdcard/Oculus\n\
			regular file/1024/1700000100//sdcard/a b.txt\n\
			regular empty file/0/1700000200//sdcard/empty\n\
			garbage\n";
		assert_eq!(
			parse_listing(text),
			[
				RemoteEntry {
					name: "Oculus".into(),
					is_dir: true,
					size: 3452,
					modified: 1_700_000_000,
				},
				RemoteEntry {
					name: "a b.txt".into(),
					is_dir: false,
					size: 1024,
					modified: 1_700_000_100,
				},
				RemoteEntry {
					name: "empty".into(),
					is_dir: false,
					size: 0,
					modified: 1_700_000_200,
				},
			]
		);
	}

	#[test]
	fn storage_bounds() {
		assert!(in_storage("/sdcard"));
		assert!(in_storage("/sdcard/Oculus/Screenshots/"));
		assert!(!in_storage("/sdcardx"));
		assert!(!in_storage("/sdcard/../system"));
		assert!(!in_storage("/data"));
	}
}
