use rust_i18n::t;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::AdbError;
use super::parse::{
	DeviceInfo, Package, parse_battery, parse_df, parse_packages, parse_stat_times,
};
use super::protocol::{read_shell_packet, send_request_checked, shell_v2};
use super::server::{AdbServer, REQUEST_TIMEOUT};
use super::sync::{IO_TIMEOUT, Progress, SyncSession, list_files, timed};

/// Package manager may spend minutes optimizing a large APK after receiving it.
const INSTALL_RESULT_TIMEOUT: Duration = Duration::from_secs(600);
/// Copying a large OBB between folders on the headset takes a while.
const MOVE_TIMEOUT: Duration = Duration::from_secs(1800);
const PUSH_TMP: &str = "/sdcard/quest-sideloader-tmp";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
	Stdout,
	Stderr,
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

	pub(crate) async fn open(&self, service: &str) -> Result<TcpStream, AdbError> {
		let mut stream = self.server.connect().await?;
		send_request_checked(&mut stream, &format!("host:transport:{}", self.serial)).await?;
		send_request_checked(&mut stream, service).await?;
		Ok(stream)
	}

	/// Shell protocol v2 keeps stdout and stderr apart and reports the exit code.
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

	/// No time limit; output goes to `on_output` as it arrives. Dropping the
	/// future ends the command on the device.
	pub async fn shell_streaming(
		&self,
		command: &str,
		mut on_output: impl FnMut(OutputStream, String),
	) -> Result<Option<u8>, AdbError> {
		let mut stream = self.open(&format!("shell,v2,raw:{command}")).await?;
		let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
		while let Some(packet) = read_shell_packet(&mut stream).await? {
			let (pending, kind) = match packet.id {
				shell_v2::STDOUT => (&mut stdout, OutputStream::Stdout),
				shell_v2::STDERR => (&mut stderr, OutputStream::Stderr),
				shell_v2::EXIT => return Ok(packet.data.first().copied()),
				_ => continue,
			};
			let text = decode_utf8(pending, &packet.data);
			if !text.is_empty() {
				on_output(kind, text);
			}
		}
		Ok(None)
	}

	pub(crate) async fn shell_ok(&self, command: &str) -> Result<String, AdbError> {
		let out = self.shell(command).await?;
		if out.success() {
			Ok(out.stdout)
		} else {
			let detail = if out.stderr.trim().is_empty() {
				out.stdout.trim()
			} else {
				out.stderr.trim()
			};
			Err(AdbError::Server(
				t!("adb.command_failed", command = command, detail = detail).into_owned(),
			))
		}
	}

	pub(crate) async fn getprop(&self, name: &str) -> Result<String, AdbError> {
		Ok(self
			.shell_ok(&format!("getprop {name}"))
			.await?
			.trim()
			.to_owned())
	}

	/// The install or update time comes from `stat` on the APK, when it can read it.
	pub async fn third_party_packages(&self) -> Result<Vec<Package>, AdbError> {
		let mut packages = parse_packages(
			&self
				.shell_ok("pm list packages -3 -f --show-versioncode")
				.await?,
		);
		let paths: Vec<String> = packages
			.iter()
			.filter_map(|p| p.apk.as_deref())
			.map(|p| shell_words::quote(p).into_owned())
			.collect();
		if !paths.is_empty()
			&& let Ok(out) = self
				.shell(&format!("stat -c '%Y %n' {}", paths.join(" ")))
				.await
		{
			let times = parse_stat_times(&out.stdout);
			for package in &mut packages {
				package.installed = package.apk.as_ref().and_then(|apk| times.get(apk).copied());
			}
		}
		Ok(packages)
	}

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

	/// `(remote path, local path)` pairs.
	pub async fn pull(
		&self,
		files: &[(String, std::path::PathBuf)],
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let stream = timed(self.open("sync:")).await?;
		let mut session = SyncSession::new(stream);
		for (remote, local) in files {
			session.recv_file(remote, local, progress).await?;
		}
		session.quit().await
	}

	/// Writes stdout into `local` as it comes; returns the exit code and stderr.
	pub async fn shell_to_file(
		&self,
		command: &str,
		local: &Path,
	) -> Result<(Option<u8>, String), AdbError> {
		let local_err = |source| AdbError::LocalIo {
			path: local.to_path_buf(),
			source,
		};
		if let Some(parent) = local.parent() {
			tokio::fs::create_dir_all(parent).await.map_err(local_err)?;
		}
		let mut file = tokio::fs::File::create(local).await.map_err(local_err)?;
		let mut stream = timed(self.open(&format!("shell,v2,raw:{command}"))).await?;
		let mut stderr = Vec::new();
		let mut exit_code = None;
		while let Some(packet) = timed(read_shell_packet(&mut stream)).await? {
			match packet.id {
				shell_v2::STDOUT => file.write_all(&packet.data).await.map_err(local_err)?,
				shell_v2::STDERR => stderr.extend_from_slice(&packet.data),
				shell_v2::EXIT => {
					exit_code = packet.data.first().copied();
					break;
				}
				_ => {}
			}
		}
		file.flush().await.map_err(local_err)?;
		Ok((exit_code, String::from_utf8_lossy(&stderr).into_owned()))
	}

	/// Files the shell can't read are skipped and returned instead of ending the
	/// transfer.
	pub async fn pull_readable(
		&self,
		files: &[(String, std::path::PathBuf)],
		progress: Progress<'_>,
	) -> Result<Vec<String>, AdbError> {
		let mut skipped = Vec::new();
		let mut rest = files;
		'session: loop {
			let stream = timed(self.open("sync:")).await?;
			let mut session = SyncSession::new(stream);
			for (i, (remote, local)) in rest.iter().enumerate() {
				match session.recv_file(remote, local, progress).await {
					Ok(()) => {}
					// adbd ends the session after a failed file, so a new one
					// carries on with the rest.
					Err(AdbError::PullFailed { message, .. })
						if message.contains("Permission denied") =>
					{
						tracing::warn!("skipped unreadable {remote}: {message}");
						skipped.push(remote.clone());
						rest = &rest[i + 1..];
						continue 'session;
					}
					Err(e) => return Err(e),
				}
			}
			session.quit().await?;
			return Ok(skipped);
		}
	}

	/// Empty if `dir` doesn't exist.
	pub async fn list_files(&self, dir: &str) -> Result<Vec<(String, u64)>, AdbError> {
		let quoted = shell_words::quote(dir);
		let out = self
			.shell(&format!(
				"[ -d {quoted} ] && find {quoted} -type f -exec stat -c '%s %n' {{}} +"
			))
			.await?;
		Ok(out
			.stdout
			.lines()
			.filter_map(|line| {
				let (size, path) = line.split_once(' ')?;
				Some((path.to_owned(), size.parse().ok()?))
			})
			.collect())
	}

	pub async fn apk_path(&self, package: &str) -> Result<String, AdbError> {
		let out = self
			.shell_ok(&format!("pm path {}", shell_words::quote(package)))
			.await?;
		out.lines()
			.filter_map(|l| l.trim().strip_prefix("package:"))
			.find(|p| p.ends_with("/base.apk"))
			.or_else(|| out.lines().find_map(|l| l.trim().strip_prefix("package:")))
			.map(str::to_owned)
			.ok_or_else(|| {
				AdbError::Server(t!("adb.not_installed", package = package).into_owned())
			})
	}

	/// Inside another app's `Android/data` or `Android/obb` this goes through a
	/// temporary folder.
	pub async fn push(
		&self,
		local: &Path,
		remote: &str,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let remote = remote.trim_end_matches('/');
		let is_dir = tokio::fs::metadata(local)
			.await
			.map_err(|source| AdbError::LocalIo {
				path: local.to_path_buf(),
				source,
			})?
			.is_dir();
		if is_protected(remote) {
			self.push_through_temp(local, is_dir, remote, progress)
				.await
		} else {
			self.send(local, is_dir, remote, progress).await
		}
	}

	async fn send(
		&self,
		local: &Path,
		is_dir: bool,
		remote: &str,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		let files = if is_dir {
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

	/// Newer headsets refuse adbd creating folders inside another app's
	/// `Android/data` or `Android/obb` ("secure_mkdirs failed: Operation not
	/// permitted"), while the shell still may. So the files go to a temporary
	/// folder first and the shell moves them into place.
	async fn push_through_temp(
		&self,
		local: &Path,
		is_dir: bool,
		remote: &str,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		static NEXT: AtomicU64 = AtomicU64::new(0);
		let q = |s: &str| shell_words::quote(s).into_owned();
		let tmp_root = format!(
			"{PUSH_TMP}/{}-{}",
			std::process::id(),
			NEXT.fetch_add(1, Ordering::Relaxed)
		);
		let tmp = format!("{tmp_root}/item");
		let result = async {
			self.send(local, is_dir, &tmp, progress).await?;
			let parent = remote.rsplit_once('/').map_or("/", |(p, _)| p);
			// An existing folder keeps its other files, so it is merged into;
			// anything else is moved, which is quick on the same storage.
			let command = if is_dir {
				format!(
					"mkdir -p {parent} && if [ -d {dest} ]; then cp -R {tmp}/. {dest}; else mv {tmp} {dest}; fi",
					parent = q(parent),
					dest = q(remote),
					tmp = q(&tmp),
				)
			} else {
				format!(
					"mkdir -p {parent} && mv -f {tmp} {dest}",
					parent = q(parent),
					dest = q(remote),
					tmp = q(&tmp),
				)
			};
			let out = self.shell_with_timeout(&command, MOVE_TIMEOUT).await?;
			if out.success() {
				Ok(())
			} else {
				Err(AdbError::PushFailed {
					remote: remote.to_owned(),
					message: out.stderr.trim().to_owned(),
				})
			}
		}
		.await;
		let _ = self.shell(&format!("rm -rf {}", q(&tmp_root))).await;
		result
	}

	/// Replaces an existing install and grants runtime permissions.
	/// Without streaming the APK is copied to the headset first.
	pub async fn install_apk(
		&self,
		apk: &Path,
		streaming: bool,
		progress: Progress<'_>,
	) -> Result<(), AdbError> {
		if streaming {
			self.install_streamed(apk, progress).await
		} else {
			self.install_pushed(apk, progress).await
		}
	}

	async fn install_streamed(&self, apk: &Path, progress: Progress<'_>) -> Result<(), AdbError> {
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
				.map_err(|_| AdbError::StreamBroken(AdbError::Timeout.to_string()))?
				.map_err(broken)?;
			progress(n as u64);
		}
		stream.flush().await.map_err(broken)?;

		let mut output = String::new();
		tokio::time::timeout(INSTALL_RESULT_TIMEOUT, stream.read_to_string(&mut output))
			.await
			.map_err(|_| AdbError::Timeout)?
			.map_err(broken)?;
		let output = output.trim();
		if output.is_empty() {
			return Err(AdbError::StreamBroken(
				"no response from package manager".into(),
			));
		}
		install_result(output)
	}

	async fn install_pushed(&self, apk: &Path, progress: Progress<'_>) -> Result<(), AdbError> {
		static NEXT: AtomicU64 = AtomicU64::new(0);
		// pm can't read /sdcard on newer Android; adb uses this folder too.
		let tmp = format!(
			"/data/local/tmp/quest-sideloader-{}-{}.apk",
			std::process::id(),
			NEXT.fetch_add(1, Ordering::Relaxed)
		);
		let quoted = shell_words::quote(&tmp).into_owned();
		let result = async {
			self.send(apk, false, &tmp, progress).await?;
			let out = self
				.shell_with_timeout(
					&format!("pm install -r -g {quoted}"),
					INSTALL_RESULT_TIMEOUT,
				)
				.await?;
			let output = format!("{}\n{}", out.stdout.trim(), out.stderr.trim());
			install_result(output.trim())
		}
		.await;
		let _ = self.shell(&format!("rm -f {quoted}")).await;
		result
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

fn is_protected(remote: &str) -> bool {
	["/sdcard/Android/data/", "/sdcard/Android/obb/"]
		.iter()
		.any(|dir| remote.starts_with(dir))
}

/// Packets can split a character in two, so an incomplete one at the end
/// waits in `pending` for the next packet.
fn decode_utf8(pending: &mut Vec<u8>, data: &[u8]) -> String {
	pending.extend_from_slice(data);
	let keep = match std::str::from_utf8(pending) {
		Err(e) if e.error_len().is_none() => pending.len() - e.valid_up_to(),
		_ => 0,
	};
	let tail = pending.split_off(pending.len() - keep);
	let text = String::from_utf8_lossy(pending).into_owned();
	*pending = tail;
	text
}

fn broken(e: std::io::Error) -> AdbError {
	AdbError::StreamBroken(e.to_string())
}

fn install_result(output: &str) -> Result<(), AdbError> {
	if output.lines().any(|l| l.trim() == "Success") {
		Ok(())
	} else {
		Err(AdbError::InstallFailed(output.to_owned()))
	}
}

#[cfg(test)]
mod tests {
	use std::net::SocketAddr;

	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	use tokio::net::TcpListener;

	use super::*;
	use crate::adb::testing::*;

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
	async fn streaming_shell_passes_output_through() {
		let word = "привет".as_bytes();
		let mut reply = packet(shell_v2::STDOUT, &word[..3]);
		reply.extend(packet(shell_v2::STDOUT, &word[3..]));
		reply.extend(packet(shell_v2::STDERR, b"oops"));
		reply.extend(packet(shell_v2::EXIT, &[2]));
		let (addr, server) = fake_server("SER1", reply).await;

		let device = AdbServer::with_addr(addr).device("SER1");
		let mut got = Vec::new();
		let code = device
			.shell_streaming("echo", |stream, text| got.push((stream, text)))
			.await
			.unwrap();
		assert_eq!(server.await.unwrap(), "shell,v2,raw:echo");
		assert_eq!(code, Some(2));
		assert_eq!(
			got,
			[
				(OutputStream::Stdout, "п".to_owned()),
				(OutputStream::Stdout, "ривет".to_owned()),
				(OutputStream::Stderr, "oops".to_owned()),
			]
		);
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
			.push(&obb, "/sdcard/Download/com.example.game/", &progress)
			.await
			.unwrap();
		server.await.unwrap();

		let files = rx.await.unwrap();
		assert_eq!(files.len(), 2);
		assert_eq!(
			files[0].0,
			"/sdcard/Download/com.example.game/main.1.com.example.game.obb"
		);
		assert_eq!(files[0].1, "33188");
		assert_eq!(files[0].2, big);
		assert_eq!(
			files[1].0,
			"/sdcard/Download/com.example.game/sub/extra.bin"
		);
		assert_eq!(files[1].2, b"extra");
		assert_eq!(sent.into_inner(), big.len() as u64 + 5);
	}

	#[tokio::test]
	async fn obb_and_data_go_through_a_temp_folder() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("main.obb"), b"0123456789").unwrap();
		let fake = crate::adb::testing::FakeAdb::start().await;

		let sent = AtomicU64::new(0);
		let progress = |n: u64| {
			sent.fetch_add(n, Ordering::Relaxed);
		};
		AdbServer::with_addr(([127, 0, 0, 1], fake.port).into())
			.device("SER1")
			.push(dir.path(), "/sdcard/Android/obb/com.x", &progress)
			.await
			.unwrap();

		let rec = fake.recorded.lock().unwrap();
		// The fake applies the move, so the file ends up where it was meant to.
		assert_eq!(
			rec.pushed,
			[(
				"/sdcard/Android/obb/com.x/main.obb".to_owned(),
				b"0123456789".to_vec()
			)]
		);
		let [moved, removed] = rec.shell.as_slice() else {
			panic!("{:?}", rec.shell);
		};
		let tmp = removed.strip_prefix("rm -rf ").unwrap();
		assert!(tmp.starts_with("/sdcard/quest-sideloader-tmp/"));
		assert_eq!(
			moved,
			&format!(
				"mkdir -p /sdcard/Android/obb && if [ -d /sdcard/Android/obb/com.x ]; then cp -R {tmp}/item/. /sdcard/Android/obb/com.x; else mv {tmp}/item /sdcard/Android/obb/com.x; fi"
			)
		);
		assert_eq!(sent.into_inner(), 10);
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
			.install_apk(&apk, true, &|_| {})
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
			.install_apk(&apk, true, &|_| {})
			.await
			.unwrap_err();
		assert!(
			matches!(err, AdbError::InstallFailed(m) if m.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE"))
		);
	}

	async fn serve_recv(s: &mut TcpStream, expect: &str, reply: Result<Vec<u8>, &str>) {
		let (id, len) = read_sync_header(s).await;
		assert_eq!(&id, b"RECV");
		let mut path = vec![0u8; len as usize];
		s.read_exact(&mut path).await.unwrap();
		assert_eq!(String::from_utf8(path).unwrap(), expect);
		match reply {
			Ok(data) => {
				for chunk in data.chunks(50_000) {
					s.write_all(b"DATA").await.unwrap();
					s.write_all(&(chunk.len() as u32).to_le_bytes())
						.await
						.unwrap();
					s.write_all(chunk).await.unwrap();
				}
				s.write_all(b"DONE\0\0\0\0").await.unwrap();
			}
			Err(message) => {
				s.write_all(b"FAIL").await.unwrap();
				s.write_all(&(message.len() as u32).to_le_bytes())
					.await
					.unwrap();
				s.write_all(message.as_bytes()).await.unwrap();
			}
		}
	}

	#[tokio::test]
	async fn pull_receives_files() {
		let data: Vec<u8> = (0..120_000u32).map(|i| (i % 241) as u8).collect();
		let served = data.clone();
		let (addr, server) = fake_device("sync:".into(), move |mut s| async move {
			serve_recv(&mut s, "/data/app/x/base.apk", Ok(served)).await;
			serve_recv(
				&mut s,
				"/sdcard/Android/obb/com.x/main.obb",
				Ok(b"obb".to_vec()),
			)
			.await;
			let (id, _) = read_sync_header(&mut s).await;
			assert_eq!(&id, b"QUIT");
		})
		.await;
		let dir = tempfile::tempdir().unwrap();
		let files = vec![
			(
				"/data/app/x/base.apk".to_owned(),
				dir.path().join("com.x.apk"),
			),
			(
				"/sdcard/Android/obb/com.x/main.obb".to_owned(),
				dir.path().join("com.x/main.obb"),
			),
		];
		let received = std::sync::atomic::AtomicU64::new(0);
		AdbServer::with_addr(addr)
			.device("SER1")
			.pull(&files, &|n| {
				received.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
			})
			.await
			.unwrap();
		server.await.unwrap();
		assert_eq!(std::fs::read(dir.path().join("com.x.apk")).unwrap(), data);
		assert_eq!(
			std::fs::read(dir.path().join("com.x/main.obb")).unwrap(),
			b"obb"
		);
		assert_eq!(received.into_inner(), data.len() as u64 + 3);
	}

	#[tokio::test]
	async fn pull_failure_leaves_no_partial_file() {
		let (addr, _server) = fake_device("sync:".into(), |mut s| async move {
			serve_recv(&mut s, "/secret", Err("Permission denied")).await;
		})
		.await;
		let dir = tempfile::tempdir().unwrap();
		let local = dir.path().join("secret");
		let err = AdbServer::with_addr(addr)
			.device("SER1")
			.pull(&[("/secret".to_owned(), local.clone())], &|_| {})
			.await
			.unwrap_err();
		assert!(
			matches!(err, AdbError::PullFailed { message, .. } if message == "Permission denied")
		);
		assert!(!local.exists());
		assert!(!dir.path().join("secret.part").exists());
	}
}
