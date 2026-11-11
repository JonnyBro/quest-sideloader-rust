//! 7z extraction for mirror metadata and game releases.

mod volumes;

use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sevenz_rust2::{ArchiveReader, Password};

pub use volumes::{MultiVolumeReader, find_volumes};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
	#[error("wrong archive password")]
	WrongPassword,

	#[error("archive is damaged or unsupported: {0}")]
	Corrupt(String),

	#[error("archive entry {0:?} points outside the destination folder")]
	UnsafePath(String),

	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("extraction was cancelled")]
	Cancelled,
}

fn convert(e: sevenz_rust2::Error) -> ArchiveError {
	use sevenz_rust2::Error as E;
	match e {
		E::PasswordRequired | E::MaybeBadPassword(_) => ArchiveError::WrongPassword,
		other => ArchiveError::Corrupt(other.to_string()),
	}
}

/// Maps an entry name to a path under `dest`, refusing anything that could
/// escape it (`..`, absolute paths, drive letters).
fn safe_join(dest: &Path, name: &str) -> Result<PathBuf, ArchiveError> {
	let normalized = name.replace('\\', "/");
	let relative = Path::new(&normalized);
	let safe = !normalized.contains(':')
		&& relative
			.components()
			.all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
	if !safe || normalized.trim_matches('/').is_empty() {
		return Err(ArchiveError::UnsafePath(name.to_owned()));
	}
	Ok(dest.join(relative))
}

