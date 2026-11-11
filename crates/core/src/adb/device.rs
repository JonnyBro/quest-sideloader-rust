use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::AdbError;
use super::parse::{DeviceInfo, Package, parse_battery, parse_df, parse_packages};
use super::protocol::{read_shell_packet, send_request_checked, shell_v2};
use super::server::{AdbServer, REQUEST_TIMEOUT};
use super::sync::{IO_TIMEOUT, Progress, SyncSession, list_files, timed};

/// Package manager may spend minutes optimizing a large APK after receiving it.
const INSTALL_RESULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct Device {
	server: AdbServer,
	serial: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
	pub stdout: String,
	pub stderr: String,
	pub exit_code: Option<u8>,
}

impl ShellOutput {
	pub fn success(&self) -> bool {
		self.exit_code == Some(0)
	}
}

impl Device {
	pub(crate) fn new(server: AdbServer, serial: String) -> Self {
		Self { server, serial }
	}

	pub fn serial(&self) -> &str {
		&self.serial
	}

	async fn open(&self, service: &str) -> Result<TcpStream, AdbError> {
		let mut stream = self.server.connect().await?;
		send_request_checked(&mut stream, &format!("host:transport:{}", self.serial)).await?;
		send_request_checked(&mut stream, service).await?;
		Ok(stream)
	}

	/// Runs `command` through `sh -c` on the device using shell protocol v2,
	/// which keeps stdout and stderr apart and reports the exit code.
	pub async fn shell(&self, command: &str) -> Result<ShellOutput, AdbError> {
		self.shell_with_timeout(command, REQUEST_TIMEOUT).await
	}

	pub async fn shell_with_timeout(
		&self,
		command: &str,
		timeout: Duration,
	) -> Result<ShellOutput, AdbError> {
		let fut = async {
			let mut stream = self.open(&format!("shell,v2,raw:{command}")).await?;
			let mut stdout = Vec::new();
			let mut stderr = Vec::new();
			let mut exit_code = None;
			while let Some(packet) = read_shell_packet(&mut stream).await? {
				match packet.id {
					shell_v2::STDOUT => stdout.extend_from_slice(&packet.data),
					shell_v2::STDERR => stderr.extend_from_slice(&packet.data),
					shell_v2::EXIT => {
						exit_code = packet.data.first().copied();
						break;
					}
					_ => {}
				}
			}
			Ok(ShellOutput {
				stdout: String::from_utf8_lossy(&stdout).into_owned(),
				stderr: String::from_utf8_lossy(&stderr).into_owned(),
				exit_code,
			})
		};
		tokio::time::timeout(timeout, fut)
			.await
			.map_err(|_| AdbError::Timeout)?
	}

	async fn shell_ok(&self, command: &str) -> Result<String, AdbError> {
		let out = self.shell(command).await?;
		if out.success() {
			Ok(out.stdout)
		} else {
			let detail = if out.stderr.trim().is_empty() {
				out.stdout.trim()
			} else {
				out.stderr.trim()
			};
			Err(AdbError::Server(format!("`{command}` failed: {detail}")))
		}
	}

	async fn getprop(&self, name: &str) -> Result<String, AdbError> {
		Ok(self
			.shell_ok(&format!("getprop {name}"))
			.await?
			.trim()
			.to_owned())
	}

	pub async fn third_party_packages(&self) -> Result<Vec<Package>, AdbError> {
		Ok(parse_packages(
			&self
				.shell_ok("pm list packages -3 --show-versioncode")
				.await?,
		))
	}

	/// Free bytes on the filesystem holding `path`.
	pub async fn free_space(&self, path: &str) -> Result<u64, AdbError> {
		let out = self
			.shell_ok(&format!("df -k {}", shell_words::quote(path)))
			.await?;
		parse_df(&out)
			.map(|s| s.free_bytes)
			.ok_or_else(|| AdbError::Protocol(format!("unexpected df output: {}", out.trim())))
	}

	pub async fn is_dir(&self, remote: &str) -> Result<bool, AdbError> {
		let out = self
			.shell(&format!("test -d {}", shell_words::quote(remote)))
			.await?;
		Ok(out.success())
	}

	/// Pushes a file, or every file of a directory, to `remote`. Missing remote
	/// directories are created by adbd.
	pub async fn push(
		&self,
		local: &Path,
		remote: &str,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let remote = remote.trim_end_matches('/');
		let metadata = tokio::fs::metadata(local)
			.await
			.map_err(|source| AdbError::LocalIo {
				path: local.to_path_buf(),
				source,
			})?;
		let files = if metadata.is_dir() {
			let root = local.to_path_buf();
			let files = tokio::task::spawn_blocking(move || list_files(&root))
				.await
				.map_err(|e| AdbError::Protocol(format!("file listing task failed: {e}")))?
				.map_err(|source| AdbError::LocalIo {
					path: local.to_path_buf(),
					source,
				})?;
			files
				.into_iter()
				.map(|f| (f.path, format!("{remote}/{}", f.relative)))
				.collect()
		} else {
			vec![(local.to_path_buf(), remote.to_owned())]
		};

		let stream = timed(self.open("sync:")).await?;
		let mut session = SyncSession::new(stream);
		for (path, target) in &files {
			session.send_file(path, target, progress).await?;
		}
		session.quit().await
	}

