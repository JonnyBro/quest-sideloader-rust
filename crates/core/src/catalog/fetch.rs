//! Downloading files from a mirror, over HTTP or through rclone.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::mirror::{Mirror, MirrorKind};
use super::{CatalogError, io_err};

/// Receives `(bytes so far, total if known)`.
pub type FetchProgress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

const RCLONE_ROOT: &str = "Quest Games";

#[derive(Clone)]
pub struct Fetcher {
	http: reqwest::Client,
	rclone: PathBuf,
}

impl Fetcher {
	pub fn new(rclone_path: Option<PathBuf>) -> Result<Self, CatalogError> {
		let http = reqwest::Client::builder()
			.user_agent(concat!("quest-sideloader/", env!("CARGO_PKG_VERSION")))
			.connect_timeout(Duration::from_secs(20))
			.read_timeout(Duration::from_secs(60))
			.build()
			.map_err(|e| CatalogError::Http(e.to_string()))?;
		Ok(Self {
			http,
			rclone: rclone_path.unwrap_or_else(|| PathBuf::from("rclone")),
		})
	}

	/// Downloads `remote_path` (relative to the mirror root) to `dest`. An
	/// HTTP download resumes from `dest.part` if a previous attempt left one.
	pub async fn download_file(
		&self,
		mirror: &Mirror,
		remote_path: &str,
		dest: &Path,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		if let Some(parent) = dest.parent() {
			tokio::fs::create_dir_all(parent)
				.await
				.map_err(io_err(parent))?;
		}
		match &mirror.kind {
			MirrorKind::Http { base_uri } => {
				let url = join_url(base_uri, remote_path)?;
				self.http_download(url, &mirror.headers, dest, progress)
					.await
			}
			MirrorKind::Rclone {
				config_file,
				remote,
			} => {
				let source = format!("{remote}:/{RCLONE_ROOT}/{remote_path}");
				self.rclone_copyto(config_file, &mirror.headers, &source, dest, progress)
					.await
			}
		}
	}

	async fn http_download(
		&self,
		url: reqwest::Url,
		headers: &[super::mirror::Header],
		dest: &Path,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		let part = part_path(dest);
		let resume_from = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());

		let mut request = self.http.get(url.clone());
		for header in headers {
			request = request.header(&header.name, &header.value);
		}
		if resume_from > 0 {
			request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
		}
		let mut response = request
			.send()
			.await
			.map_err(|e| CatalogError::Http(e.to_string()))?;
		let status = response.status();
		if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && resume_from > 0 {
			// The partial file is already complete (or stale); start over.
			tokio::fs::remove_file(&part).await.map_err(io_err(&part))?;
			return Box::pin(self.http_download(url, headers, dest, progress)).await;
		}
		if !status.is_success() {
			return Err(CatalogError::Status {
				status: status.as_u16(),
				url: redact(&url),
			});
		}

		let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
		let mut done = if resumed { resume_from } else { 0 };
		let total = response.content_length().map(|len| len + done);
		let mut file = tokio::fs::OpenOptions::new()
			.create(true)
			.write(true)
			.append(resumed)
			.truncate(!resumed)
			.open(&part)
			.await
			.map_err(io_err(&part))?;

