//! The synced library on disk: `<catalog dir>/<mirror id>/current/` holds the
//! extracted `meta.7z` (game list, `.meta/thumbnails`, `.meta/notes`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use super::fetch::{FetchProgress, Fetcher};
use super::list::{Game, parse_game_list};
use super::mirror::Mirror;
use super::{CatalogError, io_err};
use crate::archive::extract_7z;

#[derive(Debug)]
pub struct Catalog {
	pub mirror_id: String,
	pub games: Vec<Game>,
	pub synced_at: SystemTime,
	meta_dir: PathBuf,
	thumbnails: HashSet<String>,
	blacklist: HashSet<String>,
}

impl Catalog {
	pub fn thumbnail(&self, package: &str) -> Option<PathBuf> {
		["jpg", "png"].iter().find_map(|ext| {
			let file = format!("{package}.{ext}");
			self.thumbnails
				.contains(&file)
				.then(|| self.meta_dir.join(".meta/thumbnails").join(file))
		})
	}

	/// The release's note from `.meta/notes/<release>.txt`, if the mirror has one.
	pub fn note(&self, release_name: &str) -> Option<String> {
		if release_name.contains(['/', '\\']) || release_name.starts_with('.') {
			return None;
		}
		let path = self
			.meta_dir
			.join(".meta/notes")
			.join(format!("{release_name}.txt"));
		let bytes = std::fs::read(path).ok()?;
		let text = String::from_utf8_lossy(&bytes).trim().to_owned();
		(!text.is_empty()).then_some(text)
	}

	/// rclone config the mirror publishes for contributing new app versions.
	pub fn upload_config(&self) -> Option<PathBuf> {
		let path = self.meta_dir.join(".meta/upload.config");
		path.is_file().then_some(path)
	}

	/// Packages the mirror doesn't want offered for upload.
	pub fn is_blacklisted(&self, package: &str) -> bool {
		self.blacklist.contains(package)
	}

	/// Loads a previously synced catalog; `None` if this mirror was never synced.
	pub fn load(root: &Path, mirror_id: &str) -> Result<Option<Self>, CatalogError> {
		let dir = root.join(mirror_id).join("current");
		if !dir.is_dir() {
			return Ok(None);
		}
		let list = find_game_list(&dir)?;
		let text = std::fs::read(&list).map_err(io_err(&list))?;
		let games = parse_game_list(&String::from_utf8_lossy(&text))?;
		let synced_at = std::fs::metadata(&list)
			.and_then(|m| m.modified())
			.unwrap_or_else(|_| SystemTime::now());
		let thumbnails = std::fs::read_dir(dir.join(".meta/thumbnails"))
			.map(|entries| {
				entries
					.filter_map(|e| e.ok())
					.map(|e| e.file_name().to_string_lossy().into_owned())
					.collect()
			})
			.unwrap_or_default();
		let blacklist = std::fs::read_to_string(dir.join(".meta/nouns/blacklist.txt"))
			.map(|text| {
				text.lines()
					.map(|l| l.trim().to_owned())
					.filter(|l| !l.is_empty())
					.collect()
			})
			.unwrap_or_default();
		Ok(Some(Self {
			mirror_id: mirror_id.to_owned(),
			games,
			synced_at,
			meta_dir: dir,
			thumbnails,
			blacklist,
		}))
	}
}

/// The list file has been called `VRP-GameList.txt` and `GameList.txt`.
fn find_game_list(dir: &Path) -> Result<PathBuf, CatalogError> {
	let entries = std::fs::read_dir(dir).map_err(io_err(dir))?;
	let mut found: Vec<PathBuf> = entries
		.filter_map(|e| e.ok())
		.map(|e| e.path())
		.filter(|p| {
			p.is_file()
				&& p.file_name().is_some_and(|n| {
					n.to_string_lossy()
						.to_ascii_lowercase()
						.ends_with("gamelist.txt")
				})
		})
		.collect();
	found.sort();
	found.into_iter().next().ok_or(CatalogError::NoGameList)
}

