//! File transfer over the `sync:` service. Each request is a 4-byte id plus a
//! little-endian u32; `SEND` is followed by `DATA` chunks and finished by `DONE`.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::AdbError;

const MAX_CHUNK: usize = 64 * 1024;
/// A single write or read stalling this long means the transfer is dead.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(120);
const REGULAR_FILE_0644: u32 = 0o100_644;

/// Receives the number of bytes sent since the previous call.
pub type Progress<'a> = &'a (dyn Fn(u64) + Send + Sync);

pub(crate) async fn timed<T>(
	fut: impl Future<Output = Result<T, AdbError>>,
) -> Result<T, AdbError> {
	tokio::time::timeout(IO_TIMEOUT, fut)
		.await
		.map_err(|_| AdbError::Timeout)?
}

pub(crate) struct SyncSession<S> {
	stream: S,
	buf: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SyncSession<S> {
	pub fn new(stream: S) -> Self {
		Self {
			stream,
			buf: vec![0u8; MAX_CHUNK],
		}
	}

	async fn write_header(&mut self, id: &[u8; 4], len: u32) -> Result<(), AdbError> {
		let mut header = [0u8; 8];
		header[..4].copy_from_slice(id);
		header[4..].copy_from_slice(&len.to_le_bytes());
		timed(async { Ok(self.stream.write_all(&header).await?) }).await
	}

	pub async fn send_file(
		&mut self,
		local: &Path,
		remote: &str,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let local_err = |source| AdbError::LocalIo {
			path: local.to_path_buf(),
			source,
		};
		let mut file = tokio::fs::File::open(local).await.map_err(local_err)?;
		let mtime = file
			.metadata()
			.await
			.map_err(local_err)?
			.modified()
			.ok()
			.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
			.map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX));

		let target = format!("{remote},{REGULAR_FILE_0644}");
		let target_len = u32::try_from(target.len())
			.map_err(|_| AdbError::Protocol("remote path too long".into()))?;
		self.write_header(b"SEND", target_len).await?;
		timed(async { Ok(self.stream.write_all(target.as_bytes()).await?) }).await?;

		loop {
			let n = file.read(&mut self.buf).await.map_err(local_err)?;
			if n == 0 {
				break;
			}
			self.write_header(b"DATA", n as u32).await?;
			let chunk = &self.buf[..n];
			timed(async { Ok(self.stream.write_all(chunk).await?) }).await?;
			progress(n as u64);
		}
		self.write_header(b"DONE", mtime).await?;
		timed(async { Ok(self.stream.flush().await?) }).await?;
		self.read_send_result(remote).await
	}

	async fn read_send_result(&mut self, remote: &str) -> Result<(), AdbError> {
		let mut header = [0u8; 8];
		timed(async { Ok(self.stream.read_exact(&mut header).await?) }).await?;
		let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
		match &header[..4] {
			b"OKAY" => Ok(()),
			b"FAIL" => {
				let mut message = vec![0u8; len];
				timed(async { Ok(self.stream.read_exact(&mut message).await?) }).await?;
				Err(AdbError::PushFailed {
					remote: remote.to_owned(),
					message: String::from_utf8_lossy(&message).into_owned(),
				})
			}
			other => Err(AdbError::Protocol(format!(
				"unexpected sync reply {:?}",
				String::from_utf8_lossy(other)
			))),
		}
	}

	/// Copies a device file to `local` (written to `local.part`, then renamed).
	pub async fn recv_file(
		&mut self,
		remote: &str,
		local: &Path,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let local_err = |source| AdbError::LocalIo {
			path: local.to_path_buf(),
			source,
		};
		let len = u32::try_from(remote.len())
			.map_err(|_| AdbError::Protocol("remote path too long".into()))?;
		self.write_header(b"RECV", len).await?;
		timed(async { Ok(self.stream.write_all(remote.as_bytes()).await?) }).await?;
		timed(async { Ok(self.stream.flush().await?) }).await?;

		if let Some(parent) = local.parent() {
			tokio::fs::create_dir_all(parent).await.map_err(local_err)?;
		}
		let mut part_name = local.file_name().unwrap_or_default().to_os_string();
		part_name.push(".part");
		let part = local.with_file_name(part_name);
		let mut file = tokio::fs::File::create(&part).await.map_err(local_err)?;
		loop {
			let mut header = [0u8; 8];
			timed(async { Ok(self.stream.read_exact(&mut header).await?) }).await?;
			let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
			match &header[..4] {
				b"DATA" => {
					if len > MAX_CHUNK {
						return Err(AdbError::Protocol(format!("sync chunk of {len} bytes")));
					}
					let chunk = &mut self.buf[..len];
					timed(async { Ok(self.stream.read_exact(chunk).await?) }).await?;
					file.write_all(&self.buf[..len]).await.map_err(local_err)?;
					progress(len as u64);
				}
				b"DONE" => break,
				b"FAIL" => {
					let mut message = vec![0u8; len];
					timed(async { Ok(self.stream.read_exact(&mut message).await?) }).await?;
					drop(file);
					let _ = tokio::fs::remove_file(&part).await;
					return Err(AdbError::PullFailed {
						remote: remote.to_owned(),
						message: String::from_utf8_lossy(&message).into_owned(),
					});
				}
				other => {
					return Err(AdbError::Protocol(format!(
						"unexpected sync reply {:?}",
						String::from_utf8_lossy(other)
					)));
				}
			}
		}
		file.flush().await.map_err(local_err)?;
		drop(file);
		tokio::fs::rename(&part, local).await.map_err(local_err)
	}

	pub async fn quit(mut self) -> Result<(), AdbError> {
		self.write_header(b"QUIT", 0).await
	}
}

/// A local file to transfer and its path relative to the pushed directory,
/// always with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
	pub path: PathBuf,
	pub relative: String,
	pub size: u64,
}

/// Lists regular files under `root`, sorted by relative path. Symlinks are
/// skipped so a release can't point the transfer outside its own folder.
pub fn list_files(root: &Path) -> std::io::Result<Vec<LocalFile>> {
	let mut files = Vec::new();
	let mut stack = vec![root.to_path_buf()];
	while let Some(dir) = stack.pop() {
		for entry in std::fs::read_dir(&dir)? {
			let entry = entry?;
			let file_type = entry.file_type()?;
			let path = entry.path();
			if file_type.is_dir() {
				stack.push(path);
			} else if file_type.is_file() {
				let relative = path
					.strip_prefix(root)
					.expect("walked path is under root")
					.components()
					.map(|c| c.as_os_str().to_string_lossy())
					.collect::<Vec<_>>()
					.join("/");
				let size = entry.metadata()?.len();
				files.push(LocalFile {
					path,
					relative,
					size,
				});
			}
		}
	}
	files.sort_by(|a, b| a.relative.cmp(&b.relative));
	Ok(files)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lists_nested_files_with_unix_paths() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
		std::fs::write(dir.path().join("top.obb"), b"12345").unwrap();
		std::fs::write(dir.path().join("a/b/deep.bin"), b"x").unwrap();
		std::fs::create_dir(dir.path().join("empty")).unwrap();

		let files = list_files(dir.path()).unwrap();
		let names: Vec<_> = files
			.iter()
			.map(|f| (f.relative.as_str(), f.size))
			.collect();
		assert_eq!(names, [("a/b/deep.bin", 1), ("top.obb", 5)]);
	}

	#[cfg(unix)]
	#[test]
	fn skips_symlinks() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("real"), b"x").unwrap();
		std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
		let files = list_files(dir.path()).unwrap();
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].relative, "real");
	}
}
