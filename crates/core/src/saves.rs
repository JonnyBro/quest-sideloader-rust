//! Backups of game saves: an app's `Android/data` folder copied to the
//! computer, one folder per backup under `<data dir>/backups/<package>/`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::adb::{AdbError, Device, Progress};

const META_FILE: &str = "backup.json";
const DATA_DIR: &str = "data";
/// The app's `/data/data` contents, when it allowed `run-as`.
const PRIVATE_DIR: &str = "data_private";

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
	#[error("{}", rust_i18n::t!("err.no_saves", app = .0))]
	NothingToBackUp(String),
	#[error("{}", rust_i18n::t!("err.not_installed", app = .0))]
	NotInstalled(String),
	#[error("{}", rust_i18n::t!("err.private_restore", error = .0))]
	PrivateRestore(String),
	#[error(transparent)]
	Adb(#[from] AdbError),
}

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> SaveError {
	let path = path.into();
	move |source| SaveError::Io { path, source }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupMeta {
	pub package: String,
	/// What the user saw when backing up: the game's name or the package.
	pub name: String,
	pub version_code: Option<u64>,
	/// Unix seconds.
	pub created: u64,
	pub files: u64,
	pub bytes: u64,
	/// How many of the files came from the app's private data.
	#[serde(default)]
	pub private_files: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
	pub dir: PathBuf,
	pub meta: BackupMeta,
}

fn device_data_dir(package: &str) -> String {
	format!("/sdcard/Android/data/{package}")
}

/// Every backup, newest first.
pub fn list(root: &Path) -> Vec<Backup> {
	let mut backups = Vec::new();
	let Ok(packages) = std::fs::read_dir(root) else {
		return backups;
	};
	for package in packages.filter_map(|e| e.ok()) {
		let Ok(entries) = std::fs::read_dir(package.path()) else {
			continue;
		};
		for entry in entries.filter_map(|e| e.ok()) {
			let dir = entry.path();
			let Ok(text) = std::fs::read(dir.join(META_FILE)) else {
				continue;
			};
			if let Ok(meta) = serde_json::from_slice::<BackupMeta>(&text) {
				backups.push(Backup { dir, meta });
			}
		}
	}
	backups.sort_by_key(|b| std::cmp::Reverse(b.meta.created));
	backups
}

pub fn delete(backup: &Backup) -> Result<(), SaveError> {
	std::fs::remove_dir_all(&backup.dir).map_err(io_err(&backup.dir))
}

/// Folders apps rebuild on their own; not worth backing up.
const SKIPPED_TOP_DIRS: [&str; 2] = ["cache", "code_cache"];

/// Scratch folders on the headset for moving private data through `run-as`.
const BACKUP_TMP: &str = "/sdcard/backup_tmp";
const RESTORE_TMP: &str = "/sdcard/restore_tmp";

/// `(remote, relative)` for files under `remote_root`, minus caches and odd paths.
fn relative_files(files: &[(String, u64)], remote_root: &str) -> Vec<(String, String, u64)> {
	files
		.iter()
		.filter_map(|(remote, size)| {
			let relative = remote.strip_prefix(&format!("{remote_root}/"))?;
			let mut parts = relative.split('/');
			if relative
				.split('/')
				.any(|part| part.is_empty() || part == "..")
				|| parts
					.next()
					.is_some_and(|top| SKIPPED_TOP_DIRS.contains(&top) && relative.contains('/'))
			{
				return None;
			}
			Some((remote.clone(), relative.to_owned(), *size))
		})
		.collect()
}

/// Copies `/data/data/<package>` to a scratch folder with `run-as`. Only
/// debuggable apps allow that; for the rest this finds nothing, which is fine.
async fn stage_private_data(
	device: &Device,
	package: &str,
) -> Result<Vec<(String, String, u64)>, SaveError> {
	let q = |s: &str| shell_words::quote(s).into_owned();
	let tmp = format!("{BACKUP_TMP}/{package}");
	let out = device
		.shell(&format!(
			"rm -rf {root}; mkdir -p {tmp}; run-as {pkg} tar -cf - -C {src} . | tar -xf - -C {tmp}",
			root = q(BACKUP_TMP),
			tmp = q(&tmp),
			pkg = q(package),
			src = q(&format!("/data/data/{package}")),
		))
		.await?;
	if out.stderr.contains("run-as:") {
		tracing::debug!(%package, stderr = %out.stderr.trim(), "no private data (run-as refused)");
		return Ok(Vec::new());
	}
	let files = device.list_files(&tmp).await?;
	Ok(relative_files(&files, &tmp))
}

/// Copies the app's saves from the headset: its `Android/data` folder, and
/// its private data when the app allows `run-as`. Only a scratch folder on
/// the headset is written, and removed again.
pub async fn back_up(
	device: &Device,
	root: &Path,
	package: &str,
	name: &str,
	version_code: Option<u64>,
	progress: Progress<'_>,
) -> Result<Backup, SaveError> {
	let shared_root = device_data_dir(package);
	let shared = relative_files(&device.list_files(&shared_root).await?, &shared_root);
	let private = stage_private_data(device, package).await;
	let cleanup = async {
		let _ = device.shell(&format!("rm -rf {BACKUP_TMP}")).await;
	};
	let private = match private {
		Ok(private) => private,
		Err(e) => {
			cleanup.await;
			return Err(e);
		}
	};
	if shared.is_empty() && private.is_empty() {
		cleanup.await;
		return Err(SaveError::NothingToBackUp(package.to_owned()));
	}

	let created = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |d| d.as_secs());
	let package_dir = root.join(package);
	let staging = package_dir.join(format!(".{created}.partial"));
	let _ = tokio::fs::remove_dir_all(&staging).await;

	let pulls: Vec<(String, std::path::PathBuf)> = shared
		.iter()
		.map(|(remote, relative, _)| (remote.clone(), staging.join(DATA_DIR).join(relative)))
		.chain(private.iter().map(|(remote, relative, _)| {
			(remote.clone(), staging.join(PRIVATE_DIR).join(relative))
		}))
		.collect();
	let pulled = device.pull(&pulls, progress).await;
	cleanup.await;
	pulled?;

	let meta = BackupMeta {
		package: package.to_owned(),
		name: name.to_owned(),
		version_code,
		created,
		files: pulls.len() as u64,
		bytes: shared.iter().chain(&private).map(|(_, _, size)| size).sum(),
		private_files: private.len() as u64,
	};
	let json = serde_json::to_vec_pretty(&meta).expect("backup metadata serializes");
	let meta_path = staging.join(META_FILE);
	tokio::fs::write(&meta_path, json)
		.await
		.map_err(io_err(&meta_path))?;
	let dir = package_dir.join(created.to_string());
	tokio::fs::rename(&staging, &dir)
		.await
		.map_err(io_err(&dir))?;
	Ok(Backup { dir, meta })
}

