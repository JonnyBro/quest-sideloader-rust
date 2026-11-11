use tokio::net::TcpStream;

use super::AdbError;
use super::parse::{DeviceInfo, Package, parse_battery, parse_df, parse_packages};
use super::protocol::{read_shell_packet, send_request_checked, shell_v2};
use super::server::{AdbServer, REQUEST_TIMEOUT};

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
		tokio::time::timeout(REQUEST_TIMEOUT, fut)
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

	async fn read_request(stream: &mut TcpStream) -> String {
		let mut len = [0u8; 4];
		stream.read_exact(&mut len).await.unwrap();
		let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
		let mut buf = vec![0u8; len];
		stream.read_exact(&mut buf).await.unwrap();
		String::from_utf8(buf).unwrap()
	}

	fn packet(id: u8, data: &[u8]) -> Vec<u8> {
		let mut out = vec![id];
		out.extend_from_slice(&(data.len() as u32).to_le_bytes());
		out.extend_from_slice(data);
		out
	}

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
}
