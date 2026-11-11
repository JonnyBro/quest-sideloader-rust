use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::TcpStream;

use super::AdbError;
use super::device::Device;
use super::parse::{DeviceEntry, parse_devices};
use super::protocol::{read_hex_string, send_request_checked};

pub const DEFAULT_PORT: u16 = 5037;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const START_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct AdbServer {
	addr: SocketAddr,
	adb_path: PathBuf,
}

impl AdbServer {
	/// `adb_path = None` means `adb` from `PATH`.
	pub fn new(adb_path: Option<PathBuf>) -> Self {
		Self::with_port(adb_path, DEFAULT_PORT)
	}

	pub fn with_port(adb_path: Option<PathBuf>, port: u16) -> Self {
		Self {
			addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
			adb_path: adb_path.unwrap_or_else(|| PathBuf::from("adb")),
		}
	}

	#[cfg(test)]
	pub(crate) fn with_addr(addr: SocketAddr) -> Self {
		Self {
			addr,
			adb_path: PathBuf::from("adb"),
		}
	}

	pub(crate) async fn connect(&self) -> Result<TcpStream, AdbError> {
		match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(self.addr)).await {
			Ok(Ok(stream)) => {
				stream.set_nodelay(true)?;
				Ok(stream)
			}
			Ok(Err(e)) => Err(AdbError::Unreachable(e)),
			Err(_) => Err(AdbError::Timeout),
		}
	}

	/// Starts the server through the `adb` binary if nothing listens on the port.
	/// Returns the server's protocol version.
	pub async fn ensure_running(&self) -> Result<u32, AdbError> {
		if let Ok(version) = self.version().await {
			return Ok(version);
		}
		self.start_server().await?;
		self.version().await
	}

	async fn start_server(&self) -> Result<(), AdbError> {
		tracing::info!("starting adb server with {}", self.adb_path.display());
		let mut command = tokio::process::Command::new(&self.adb_path);
		command
			.arg("-P")
			.arg(self.addr.port().to_string())
			.arg("start-server")
			.stdin(std::process::Stdio::null())
			.kill_on_drop(true);
		#[cfg(windows)]
		{
			const CREATE_NO_WINDOW: u32 = 0x0800_0000;
			command.creation_flags(CREATE_NO_WINDOW);
		}

		let output = match tokio::time::timeout(START_TIMEOUT, command.output()).await {
			Err(_) => return Err(AdbError::Timeout),
			Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
				return Err(AdbError::BinaryNotFound(self.adb_path.clone()));
			}
			Ok(Err(e)) => return Err(AdbError::StartFailed(e.to_string())),
			Ok(Ok(output)) => output,
		};
		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);
			return Err(AdbError::StartFailed(stderr.trim().to_owned()));
		}
		Ok(())
	}

	async fn host_query(&self, request: &str) -> Result<String, AdbError> {
		let fut = async {
			let mut stream = self.connect().await?;
			send_request_checked(&mut stream, request).await?;
			read_hex_string(&mut stream).await
		};
		tokio::time::timeout(REQUEST_TIMEOUT, fut)
			.await
			.map_err(|_| AdbError::Timeout)?
	}

	pub async fn version(&self) -> Result<u32, AdbError> {
		let text = self.host_query("host:version").await?;
		u32::from_str_radix(text.trim(), 16)
			.map_err(|_| AdbError::Protocol(format!("bad version {text:?}")))
	}

	pub async fn devices(&self) -> Result<Vec<DeviceEntry>, AdbError> {
		Ok(parse_devices(&self.host_query("host:devices-l").await?))
	}

	/// The server pushes the full device list on every change.
	pub async fn track_devices(&self) -> Result<DeviceTracker, AdbError> {
		let mut stream = self.connect().await?;
		send_request_checked(&mut stream, "host:track-devices-l").await?;
		Ok(DeviceTracker { stream })
	}

	/// `address` is `host` or `host:port`.
	pub async fn connect_wireless(&self, address: &str) -> Result<String, AdbError> {
		let message = self.host_query(&format!("host:connect:{address}")).await?;
		let lower = message.to_lowercase();
		if lower.contains("failed") || lower.contains("cannot") || lower.contains("unable") {
			Err(AdbError::Server(message))
		} else {
			Ok(message)
		}
	}

	pub async fn disconnect_wireless(&self, address: &str) -> Result<String, AdbError> {
		self.host_query(&format!("host:disconnect:{address}")).await
	}

	pub fn device(&self, serial: impl Into<String>) -> Device {
		Device::new(self.clone(), serial.into())
	}
}

pub struct DeviceTracker {
	stream: TcpStream,
}

impl DeviceTracker {
	pub async fn next(&mut self) -> Result<Vec<DeviceEntry>, AdbError> {
		Ok(parse_devices(&read_hex_string(&mut self.stream).await?))
	}
}

#[cfg(test)]
mod tests {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	use tokio::net::TcpListener;

	use super::*;
	use crate::adb::DeviceState;

	async fn read_request(stream: &mut TcpStream) -> String {
		let mut len = [0u8; 4];
		stream.read_exact(&mut len).await.unwrap();
		let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
		let mut buf = vec![0u8; len];
		stream.read_exact(&mut buf).await.unwrap();
		String::from_utf8(buf).unwrap()
	}

	async fn write_hex(stream: &mut TcpStream, payload: &str) {
		let framed = format!("{:04x}{payload}", payload.len());
		stream.write_all(framed.as_bytes()).await.unwrap();
	}

	#[tokio::test]
	async fn version_and_device_tracking() {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let server = AdbServer::with_addr(listener.local_addr().unwrap());
		let fake = tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			assert_eq!(read_request(&mut s).await, "host:version");
			s.write_all(b"OKAY").await.unwrap();
			write_hex(&mut s, "0029").await;

			let (mut s, _) = listener.accept().await.unwrap();
			assert_eq!(read_request(&mut s).await, "host:track-devices-l");
			s.write_all(b"OKAY").await.unwrap();
			write_hex(&mut s, "").await;
			write_hex(&mut s, "SER1 unauthorized usb:1-4 transport_id:1\n").await;
			write_hex(&mut s, "SER1 device usb:1-4 model:Quest_3 transport_id:1\n").await;
		});

		assert_eq!(server.ensure_running().await.unwrap(), 41);
		let mut tracker = server.track_devices().await.unwrap();
		assert!(tracker.next().await.unwrap().is_empty());
		assert_eq!(
			tracker.next().await.unwrap()[0].state,
			DeviceState::Unauthorized
		);
		let online = tracker.next().await.unwrap();
		assert_eq!(online[0].state, DeviceState::Online);
		assert_eq!(online[0].model.as_deref(), Some("Quest_3"));
		fake.await.unwrap();
		assert!(
			tracker.next().await.is_err(),
			"closed connection must end tracking"
		);
	}

	#[tokio::test]
	async fn wireless_connect_failure_is_an_error() {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let server = AdbServer::with_addr(listener.local_addr().unwrap());
		tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			assert_eq!(read_request(&mut s).await, "host:connect:10.0.0.9:5555");
			s.write_all(b"OKAY").await.unwrap();
			write_hex(
				&mut s,
				"failed to connect to '10.0.0.9:5555': Connection refused",
			)
			.await;
		});
		let err = server.connect_wireless("10.0.0.9:5555").await.unwrap_err();
		assert!(matches!(err, AdbError::Server(m) if m.contains("Connection refused")));
	}
}
