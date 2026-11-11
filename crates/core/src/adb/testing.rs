//! Helpers for tests that stand up a fake adb server.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn read_request(stream: &mut TcpStream) -> String {
	let mut len = [0u8; 4];
	stream.read_exact(&mut len).await.unwrap();
	let len = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
	let mut buf = vec![0u8; len];
	stream.read_exact(&mut buf).await.unwrap();
	String::from_utf8(buf).unwrap()
}

pub fn packet(id: u8, data: &[u8]) -> Vec<u8> {
	let mut out = vec![id];
	out.extend_from_slice(&(data.len() as u32).to_le_bytes());
	out.extend_from_slice(data);
	out
}

pub async fn read_sync_header(stream: &mut TcpStream) -> ([u8; 4], u32) {
	let mut header = [0u8; 8];
	stream.read_exact(&mut header).await.unwrap();
	let id = [header[0], header[1], header[2], header[3]];
	(
		id,
		u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
	)
}

/// Reads SEND requests until QUIT and returns `(remote path, mode, content)`.
pub async fn receive_files(stream: &mut TcpStream) -> Vec<(String, String, Vec<u8>)> {
	let mut files = Vec::new();
	loop {
		let (id, len) = read_sync_header(stream).await;
		if &id == b"QUIT" {
			return files;
		}
		assert_eq!(&id, b"SEND");
		let mut target = vec![0u8; len as usize];
		stream.read_exact(&mut target).await.unwrap();
		let target = String::from_utf8(target).unwrap();
		let (path, mode) = target.rsplit_once(',').unwrap();
		let mut content = Vec::new();
		loop {
			let (id, len) = read_sync_header(stream).await;
			match &id {
				b"DATA" => {
					let mut chunk = vec![0u8; len as usize];
					stream.read_exact(&mut chunk).await.unwrap();
					content.extend(chunk);
				}
				b"DONE" => break,
				other => panic!("unexpected {other:?}"),
			}
		}
		stream.write_all(b"OKAY\0\0\0\0").await.unwrap();
		files.push((path.to_owned(), mode.to_owned(), content));
	}
}

pub async fn write_hex(stream: &mut TcpStream, payload: &str) {
	let framed = format!("{:04x}{payload}", payload.len());
	stream.write_all(framed.as_bytes()).await.unwrap();
}

/// What a [`FakeAdb`] received from the client.
#[derive(Default)]
pub struct Recorded {
	pub pushed: Vec<(String, Vec<u8>)>,
	pub shell: Vec<String>,
}

/// A fake adb server with one online device `SER1`: answers version and
/// device tracking, records pushes and shell commands (all succeed).
pub struct FakeAdb {
	pub port: u16,
	pub recorded: std::sync::Arc<std::sync::Mutex<Recorded>>,
}

impl FakeAdb {
	pub async fn start() -> Self {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let port = listener.local_addr().unwrap().port();
		let recorded = std::sync::Arc::new(std::sync::Mutex::new(Recorded::default()));
		let rec = recorded.clone();
		tokio::spawn(async move {
			while let Ok((stream, _)) = listener.accept().await {
				tokio::spawn(serve(stream, rec.clone()));
			}
		});
		Self { port, recorded }
	}
}

async fn serve(mut s: TcpStream, rec: std::sync::Arc<std::sync::Mutex<Recorded>>) {
	let request = read_request(&mut s).await;
	match request.as_str() {
		"host:version" => {
			s.write_all(b"OKAY").await.unwrap();
			write_hex(&mut s, "0029").await;
		}
		"host:track-devices-l" => {
			s.write_all(b"OKAY").await.unwrap();
			write_hex(&mut s, "SER1 device model:Quest_3 transport_id:1\n").await;
			let mut sink = Vec::new();
			let _ = s.read_to_end(&mut sink).await;
		}
		"host:transport:SER1" => {
			s.write_all(b"OKAY").await.unwrap();
			let service = read_request(&mut s).await;
			s.write_all(b"OKAY").await.unwrap();
			if service == "sync:" {
				let files = receive_files(&mut s).await;
				let mut rec = rec.lock().unwrap();
				rec.pushed
					.extend(files.into_iter().map(|(path, _, data)| (path, data)));
			} else if let Some(command) = service.strip_prefix("shell,v2,raw:") {
				rec.lock().unwrap().shell.push(command.to_owned());
				const EXISTING_DIRS: [&str; 3] =
					["/sdcard", "/sdcard/Android/obb", "/sdcard/Android/data"];
				let exit = match command.strip_prefix("test -d ") {
					Some(path) if !EXISTING_DIRS.contains(&path) => 1,
					_ => 0,
				};
				s.write_all(&packet(3, &[exit])).await.unwrap();
			}
		}
		other => panic!("fake adb: unexpected request {other}"),
	}
}