pub enum SyncStage {
	Downloading { done: u64, total: Option<u64> },
	Extracting { done: u64, total: u64 },
}

/// Downloads and unpacks the mirror's `meta.7z`, then swaps it in for the
/// previous copy. The old catalog stays usable if anything fails.
pub async fn sync(
	fetcher: &Fetcher,
	mirror: &Mirror,
	root: &Path,
	stage: &(dyn Fn(SyncStage) + Send + Sync),
) -> Result<Catalog, CatalogError> {
	let password = mirror.archive_password()?;
	let mirror_dir = root.join(&mirror.id);
	let archive = mirror_dir.join("meta.7z");
	let progress: FetchProgress<'_> = &|done, total| stage(SyncStage::Downloading { done, total });
	fetcher
		.download_file(mirror, "meta.7z", &archive, progress)
		.await?;

	let staging = mirror_dir.join("staging");
	let current = mirror_dir.join("current");
	let previous = mirror_dir.join("previous");
	let mirror_id = mirror.id.clone();
	let root = root.to_path_buf();

	let unpack = move || -> Result<Catalog, CatalogError> {
		remove_dir_if_exists(&staging)?;
		let never = AtomicBool::new(false);
		extract_7z(
			std::slice::from_ref(&archive),
			&staging,
			Some(&password),
			&|done, total| stage(SyncStage::Extracting { done, total }),
			&never,
		)?;
		find_game_list(&staging)?;

		remove_dir_if_exists(&previous)?;
		if current.exists() {
			std::fs::rename(&current, &previous).map_err(io_err(&current))?;
		}
		std::fs::rename(&staging, &current).map_err(io_err(&staging))?;
		remove_dir_if_exists(&previous)?;
		let _ = std::fs::remove_file(&archive);
		Catalog::load(&root, &mirror_id)?.ok_or(CatalogError::NoGameList)
	};
	// Borrowed `stage` can't cross into spawn_blocking, so run on this thread.
	tokio::task::block_in_place(unpack)
}