	/// Streams the APK straight into the package manager (`-S <size>`), so no
	/// temporary copy is left on the device. Replaces an existing install and
	/// grants runtime permissions, as sideloaded games expect.
	pub async fn install_apk(&self, apk: &Path, progress: Progress<'_>) -> Result<(), AdbError> {
		let local_err = |source| AdbError::LocalIo {
			path: apk.to_path_buf(),
			source,
		};
		let mut file = tokio::fs::File::open(apk).await.map_err(local_err)?;
		let size = file.metadata().await.map_err(local_err)?.len();
		let mut stream =
			timed(self.open(&format!("exec:cmd package install -r -g -S {size}"))).await?;

		let mut buf = vec![0u8; 64 * 1024];
		loop {
			let n = file.read(&mut buf).await.map_err(local_err)?;
			if n == 0 {
				break;
			}
			let chunk = &buf[..n];
			tokio::time::timeout(IO_TIMEOUT, stream.write_all(chunk))
				.await
				.map_err(|_| AdbError::Timeout)??;
			progress(n as u64);
		}
		stream.flush().await?;

		let mut output = String::new();
		tokio::time::timeout(INSTALL_RESULT_TIMEOUT, stream.read_to_string(&mut output))
			.await
			.map_err(|_| AdbError::Timeout)??;
		let output = output.trim();
		if output.lines().any(|l| l.trim() == "Success") {
			Ok(())
		} else if output.is_empty() {
			Err(AdbError::InstallFailed(
				"no response from package manager".into(),
			))
		} else {
			Err(AdbError::InstallFailed(output.to_owned()))
		}
	}

	pub async fn info(&self) -> Result<DeviceInfo, AdbError> {
		let (model, android_version, battery, storage, packages) = tokio::try_join!(
			self.getprop("ro.product.model"),
			self.getprop("ro.build.version.release"),
			self.shell_ok("dumpsys battery"),
			self.shell_ok("df -k /sdcard"),
			self.third_party_packages(),
		)?;
		Ok(DeviceInfo {
			model,
			android_version,
			battery: parse_battery(&battery),
			storage: parse_df(&storage),
			packages,
		})
	}
}

#[cfg(test)]
mod tests {
	use std::net::SocketAddr;

	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	use tokio::net::TcpListener;

	use super::*;
	use crate::adb::testing::*;