		progress(done, total);
		while let Some(chunk) = response
			.chunk()
			.await
			.map_err(|e| CatalogError::Http(e.to_string()))?
		{
			file.write_all(&chunk).await.map_err(io_err(&part))?;
			done += chunk.len() as u64;
			progress(done, total);
		}
		file.flush().await.map_err(io_err(&part))?;
		drop(file);
		if let Some(total) = total
			&& done != total
		{
			return Err(CatalogError::Http(format!(
				"download ended early ({done} of {total} bytes)"
			)));
		}
		tokio::fs::rename(&part, dest).await.map_err(io_err(dest))
	}

	async fn rclone_copyto(
		&self,
		config_file: &Path,
		headers: &[super::mirror::Header],
		source: &str,
		dest: &Path,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		let mut command = tokio::process::Command::new(&self.rclone);
		// Through the environment rather than `--header`, so keys don't show up
		// in other users' process lists.
		if !headers.is_empty() {
			command.env("RCLONE_HEADER", rclone_header_env(headers));
		}
		command
			.arg("copyto")
			.arg(source)
			.arg(dest)
			.arg("--config")
			.arg(config_file)
			.args([
				"--use-json-log",
				"--stats",
				"1s",
				"--stats-log-level",
				"NOTICE",
			])
			.args(["--retries", "3", "--low-level-retries", "10"])
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::piped())
			.kill_on_drop(true);
		#[cfg(windows)]
		{
			const CREATE_NO_WINDOW: u32 = 0x0800_0000;
			command.creation_flags(CREATE_NO_WINDOW);
		}
		let mut child = command.spawn().map_err(|e| {
			if e.kind() == std::io::ErrorKind::NotFound {
				CatalogError::RcloneNotFound(self.rclone.clone())
			} else {
				CatalogError::Rclone(e.to_string())
			}
		})?;

		let stderr = child.stderr.take().expect("stderr is piped");
		let mut lines = BufReader::new(stderr).lines();
		let mut last_error = None;
		while let Some(line) = lines
			.next_line()
			.await
			.map_err(|e| CatalogError::Rclone(e.to_string()))?
		{
			match parse_rclone_log(&line) {
				Some(RcloneLog::Stats { bytes, total }) => progress(bytes, total),
				Some(RcloneLog::Error(message)) => last_error = Some(message),
				None => {}
			}
		}
		let status = child
			.wait()
			.await
			.map_err(|e| CatalogError::Rclone(e.to_string()))?;
		if status.success() {
			Ok(())
		} else {
			Err(CatalogError::Rclone(
				last_error.unwrap_or_else(|| format!("rclone exited with {status}")),
			))
		}
	}
}

/// rclone reads multi-value flags from the environment as one CSV line.
fn rclone_header_env(headers: &[super::mirror::Header]) -> String {
	headers
		.iter()
		.map(|h| {
			let field = format!("{}: {}", h.name, h.value);
			if field.contains([',', '"', '\n', '\r']) {
				format!("\"{}\"", field.replace('"', "\"\""))
			} else {
				field
			}
		})
		.collect::<Vec<_>>()
		.join(",")
}

fn part_path(dest: &Path) -> PathBuf {
	let mut name = dest.file_name().unwrap_or_default().to_os_string();
	name.push(".part");
	dest.with_file_name(name)
}

fn join_url(base: &str, path: &str) -> Result<reqwest::Url, CatalogError> {
	let base = if base.ends_with('/') {
		base.to_owned()
	} else {
		format!("{base}/")
	};
	reqwest::Url::parse(&base)
		.and_then(|b| b.join(path))
		.map_err(|e| CatalogError::Config(format!("invalid mirror URL: {e}")))
}

/// Mirror URLs can carry credentials; keep them out of error messages.
fn redact(url: &reqwest::Url) -> String {
	let mut url = url.clone();
	let _ = url.set_username("");
	let _ = url.set_password(None);
	url.set_query(None);
	url.to_string()
}

#[derive(Debug, PartialEq)]
enum RcloneLog {
	Stats { bytes: u64, total: Option<u64> },
	Error(String),
}

fn parse_rclone_log(line: &str) -> Option<RcloneLog> {
	let value: serde_json::Value = serde_json::from_str(line).ok()?;
	if let Some(stats) = value.get("stats") {
		let bytes = stats.get("bytes")?.as_u64()?;
		let total = stats
			.get("totalBytes")
			.and_then(serde_json::Value::as_u64)
			.filter(|&t| t > 0);
		return Some(RcloneLog::Stats { bytes, total });
	}
	let level = value.get("level")?.as_str()?;
	if level == "error" || level == "critical" {
		return Some(RcloneLog::Error(
			value.get("msg")?.as_str()?.trim().to_owned(),
		));
	}
	None
}

#[cfg(test)]
mod tests {
	use std::sync::Mutex;

	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	use tokio::net::TcpListener;

	use super::*;

