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

#[derive(Default)]
pub struct Recorded {
	pub pushed: Vec<(String, Vec<u8>)>,
	pub shell: Vec<String>,
	/// Files the fake headset has, for listing and pulling.
	pub device_files: Vec<(String, Vec<u8>)>,
	/// Sizes of APKs streamed to `cmd package install`.
	pub installs: Vec<usize>,
	/// Streamed installs drop the connection halfway.
	pub break_streams: bool,
	/// Failure messages the next installs answer with, in order.
	pub install_failures: Vec<String>,
	/// Device files that are listed but can't be pulled.
	pub unreadable: Vec<String>,
	/// The app isn't debuggable, so `run-as` refuses.
	pub run_as_refused: bool,
	/// Third-party packages and their version codes, for `pm list packages`.
	pub installed: Vec<(String, u64)>,
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

type Shared = std::sync::Arc<std::sync::Mutex<Recorded>>;

async fn serve(mut s: TcpStream, rec: Shared) {
	// Clients of other, finished tests drop connections mid-request; that's
	// not a failure of the test that owns this fake.
	let _ = try_serve(&mut s, &rec).await;
}

async fn try_read_request(s: &mut TcpStream) -> std::io::Result<String> {
	let mut len = [0u8; 4];
	s.read_exact(&mut len).await?;
	let len = std::str::from_utf8(&len)
		.ok()
		.and_then(|l| usize::from_str_radix(l, 16).ok())
		.ok_or_else(|| std::io::Error::other("bad length"))?;
	let mut buf = vec![0u8; len];
	s.read_exact(&mut buf).await?;
	String::from_utf8(buf).map_err(std::io::Error::other)
}

async fn try_header(s: &mut TcpStream) -> std::io::Result<([u8; 4], usize)> {
	let mut header = [0u8; 8];
	s.read_exact(&mut header).await?;
	let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
	Ok(([header[0], header[1], header[2], header[3]], len))
}

async fn try_serve(s: &mut TcpStream, rec: &Shared) -> std::io::Result<()> {
	match try_read_request(s).await?.as_str() {
		"host:version" => {
			s.write_all(b"OKAY").await?;
			s.write_all(b"00040029").await?;
		}
		"host:track-devices-l" => {
			s.write_all(b"OKAY").await?;
			let devices = "SER1 device model:Quest_3 transport_id:1\n";
			s.write_all(format!("{:04x}{devices}", devices.len()).as_bytes())
				.await?;
			let mut sink = Vec::new();
			let _ = s.read_to_end(&mut sink).await;
		}
		"host:transport:SER1" => {
			s.write_all(b"OKAY").await?;
			let service = try_read_request(s).await?;
			s.write_all(b"OKAY").await?;
			if service == "sync:" {
				receive_and_record(s, rec).await?;
			} else if let Some(size) = service.strip_prefix("exec:cmd package install ") {
				let size: usize = size
					.rsplit(' ')
					.next()
					.and_then(|n| n.parse().ok())
					.unwrap_or(0);
				if rec.lock().unwrap().break_streams {
					let mut half = vec![0u8; size / 2];
					s.read_exact(&mut half).await?;
					return Ok(());
				}
				let mut apk = vec![0u8; size];
				s.read_exact(&mut apk).await?;
				let reply = {
					let mut rec = rec.lock().unwrap();
					rec.installs.push(size);
					if rec.install_failures.is_empty() {
						"Success\n".to_owned()
					} else {
						format!("Failure [{}]\n", rec.install_failures.remove(0))
					}
				};
				s.write_all(reply.as_bytes()).await?;
			} else if let Some(command) = service.strip_prefix("shell,v2,raw:") {
				rec.lock().unwrap().shell.push(command.to_owned());
				// `Device::push` into Android/data or obb ends with `mv TMP DEST`.
				if command.starts_with("mkdir -p ") {
					apply_move(command, &mut rec.lock().unwrap().pushed);
				}
				if command.contains("run-as ") {
					let (refused, data) = {
						let rec = rec.lock().unwrap();
						let words = shell_words::split(command).unwrap_or_default();
						let data = (words.get(2).map(String::as_str) == Some("cat"))
							.then(|| words.get(3))
							.flatten()
							.and_then(|path| rec.device_files.iter().find(|(p, _)| p == path))
							.map(|(_, d)| d.clone());
						(rec.run_as_refused, data)
					};
					if refused {
						s.write_all(&packet(2, b"run-as: package not debuggable\n"))
							.await?;
						s.write_all(&packet(3, &[1])).await?;
						return Ok(());
					}
					if let Some(data) = data {
						s.write_all(&packet(1, &data)).await?;
						s.write_all(&packet(3, &[0])).await?;
						return Ok(());
					}
				}
				if command.starts_with("pm list packages") {
					let listing: String = rec
						.lock()
						.unwrap()
						.installed
						.iter()
						.map(|(p, v)| {
							format!("package:/data/app/~~x/{p}-1/base.apk={p} versionCode:{v}\n")
						})
						.collect();
					s.write_all(&packet(1, listing.as_bytes())).await?;
					s.write_all(&packet(3, &[0])).await?;
					return Ok(());
				}
				if command.starts_with("pm path ") {
					s.write_all(&packet(1, b"package:/data/app/~~x/pkg-1/base.apk\n"))
						.await?;
					s.write_all(&packet(3, &[0])).await?;
					return Ok(());
				}
				if let Some(path) = command.strip_prefix("pm install -r -g ") {
					let path = path.trim_matches('\'');
					{
						let mut rec = rec.lock().unwrap();
						let size = rec
							.pushed
							.iter()
							.find(|(p, _)| p == path)
							.map_or(0, |(_, d)| d.len());
						rec.installs.push(size);
					}
					s.write_all(&packet(1, b"Success\n")).await?;
					s.write_all(&packet(3, &[0])).await?;
					return Ok(());
				}
				if command.starts_with("pm uninstall") {
					s.write_all(&packet(1, b"Success\n")).await?;
					s.write_all(&packet(3, &[0])).await?;
					return Ok(());
				}
				// `Device::list_files`: `[ -d DIR ] && find DIR -type f -exec stat ...`
				if let Some(rest) = command.strip_prefix("[ -d ") {
					let dir = rest
						.split(" ]")
						.next()
						.unwrap_or_default()
						.trim_matches('\'');
					let listing: String = rec
						.lock()
						.unwrap()
						.device_files
						.iter()
						.filter(|(path, _)| path.starts_with(&format!("{dir}/")))
						.map(|(path, data)| format!("{} {path}\n", data.len()))
						.collect();
					s.write_all(&packet(1, listing.as_bytes())).await?;
					s.write_all(&packet(3, &[u8::from(listing.is_empty())]))
						.await?;
					return Ok(());
				}
				const EXISTING_DIRS: [&str; 3] =
					["/sdcard", "/sdcard/Android/obb", "/sdcard/Android/data"];
				let exit = match command.strip_prefix("test -d ") {
					Some(path) if !EXISTING_DIRS.contains(&path) => 1,
					_ => 0,
				};
				s.write_all(&packet(3, &[exit])).await?;
			}
		}
		other => {
			return Err(std::io::Error::other(format!(
				"fake adb: unexpected request {other}"
			)));
		}
	}
	Ok(())
}

fn apply_move(command: &str, pushed: &mut [(String, Vec<u8>)]) {
	let words = shell_words::split(command).unwrap_or_default();
	let Some(mv) = words.iter().position(|w| w == "mv") else {
		return;
	};
	let mut args = words[mv + 1..].iter().filter(|w| *w != "-f");
	let (Some(from), Some(to)) = (args.next(), args.next()) else {
		return;
	};
	let to = to.trim_end_matches(';');
	for (path, _) in pushed.iter_mut() {
		if let Some(rest) = path.strip_prefix(from.as_str()) {
			*path = format!("{to}{rest}");
		}
	}
}

/// Records each pushed file before acknowledging it. The client waits for that
/// acknowledgement, so once its task is done every file is already recorded.
async fn receive_and_record(s: &mut TcpStream, rec: &Shared) -> std::io::Result<()> {
	loop {
		let (id, len) = try_header(s).await?;
		match &id {
			b"QUIT" => return Ok(()),
			b"SEND" => {}
			b"RECV" => {
				let mut path = vec![0u8; len];
				s.read_exact(&mut path).await?;
				let path = String::from_utf8_lossy(&path).into_owned();
				let data = {
					let rec = rec.lock().unwrap();
					rec.device_files
						.iter()
						.find(|(p, _)| *p == path && !rec.unreadable.contains(p))
						.map(|(_, d)| d.clone())
				};
				match data {
					Some(data) => {
						for chunk in data.chunks(64 * 1024) {
							s.write_all(b"DATA").await?;
							s.write_all(&(chunk.len() as u32).to_le_bytes()).await?;
							s.write_all(chunk).await?;
						}
						s.write_all(b"DONE\0\0\0\0").await?;
					}
					None => {
						let message: &[u8] = if rec.lock().unwrap().unreadable.contains(&path) {
							b"open failed: Permission denied"
						} else {
							b"No such file or directory"
						};
						s.write_all(b"FAIL").await?;
						s.write_all(&(message.len() as u32).to_le_bytes()).await?;
						s.write_all(message).await?;
					}
				}
				continue;
			}
			other => {
				return Err(std::io::Error::other(format!(
					"unexpected sync request {other:?}"
				)));
			}
		}
		let mut target = vec![0u8; len];
		s.read_exact(&mut target).await?;
		let target = String::from_utf8_lossy(&target).into_owned();
		let path = target
			.rsplit_once(',')
			.map_or(target.as_str(), |(p, _)| p)
			.to_owned();
		let mut content = Vec::new();
		loop {
			let (id, len) = try_header(s).await?;
			match &id {
				b"DATA" => {
					let mut chunk = vec![0u8; len];
					s.read_exact(&mut chunk).await?;
					content.extend(chunk);
				}
				b"DONE" => break,
				other => {
					return Err(std::io::Error::other(format!(
						"unexpected sync data {other:?}"
					)));
				}
			}
		}
		rec.lock().unwrap().pushed.push((path, content));
		s.write_all(b"OKAY\0\0\0\0").await?;
	}
}