/// Extracts all volumes of one archive into `dest`. `progress` receives
/// `(unpacked bytes written, total unpacked size)`. Setting `cancel` stops at
/// the next chunk.
pub fn extract_7z(
	volumes: &[PathBuf],
	dest: &Path,
	password: Option<&str>,
	progress: &dyn Fn(u64, u64),
	cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
	let first = volumes.first().cloned().unwrap_or_default();
	let reader = MultiVolumeReader::open(volumes).map_err(|source| ArchiveError::Io {
		path: first.clone(),
		source,
	})?;
	let password = password.map_or_else(Password::empty, Password::from);
	let mut archive = ArchiveReader::new(reader, password).map_err(convert)?;
	if let Ok(threads) = std::thread::available_parallelism() {
		archive.set_thread_count(u32::try_from(threads.get()).unwrap_or(1));
	}

	let total: u64 = archive.archive().files.iter().map(|f| f.size).sum();
	let mut done = 0u64;
	let mut failure: Option<ArchiveError> = None;
	let mut buf = vec![0u8; 256 * 1024];

	let result = archive.for_each_entries(|entry, data| {
		if entry.is_anti_item() {
			return Ok(true);
		}
		let path = match safe_join(dest, entry.name()) {
			Ok(path) => path,
			Err(e) => {
				failure = Some(e);
				return Ok(false);
			}
		};
		let io_fail = |failure: &mut Option<ArchiveError>, path: &Path, source| {
			*failure = Some(ArchiveError::Io {
				path: path.to_path_buf(),
				source,
			});
		};
		if entry.is_directory() {
			if let Err(e) = std::fs::create_dir_all(&path) {
				io_fail(&mut failure, &path, e);
				return Ok(false);
			}
			return Ok(true);
		}
		if let Some(parent) = path.parent()
			&& let Err(e) = std::fs::create_dir_all(parent)
		{
			io_fail(&mut failure, parent, e);
			return Ok(false);
		}
		let file = match std::fs::File::create(&path) {
			Ok(file) => file,
			Err(e) => {
				io_fail(&mut failure, &path, e);
				return Ok(false);
			}
		};
		let mut out = BufWriter::new(file);
		loop {
			if cancel.load(Ordering::Relaxed) {
				failure = Some(ArchiveError::Cancelled);
				return Ok(false);
			}
			let n = data.read(&mut buf)?;
			if n == 0 {
				break;
			}
			if let Err(e) = out.write_all(&buf[..n]) {
				io_fail(&mut failure, &path, e);
				return Ok(false);
			}
			done += n as u64;
			progress(done, total);
		}
		if let Err(e) = out.flush() {
			io_fail(&mut failure, &path, e);
			return Ok(false);
		}
		Ok(true)
	});

	if let Some(failure) = failure {
		return Err(failure);
	}
	result.map_err(convert)
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::AtomicU64;

	use super::*;

	fn make_archive(dir: &Path, password: Option<&str>) -> PathBuf {
		let src = dir.join("src");
		std::fs::create_dir_all(src.join(".meta/thumbnails")).unwrap();
		std::fs::write(
			src.join("VRP-GameList.txt"),
			"Game Name;Release Name\nA;A v1\n",
		)
		.unwrap();
		std::fs::write(src.join(".meta/thumbnails/com.a.b.jpg"), vec![7u8; 50_000]).unwrap();
		let out = dir.join("meta.7z");
		match password {
			Some(p) => {
				sevenz_rust2::compress_to_path_encrypted(&src, &out, Password::from(p)).unwrap()
			}
			None => sevenz_rust2::compress_to_path(&src, &out).unwrap(),
		}
		out
	}

	#[test]
	fn extracts_encrypted_archive_with_progress() {
		let dir = tempfile::tempdir().unwrap();
		let archive = make_archive(dir.path(), Some("s3cret"));
		let dest = dir.path().join("out");
		let last = AtomicU64::new(0);
		extract_7z(
			&[archive],
			&dest,
			Some("s3cret"),
			&|done, total| {
				assert!(done <= total);
				last.store(done, Ordering::Relaxed);
			},
			&AtomicBool::new(false),
		)
		.unwrap();
		assert!(
			std::fs::read_to_string(dest.join("VRP-GameList.txt"))
				.unwrap()
				.starts_with("Game Name")
		);
		assert_eq!(
			std::fs::read(dest.join(".meta/thumbnails/com.a.b.jpg"))
				.unwrap()
				.len(),
			50_000
		);
		assert!(last.into_inner() >= 50_000);
	}

	#[test]
	fn wrong_password_is_reported() {
		let dir = tempfile::tempdir().unwrap();
		let archive = make_archive(dir.path(), Some("right"));
		let err = extract_7z(
			&[archive],
			&dir.path().join("out"),
			Some("wrong"),
			&|_, _| {},
			&AtomicBool::new(false),
		)
		.unwrap_err();
		assert!(matches!(err, ArchiveError::WrongPassword), "{err:?}");
	}

	#[test]
	fn split_archive_extracts() {
		let dir = tempfile::tempdir().unwrap();
		let archive = make_archive(dir.path(), None);
		let bytes = std::fs::read(&archive).unwrap();
		let mut volumes = Vec::new();
		for (i, chunk) in bytes.chunks(bytes.len() / 3 + 1).enumerate() {
			let path = dir.path().join(format!("game.7z.{:03}", i + 1));
			std::fs::write(&path, chunk).unwrap();
			volumes.push(path);
		}
		assert_eq!(find_volumes(&volumes[0]).unwrap(), volumes);
		let dest = dir.path().join("out");
		extract_7z(&volumes, &dest, None, &|_, _| {}, &AtomicBool::new(false)).unwrap();
		assert!(dest.join("VRP-GameList.txt").is_file());
	}

	#[test]
	fn cancel_stops_extraction() {
		let dir = tempfile::tempdir().unwrap();
		let archive = make_archive(dir.path(), None);
		let err = extract_7z(
			&[archive],
			&dir.path().join("out"),
			None,
			&|_, _| {},
			&AtomicBool::new(true),
		)
		.unwrap_err();
		assert!(matches!(err, ArchiveError::Cancelled));
	}

	#[test]
	fn unsafe_names_are_rejected() {
		let dest = Path::new("/out");
		for bad in [
			"../evil",
			"/etc/passwd",
			"a/../../b",
			"C:/Windows/x",
			"..\\x",
			"",
		] {
			assert!(safe_join(dest, bad).is_err(), "{bad}");
		}
		assert_eq!(
			safe_join(dest, "a\\b/c.txt").unwrap(),
			Path::new("/out/a/b/c.txt")
		);
	}

	/// Archives made by real 7-Zip, the way mirrors pack them: LZMA2, AES with
	/// encrypted headers, split into volumes. Skipped when `7z` isn't installed.
	#[test]
	fn real_7zip_split_archive_with_encrypted_headers() {
		let dir = tempfile::tempdir().unwrap();
		let src = dir.path().join("Game v1");
		std::fs::create_dir_all(src.join("com.example.game")).unwrap();
		let mut x = 0x2545_f491_4f6c_dd1du64;
		let obb: Vec<u8> = (0..3_000_000)
			.map(|_| {
				x ^= x << 13;
				x ^= x >> 7;
				x ^= x << 17;
				(x >> 32) as u8
			})
			.collect();
		std::fs::write(src.join("com.example.game/main.obb"), &obb).unwrap();
		std::fs::write(src.join("install.txt"), "adb install game.apk\n").unwrap();

		let archive = dir.path().join("hash.7z");
		let status = match std::process::Command::new("7z")
			.args([
				"a",
				"-t7z",
				"-m0=lzma2",
				"-mx=1",
				"-mhe=on",
				"-pVRpass",
				"-v1m",
			])
			.arg(&archive)
			.arg(&src)
			.stdout(std::process::Stdio::null())
			.status()
		{
			Ok(status) => status,
			Err(_) => {
				eprintln!("7z not installed, skipping");
				return;
			}
		};
		assert!(status.success());

		let volumes = find_volumes(&dir.path().join("hash.7z.001")).unwrap();
		assert!(
			volumes.len() >= 2,
			"expected a split archive, got {volumes:?}"
		);
		let dest = dir.path().join("out");
		extract_7z(
			&volumes,
			&dest,
			Some("VRpass"),
			&|_, _| {},
			&AtomicBool::new(false),
		)
		.unwrap();
		assert_eq!(
			std::fs::read(dest.join("Game v1/com.example.game/main.obb")).unwrap(),
			obb
		);

		let err = extract_7z(
			&volumes,
			&dir.path().join("bad"),
			Some("nope"),
			&|_, _| {},
			&AtomicBool::new(false),
		)
		.unwrap_err();
		assert!(
			matches!(err, ArchiveError::WrongPassword | ArchiveError::Corrupt(_)),
			"{err:?}"
		);
	}
}