	fn http_mirror(base: String) -> Mirror {
		Mirror {
			id: "m".into(),
			name: "test".into(),
			kind: MirrorKind::Http { base_uri: base },
			password: "aGk=".into(),
			headers: vec![super::super::mirror::Header {
				name: "X-Test".into(),
				value: "yes".into(),
			}],
		}
	}

	/// Serves `body` once per connection; honours `Range: bytes=N-`.
	async fn serve(body: &'static [u8]) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
		tokio::spawn(async move {
			while let Ok((mut s, _)) = listener.accept().await {
				let mut req = vec![0u8; 4096];
				let n = s.read(&mut req).await.unwrap();
				let req = String::from_utf8_lossy(&req[..n]).into_owned();
				let range = req
					.lines()
					.find_map(|l| {
						l.to_ascii_lowercase()
							.strip_prefix("range: bytes=")
							.map(str::to_owned)
					})
					.and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
				let _ = tx.send(req);
				let response = match range {
					Some(start) => {
						let rest = &body[start..];
						let mut head = format!(
							"HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							rest.len()
						)
						.into_bytes();
						head.extend_from_slice(rest);
						head
					}
					None => {
						let mut head = format!(
							"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
							body.len()
						)
						.into_bytes();
						head.extend_from_slice(body);
						head
					}
				};
				s.write_all(&response).await.unwrap();
			}
		});
		(format!("http://{addr}/files"), rx)
	}

	#[tokio::test]
	async fn http_download_with_headers_and_progress() {
		let (base, mut requests) = serve(b"0123456789").await;
		let dir = tempfile::tempdir().unwrap();
		let dest = dir.path().join("meta.7z");
		let seen = Mutex::new(Vec::new());
		Fetcher::new(None)
			.unwrap()
			.download_file(&http_mirror(base), "meta.7z", &dest, &|done, total| {
				seen.lock().unwrap().push((done, total));
			})
			.await
			.unwrap();
		assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
		assert!(!part_path(&dest).exists());
		assert_eq!(seen.lock().unwrap().last(), Some(&(10, Some(10))));
		let request = requests.recv().await.unwrap();
		assert!(request.starts_with("GET /files/meta.7z "), "{request}");
		assert!(request.to_ascii_lowercase().contains("x-test: yes"));
	}

	#[tokio::test]
	async fn http_download_resumes_partial_file() {
		let (base, mut requests) = serve(b"0123456789").await;
		let dir = tempfile::tempdir().unwrap();
		let dest = dir.path().join("game.7z.001");
		std::fs::write(part_path(&dest), b"0123").unwrap();
		Fetcher::new(None)
			.unwrap()
			.download_file(&http_mirror(base), "abc/game.7z.001", &dest, &|_, _| {})
			.await
			.unwrap();
		assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
		assert!(
			requests
				.recv()
				.await
				.unwrap()
				.to_ascii_lowercase()
				.contains("range: bytes=4-")
		);
	}