/// Puts a backup back. The app has to be installed first, or Android hands
/// the restored folders to the wrong owner. Files in the backup replace those
/// on the headset; others are left alone.
pub async fn restore(
	device: &Device,
	backup: &Backup,
	progress: Progress<'_>,
) -> Result<(), SaveError> {
	let package = &backup.meta.package;
	if device.apk_path(package).await.is_err() {
		return Err(SaveError::NotInstalled(package.clone()));
	}
	let shared = backup.dir.join(DATA_DIR);
	if shared.is_dir() {
		device
			.push(&shared, &device_data_dir(package), progress)
			.await?;
	}
	let private = backup.dir.join(PRIVATE_DIR);
	if private.is_dir() {
		let q = |s: &str| shell_words::quote(s).into_owned();
		let tmp = format!("{RESTORE_TMP}/{package}");
		let _ = device.shell(&format!("rm -rf {}", q(RESTORE_TMP))).await;
		let pushed = device.push(&private, &tmp, progress).await;
		let result = match pushed {
			Ok(()) => device
				.shell(&format!(
					"tar -cf - -C {tmp} . | run-as {pkg} tar -xf - -C {dest}",
					tmp = q(&tmp),
					pkg = q(package),
					dest = q(&format!("/data/data/{package}")),
				))
				.await
				.map_err(SaveError::from),
			Err(e) => Err(e.into()),
		};
		let _ = device.shell(&format!("rm -rf {}", q(RESTORE_TMP))).await;
		let out = result?;
		if out.stderr.contains("run-as:") {
			return Err(SaveError::PrivateRestore(out.stderr.trim().to_owned()));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::adb::AdbServer;

	fn write_backup(root: &Path, package: &str, created: u64) {
		let dir = root.join(package).join(created.to_string());
		std::fs::create_dir_all(dir.join(DATA_DIR)).unwrap();
		let meta = BackupMeta {
			package: package.into(),
			name: package.into(),
			version_code: Some(1),
			created,
			files: 1,
			bytes: 3,
			private_files: 0,
		};
		std::fs::write(dir.join(META_FILE), serde_json::to_vec(&meta).unwrap()).unwrap();
	}

	#[test]
	fn lists_newest_first_and_skips_partial() {
		let dir = tempfile::tempdir().unwrap();
		write_backup(dir.path(), "com.a.b", 100);
		write_backup(dir.path(), "com.a.b", 300);
		write_backup(dir.path(), "com.c.d", 200);
		std::fs::create_dir_all(dir.path().join("com.a.b/.400.partial")).unwrap();
		let created: Vec<_> = list(dir.path()).iter().map(|b| b.meta.created).collect();
		assert_eq!(created, [300, 200, 100]);
		delete(&list(dir.path())[0]).unwrap();
		assert_eq!(list(dir.path()).len(), 2);
		assert!(list(&dir.path().join("missing")).is_empty());
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn backs_up_and_restores_through_adb() {
		let fake = crate::adb::testing::FakeAdb::start().await;
		fake.recorded.lock().unwrap().device_files = vec![
			(
				"/sdcard/Android/data/com.a.b/files/save.dat".into(),
				b"save".to_vec(),
			),
			(
				"/sdcard/Android/data/com.a.b/cache/junk.bin".into(),
				b"junk".to_vec(),
			),
			// Where `run-as ... tar` would have copied the private data.
			(
				"/sdcard/backup_tmp/com.a.b/shared_prefs/prefs.xml".into(),
				b"<x/>".to_vec(),
			),
			(
				"/sdcard/backup_tmp/com.a.b/code_cache/x.dex".into(),
				b"dex".to_vec(),
			),
		];
		let device = AdbServer::with_port(Some("/no/adb".into()), fake.port).device("SER1");
		let dir = tempfile::tempdir().unwrap();

		let backup = back_up(&device, dir.path(), "com.a.b", "Game", Some(5), &|_| {})
			.await
			.unwrap();
		assert_eq!((backup.meta.files, backup.meta.private_files), (2, 1));
		assert_eq!(
			std::fs::read(backup.dir.join("data/files/save.dat")).unwrap(),
			b"save"
		);
		assert_eq!(
			std::fs::read(backup.dir.join("data_private/shared_prefs/prefs.xml")).unwrap(),
			b"<x/>"
		);
		assert!(
			!backup.dir.join("data/cache").exists(),
			"caches are skipped"
		);
		assert!(!backup.dir.join("data_private/code_cache").exists());
		assert_eq!(list(dir.path()), std::slice::from_ref(&backup));
		{
			let rec = fake.recorded.lock().unwrap();
			assert!(
				rec.shell
					.iter()
					.any(|c| c.contains("run-as com.a.b tar -cf -"))
			);
			assert_eq!(
				rec.shell.last().unwrap(),
				"rm -rf /sdcard/backup_tmp",
				"scratch folder removed"
			);
		}

		restore(&device, &backup, &|_| {}).await.unwrap();
		{
			let rec = fake.recorded.lock().unwrap();
			let mut pushed: Vec<_> = rec.pushed.iter().map(|(p, _)| p.clone()).collect();
			pushed.sort();
			assert_eq!(
				pushed,
				[
					"/sdcard/Android/data/com.a.b/files/save.dat",
					"/sdcard/restore_tmp/com.a.b/shared_prefs/prefs.xml"
				]
			);
			assert!(
				rec.shell
					.iter()
					.any(|c| c.contains("| run-as com.a.b tar -xf - -C /data/data/com.a.b"))
			);
			assert_eq!(rec.shell.last().unwrap(), "rm -rf /sdcard/restore_tmp");
		}

		fake.recorded.lock().unwrap().device_files.clear();
		let err = back_up(&device, dir.path(), "com.empty.x", "Empty", None, &|_| {})
			.await
			.unwrap_err();
		assert!(matches!(err, SaveError::NothingToBackUp(_)));
	}
}