	/// Fake adb server that answers one shell request.
	async fn fake_server(
		expect_serial: &'static str,
		reply: Vec<u8>,
	) -> (SocketAddr, tokio::task::JoinHandle<String>) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let handle = tokio::spawn(async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			assert_eq!(
				read_request(&mut stream).await,
				format!("host:transport:{expect_serial}")
			);
			stream.write_all(b"OKAY").await.unwrap();
			let service = read_request(&mut stream).await;
			stream.write_all(b"OKAY").await.unwrap();
			stream.write_all(&reply).await.unwrap();
			service
		});
		(addr, handle)
	}

	#[tokio::test]
	async fn shell_v2_collects_output_and_exit_code() {
		let mut reply = packet(shell_v2::STDOUT, b"Quest 3\n");
		reply.extend(packet(shell_v2::STDERR, b"warn"));
		reply.extend(packet(shell_v2::EXIT, &[0]));
		let (addr, server) = fake_server("SER1", reply).await;

		let device = AdbServer::with_addr(addr).device("SER1");
		let out = device.shell("getprop ro.product.model").await.unwrap();
		assert_eq!(
			server.await.unwrap(),
			"shell,v2,raw:getprop ro.product.model"
		);
		assert_eq!(out.stdout, "Quest 3\n");
		assert_eq!(out.stderr, "warn");
		assert!(out.success());
	}

	#[tokio::test]
	async fn failing_command_is_an_error() {
		let mut reply = packet(shell_v2::STDERR, b"pm: not found");
		reply.extend(packet(shell_v2::EXIT, &[127]));
		let (addr, _server) = fake_server("SER1", reply).await;

		let device = AdbServer::with_addr(addr).device("SER1");
		let err = device.third_party_packages().await.unwrap_err();
		assert!(matches!(err, AdbError::Server(m) if m.contains("pm: not found")));
	}

	/// Accepts one connection, checks the transport and service, then hands the
	/// socket to `handler`.
	async fn fake_device<F, Fut>(
		service: String,
		handler: F,
	) -> (SocketAddr, tokio::task::JoinHandle<()>)
	where
		F: FnOnce(TcpStream) -> Fut + Send + 'static,
		Fut: std::future::Future<Output = ()> + Send,
	{
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let handle = tokio::spawn(async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			assert_eq!(read_request(&mut stream).await, "host:transport:SER1");
			stream.write_all(b"OKAY").await.unwrap();
			assert_eq!(read_request(&mut stream).await, service);
			stream.write_all(b"OKAY").await.unwrap();
			handler(stream).await;
		});
		(addr, handle)
	}

	#[tokio::test]
	async fn push_directory_sends_every_file() {
		let dir = tempfile::tempdir().unwrap();
		let obb = dir.path().join("com.example.game");
		std::fs::create_dir_all(obb.join("sub")).unwrap();
		let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
		std::fs::write(obb.join("main.1.com.example.game.obb"), &big).unwrap();
		std::fs::write(obb.join("sub/extra.bin"), b"extra").unwrap();

		let (tx, rx) = tokio::sync::oneshot::channel();
		let (addr, server) = fake_device("sync:".into(), |mut s| async move {
			let _ = tx.send(receive_files(&mut s).await);
		})
		.await;

		let sent = std::sync::atomic::AtomicU64::new(0);
		let progress = |n: u64| {
			sent.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
		};
		AdbServer::with_addr(addr)
			.device("SER1")
			.push(&obb, "/sdcard/Android/obb/com.example.game/", &progress)
			.await
			.unwrap();
		server.await.unwrap();

		let files = rx.await.unwrap();
		assert_eq!(files.len(), 2);
		assert_eq!(
			files[0].0,
			"/sdcard/Android/obb/com.example.game/main.1.com.example.game.obb"
		);
		assert_eq!(files[0].1, "33188");
		assert_eq!(files[0].2, big);
		assert_eq!(
			files[1].0,
			"/sdcard/Android/obb/com.example.game/sub/extra.bin"
		);
		assert_eq!(files[1].2, b"extra");
		assert_eq!(sent.into_inner(), big.len() as u64 + 5);
	}

	#[tokio::test]
	async fn push_reports_device_failure() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join("a.txt");
		std::fs::write(&file, b"hi").unwrap();

		let (addr, _server) = fake_device("sync:".into(), |mut s| async move {
			let (id, len) = read_sync_header(&mut s).await;
			assert_eq!(&id, b"SEND");
			let mut target = vec![0u8; len as usize];
			s.read_exact(&mut target).await.unwrap();
			loop {
				let (id, len) = read_sync_header(&mut s).await;
				if &id == b"DONE" {
					break;
				}
				let mut skip = vec![0u8; len as usize];
				s.read_exact(&mut skip).await.unwrap();
			}
			let message = b"Permission denied";
			s.write_all(b"FAIL").await.unwrap();
			s.write_all(&(message.len() as u32).to_le_bytes())
				.await
				.unwrap();
			s.write_all(message).await.unwrap();
		})
		.await;

		let err = AdbServer::with_addr(addr)
			.device("SER1")
			.push(&file, "/system/a.txt", &|_| {})
			.await
			.unwrap_err();
		assert!(
			matches!(err, AdbError::PushFailed { remote, message } if remote == "/system/a.txt" && message == "Permission denied")
		);
	}

	async fn fake_install(
		apk_len: usize,
		reply: &'static str,
	) -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
		let (tx, rx) = tokio::sync::oneshot::channel();
		let (addr, _server) = fake_device(
			format!("exec:cmd package install -r -g -S {apk_len}"),
			move |mut s| async move {
				let mut body = vec![0u8; apk_len];
				s.read_exact(&mut body).await.unwrap();
				s.write_all(reply.as_bytes()).await.unwrap();
				let _ = tx.send(body);
			},
		)
		.await;
		(addr, rx)
	}

	#[tokio::test]
	async fn install_streams_apk() {
		let dir = tempfile::tempdir().unwrap();
		let apk = dir.path().join("game.apk");
		let content: Vec<u8> = (0..150_000u32).map(|i| (i % 7) as u8).collect();
		std::fs::write(&apk, &content).unwrap();

		let (addr, received) = fake_install(content.len(), "Success\n").await;
		AdbServer::with_addr(addr)
			.device("SER1")
			.install_apk(&apk, &|_| {})
			.await
			.unwrap();
		assert_eq!(received.await.unwrap(), content);
	}

	#[tokio::test]
	async fn install_failure_carries_reason() {
		let dir = tempfile::tempdir().unwrap();
		let apk = dir.path().join("game.apk");
		std::fs::write(&apk, b"not really an apk").unwrap();

		let (addr, _received) = fake_install(
			17,
			"Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match]\n",
		)
		.await;
		let err = AdbServer::with_addr(addr)
			.device("SER1")
			.install_apk(&apk, &|_| {})
			.await
			.unwrap_err();
		assert!(
			matches!(err, AdbError::InstallFailed(m) if m.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE"))
		);
	}
}