	#[tokio::test]
	async fn http_error_status() {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			let mut buf = [0u8; 1024];
			let _ = s.read(&mut buf).await;
			s.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
				.await
				.unwrap();
		});
		let dir = tempfile::tempdir().unwrap();
		let err = Fetcher::new(None)
			.unwrap()
			.download_file(
				&http_mirror(format!("http://user:pw@{addr}/")),
				"meta.7z",
				&dir.path().join("x"),
				&|_, _| {},
			)
			.await
			.unwrap_err();
		match err {
			CatalogError::Status { status, url } => {
				assert_eq!(status, 403);
				assert!(!url.contains("pw"), "{url}");
			}
			other => panic!("{other:?}"),
		}
	}

	#[test]
	fn url_joining() {
		assert_eq!(
			join_url("https://h/a", "meta.7z").unwrap().as_str(),
			"https://h/a/meta.7z"
		);
		assert_eq!(
			join_url("https://h/a/", "0bee/x.7z.001").unwrap().as_str(),
			"https://h/a/0bee/x.7z.001"
		);
	}

	#[test]
	fn rclone_log_lines() {
		let stats = r#"{"level":"notice","msg":"...","stats":{"bytes":1048576,"totalBytes":4194304,"speed":1.0}}"#;
		assert_eq!(
			parse_rclone_log(stats),
			Some(RcloneLog::Stats {
				bytes: 1_048_576,
				total: Some(4_194_304)
			})
		);
		let error =
			r#"{"level":"error","msg":"Failed to copyto: directory not found\n","source":"x"}"#;
		assert_eq!(
			parse_rclone_log(error),
			Some(RcloneLog::Error(
				"Failed to copyto: directory not found".into()
			))
		);
		assert_eq!(parse_rclone_log(r#"{"level":"info","msg":"hi"}"#), None);
		assert_eq!(parse_rclone_log("not json"), None);
	}

	#[test]
	fn rclone_header_csv() {
		use super::super::mirror::Header;
		let h = |name: &str, value: &str| Header {
			name: name.into(),
			value: value.into(),
		};
		assert_eq!(
			rclone_header_env(&[h("X-Api-Key", "abc")]),
			"X-Api-Key: abc"
		);
		assert_eq!(
			rclone_header_env(&[h("X-Api-Key", "a,b"), h("X-Other", "say \"hi\"")]),
			r#""X-Api-Key: a,b","X-Other: say ""hi""""#
		);
	}

	/// Minimal HTTP server like a key-protected mirror: 403 without
	/// `X-Api-Key: secret`, otherwise serves one file under `/Quest Games/`.
	async fn protected_server(body: &'static [u8]) -> String {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move {
			while let Ok((mut s, _)) = listener.accept().await {
				tokio::spawn(async move {
					let mut req = Vec::new();
					let mut buf = [0u8; 4096];
					while !req.windows(4).any(|w| w == b"\r\n\r\n") {
						match s.read(&mut buf).await {
							Ok(0) | Err(_) => return,
							Ok(n) => req.extend_from_slice(&buf[..n]),
						}
					}
					let req = String::from_utf8_lossy(&req).to_ascii_lowercase();
					let mut first = req.lines().next().unwrap_or("").split(' ');
					let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
					let authorized = req.lines().any(|l| l.trim() == "x-api-key: secret");
					let (status, payload): (&str, &[u8]) = if !authorized {
						("403 Forbidden", b"")
					} else if path == "/quest%20games/meta.7z" {
						("200 OK", body)
					} else if path.ends_with('/') {
						("200 OK", b"<html><a href=\"meta.7z\">meta.7z</a></html>")
					} else {
						("404 Not Found", b"")
					};
					let head = format!(
						"HTTP/1.1 {status}\r\nContent-Length: {}\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\nConnection: close\r\n\r\n",
						payload.len()
					);
					let _ = s.write_all(head.as_bytes()).await;
					if method != "head" {
						let _ = s.write_all(payload).await;
					}
				});
			}
		});
		format!("http://{addr}/")
	}

	/// Real rclone with an `http` remote; skipped when rclone isn't installed.
	#[tokio::test]
	async fn rclone_sends_mirror_headers() {
		if std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_err()
		{
			eprintln!("rclone not installed, skipping");
			return;
		}
		let url = protected_server(b"meta archive bytes").await;
		let dir = tempfile::tempdir().unwrap();
		let config = dir.path().join("rclone.conf");
		std::fs::write(&config, format!("[vrp]\ntype = http\nurl = {url}\n")).unwrap();
		let mut mirror = Mirror {
			id: "r".into(),
			name: "r".into(),
			kind: MirrorKind::Rclone {
				config_file: config,
				remote: "vrp".into(),
			},
			password: "aGk=".into(),
			headers: Vec::new(),
		};
		let fetcher = Fetcher::new(None).unwrap();
		let dest = dir.path().join("meta.7z");

		let denied = fetcher
			.download_file(&mirror, "meta.7z", &dest, &|_, _| {})
			.await;
		assert!(
			denied.is_err(),
			"the server must refuse requests without the key"
		);

		mirror.headers = vec![super::super::mirror::Header {
			name: "X-Api-Key".into(),
			value: "secret".into(),
		}];
		fetcher
			.download_file(&mirror, "meta.7z", &dest, &|_, _| {})
			.await
			.unwrap();
		assert_eq!(std::fs::read(&dest).unwrap(), b"meta archive bytes");
	}
}