fn remove_dir_if_exists(path: &Path) -> Result<(), CatalogError> {
	match std::fs::remove_dir_all(path) {
		Ok(()) => Ok(()),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(e) => Err(io_err(path)(e)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_meta(dir: &Path) {
		std::fs::create_dir_all(dir.join(".meta/thumbnails")).unwrap();
		std::fs::create_dir_all(dir.join(".meta/notes")).unwrap();
		std::fs::write(
			dir.join("VRP-GameList.txt"),
			"Game Name;Release Name;Package Name;Version Code\nA;A v1;com.a.game;5\n",
		)
		.unwrap();
		std::fs::write(dir.join(".meta/thumbnails/com.a.game.jpg"), b"jpg").unwrap();
		std::fs::write(dir.join(".meta/notes/A v1.txt"), "  Needs hand tracking.\n").unwrap();
		std::fs::create_dir_all(dir.join(".meta/nouns")).unwrap();
		std::fs::write(
			dir.join(".meta/nouns/blacklist.txt"),
			"com.discord\r\n\r\ncom.x.y\n",
		)
		.unwrap();
		std::fs::write(
			dir.join(".meta/upload.config"),
			"[RSL-gameuploads]\ntype = ftp\n",
		)
		.unwrap();
	}

	#[test]
	fn loads_cached_catalog() {
		let root = tempfile::tempdir().unwrap();
		assert!(Catalog::load(root.path(), "m1").unwrap().is_none());

		write_meta(&root.path().join("m1/current"));
		let catalog = Catalog::load(root.path(), "m1").unwrap().unwrap();
		assert_eq!(catalog.games.len(), 1);
		assert!(
			catalog
				.thumbnail("com.a.game")
				.unwrap()
				.ends_with(".meta/thumbnails/com.a.game.jpg")
		);
		assert!(catalog.thumbnail("com.other").is_none());
		assert_eq!(
			catalog.note("A v1").as_deref(),
			Some("Needs hand tracking.")
		);
		assert_eq!(catalog.note("missing"), None);
		assert_eq!(catalog.note("../../etc/passwd"), None);
		assert!(catalog.is_blacklisted("com.discord"));
		assert!(catalog.is_blacklisted("com.x.y"));
		assert!(!catalog.is_blacklisted("com.a.game"));
		assert!(catalog.upload_config().is_some());
	}

	fn packed_meta(dir: &Path, password: &str) -> Vec<u8> {
		let src = dir.join("meta-src");
		write_meta(&src);
		let out = dir.join("packed.7z");
		sevenz_rust2::compress_to_path_encrypted(&src, &out, password.into()).unwrap();
		std::fs::read(out).unwrap()
	}

	async fn serve_once(body: Vec<u8>) -> String {
		use tokio::io::{AsyncReadExt, AsyncWriteExt};
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			let mut buf = vec![0u8; 4096];
			let _ = s.read(&mut buf).await;
			let head = format!(
				"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
				body.len()
			);
			s.write_all(head.as_bytes()).await.unwrap();
			s.write_all(&body).await.unwrap();
		});
		format!("http://{addr}/")
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn sync_over_http_replaces_previous_catalog() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join("catalog");
		std::fs::create_dir_all(root.join("m1/current")).unwrap();
		std::fs::write(root.join("m1/current/old-marker"), b"").unwrap();

		let base = serve_once(packed_meta(dir.path(), "hunter2")).await;
		let mirror = Mirror {
			id: "m1".into(),
			name: "t".into(),
			kind: super::super::MirrorKind::Http { base_uri: base },
			password: "aHVudGVyMg==".into(),
			headers: Vec::new(),
		};
		let catalog = sync(&Fetcher::new(None).unwrap(), &mirror, &root, &|_| {})
			.await
			.unwrap();
		assert_eq!(catalog.games[0].package, "com.a.game");
		assert!(catalog.thumbnail("com.a.game").is_some());
		assert!(!root.join("m1/current/old-marker").exists());
		assert!(!root.join("m1/meta.7z").exists());
		assert!(!root.join("m1/staging").exists());
	}

	#[tokio::test(flavor = "multi_thread")]
	async fn failed_sync_keeps_old_catalog() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join("catalog");
		write_meta(&root.join("m1/current"));
		let base = serve_once(packed_meta(dir.path(), "right")).await;
		let mirror = Mirror {
			id: "m1".into(),
			name: "t".into(),
			kind: super::super::MirrorKind::Http { base_uri: base },
			password: "d3Jvbmc=".into(),
			headers: Vec::new(),
		};
		let err = sync(&Fetcher::new(None).unwrap(), &mirror, &root, &|_| {})
			.await
			.unwrap_err();
		assert!(matches!(err, CatalogError::Archive(_)), "{err:?}");
		assert!(Catalog::load(&root, "m1").unwrap().is_some());
	}

	/// Uses the real rclone binary with a local alias remote; skipped without rclone.
	#[tokio::test(flavor = "multi_thread")]
	async fn sync_through_rclone() {
		if std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_err()
		{
			eprintln!("rclone not installed, skipping");
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let served = dir.path().join("server");
		std::fs::create_dir_all(served.join("Quest Games")).unwrap();
		std::fs::write(
			served.join("Quest Games/meta.7z"),
			packed_meta(dir.path(), "hunter2"),
		)
		.unwrap();
		let config = dir.path().join("rclone.conf");
		std::fs::write(
			&config,
			format!("[vrp]\ntype = alias\nremote = {}\n", served.display()),
		)
		.unwrap();

		let mirror = Mirror {
			id: "r1".into(),
			name: "r".into(),
			kind: super::super::MirrorKind::Rclone {
				config_file: config,
				remote: "vrp".into(),
			},
			password: "aHVudGVyMg==".into(),
			headers: Vec::new(),
		};
		let root = dir.path().join("catalog");
		let catalog = sync(&Fetcher::new(None).unwrap(), &mirror, &root, &|_| {})
			.await
			.unwrap();
		assert_eq!(catalog.games.len(), 1);
	}
}
