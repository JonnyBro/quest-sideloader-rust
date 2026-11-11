//! Downloading files from a mirror, over HTTP or through rclone.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::mirror::{Mirror, MirrorKind, release_hash};
use super::{CatalogError, io_err};

/// Receives `(bytes so far, total if known)`.
pub type FetchProgress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

const RCLONE_ROOT: &str = "Quest Games";

#[derive(Clone)]
pub struct Fetcher {
	http: reqwest::Client,
	rclone: PathBuf,
	throttle: std::sync::Arc<super::throttle::Throttle>,
	rclone_parallel: std::sync::Arc<std::sync::atomic::AtomicU8>,
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
			throttle: std::sync::Arc::new(super::throttle::Throttle::new()),
			rclone_parallel: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(1)),
		})
	}

	/// Caps the combined speed of all downloads; `0` is unlimited. Clones of
	/// this fetcher share the limit. `parallel` is how many downloads may run
	/// at once, which rclone needs to split the limit.
	pub fn set_bandwidth_limit(&self, bytes_per_sec: u64, parallel: u8) {
		self.throttle.set(bytes_per_sec);
		self.rclone_parallel
			.store(parallel.max(1), std::sync::atomic::Ordering::Relaxed);
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
			self.throttle.take(chunk.len() as u64).await;
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
		let mut command = self.rclone_command(config_file, headers);
		command
			.arg("copyto")
			.arg(source)
			.arg(dest)
			.args(["--retries", "3", "--low-level-retries", "10"])
			.args(self.rclone_bwlimit());
		self.run_rclone(command, progress).await
	}

	/// Uploads one file into the root of `remote` defined in `config_file`.
	pub async fn rclone_upload(
		&self,
		config_file: &Path,
		remote: &str,
		file: &Path,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		let mut command = self.rclone_command(config_file, &[]);
		command
			.arg("copy")
			.arg(file)
			.arg(format!("{remote}:"))
			.args(["--checkers", "1", "--retries", "2", "--inplace"]);
		self.run_rclone(command, progress).await
	}

	/// Runs rclone with JSON logs, reporting its transfer stats as progress.
	async fn run_rclone(
		&self,
		mut command: tokio::process::Command,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		command
			.args([
				"--use-json-log",
				"--stats",
				"1s",
				"--stats-log-level",
				"NOTICE",
			])
			.stdout(Stdio::null())
			.stderr(Stdio::piped());
		let mut child = command.spawn().map_err(|e| self.spawn_error(e))?;

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

impl Fetcher {
	fn rclone_command(
		&self,
		config_file: &Path,
		headers: &[super::mirror::Header],
	) -> tokio::process::Command {
		let mut command = tokio::process::Command::new(&self.rclone);
		// Through the environment rather than `--header`, so keys don't show up
		// in other users' process lists.
		if !headers.is_empty() {
			command.env("RCLONE_HEADER", rclone_header_env(headers));
		}
		command
			.arg("--config")
			.arg(config_file)
			// Keep request bursts small; mirrors ask clients to be gentle.
			.args(["--tpslimit", "2", "--tpslimit-burst", "4"])
			.stdin(Stdio::null())
			.kill_on_drop(true);
		#[cfg(windows)]
		{
			const CREATE_NO_WINDOW: u32 = 0x0800_0000;
			command.creation_flags(CREATE_NO_WINDOW);
		}
		command
	}

	/// rclone can't share a limit between processes, so each one gets the full
	/// limit divided by the most downloads that may run at once. The `B`
	/// suffix keeps it in plain bytes, whatever unit the user picked.
	fn rclone_bwlimit(&self) -> Vec<String> {
		let parallel = u64::from(
			self.rclone_parallel
				.load(std::sync::atomic::Ordering::Relaxed)
				.max(1),
		);
		match self.throttle.limit() {
			0 => Vec::new(),
			bytes => vec![
				"--bwlimit".into(),
				format!("{}B", (bytes / parallel).max(1)),
			],
		}
	}

	fn spawn_error(&self, e: std::io::Error) -> CatalogError {
		if e.kind() == std::io::ErrorKind::NotFound {
			CatalogError::RcloneNotFound(self.rclone.clone())
		} else {
			CatalogError::Rclone(e.to_string())
		}
	}

	/// Files in a release's folder on the mirror.
	pub async fn list_release(
		&self,
		mirror: &Mirror,
		release_name: &str,
	) -> Result<Vec<RemoteFile>, CatalogError> {
		match &mirror.kind {
			MirrorKind::Http { base_uri } => {
				let url = join_url(base_uri, &format!("{}/", release_hash(release_name)))?;
				let mut request = self.http.get(url.clone());
				for header in &mirror.headers {
					request = request.header(&header.name, &header.value);
				}
				let response = request
					.send()
					.await
					.map_err(|e| CatalogError::Http(e.to_string()))?;
				let status = response.status();
				if !status.is_success() {
					return Err(CatalogError::Status {
						status: status.as_u16(),
						url: redact(&url),
					});
				}
				let html = response
					.text()
					.await
					.map_err(|e| CatalogError::Http(e.to_string()))?;
				Ok(parse_index(&url, &html))
			}
			MirrorKind::Rclone {
				config_file,
				remote,
			} => {
				let mut command = self.rclone_command(config_file, &mirror.headers);
				command
					.arg("lsjson")
					.arg("--files-only")
					.arg(format!("{remote}:/{RCLONE_ROOT}/{release_name}/"))
					.stdout(Stdio::piped())
					.stderr(Stdio::piped());
				let output = command.output().await.map_err(|e| self.spawn_error(e))?;
				if !output.status.success() {
					let stderr = String::from_utf8_lossy(&output.stderr);
					return Err(CatalogError::Rclone(
						stderr
							.lines()
							.last()
							.unwrap_or("lsjson failed")
							.trim()
							.to_owned(),
					));
				}
				parse_lsjson(&output.stdout)
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
	pub name: String,
	pub size: Option<u64>,
}

/// Path of a release file relative to the mirror root, as `download_file` wants it.
pub fn release_file_path(mirror: &Mirror, release_name: &str, file: &str) -> String {
	match mirror.kind {
		MirrorKind::Http { .. } => format!(
			"{}/{}",
			release_hash(release_name),
			percent_encoding::utf8_percent_encode(file, PATH_SEGMENT)
		),
		MirrorKind::Rclone { .. } => format!("{release_name}/{file}"),
	}
}

const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
	.remove(b'-')
	.remove(b'.')
	.remove(b'_')
	.remove(b'~');

/// Direct child files linked from an autoindex page (nginx, Apache, rclone serve).
fn parse_index(listing: &reqwest::Url, html: &str) -> Vec<RemoteFile> {
	let mut files = Vec::new();
	let mut rest = html;
	while let Some(pos) = rest.find("href=") {
		rest = &rest[pos + 5..];
		let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
			continue;
		};
		let Some(end) = rest[1..].find(quote) else {
			break;
		};
		let href = rest[1..=end].replace("&amp;", "&");
		rest = &rest[end + 1..];

		let Ok(url) = listing.join(&href) else {
			continue;
		};
		if url.origin() != listing.origin() || url.query().is_some() {
			continue;
		}
		let Some(name) = url.path().strip_prefix(listing.path()) else {
			continue;
		};
		if name.is_empty() || name.contains('/') {
			continue;
		}
		let name = percent_encoding::percent_decode_str(name)
			.decode_utf8_lossy()
			.into_owned();
		if !files.iter().any(|f: &RemoteFile| f.name == name) {
			files.push(RemoteFile { name, size: None });
		}
	}
	files
}

fn parse_lsjson(stdout: &[u8]) -> Result<Vec<RemoteFile>, CatalogError> {
	#[derive(serde::Deserialize)]
	#[serde(rename_all = "PascalCase")]
	struct Entry {
		name: String,
		size: i64,
		#[serde(default)]
		is_dir: bool,
	}
	let entries: Vec<Entry> = serde_json::from_slice(stdout)
		.map_err(|e| CatalogError::Rclone(format!("unexpected lsjson output: {e}")))?;
	Ok(entries
		.into_iter()
		.filter(|e| !e.is_dir)
		.map(|e| RemoteFile {
			name: e.name,
			size: u64::try_from(e.size).ok(),
		})
		.collect())
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

	#[test]
	fn autoindex_listing() {
		let listing = reqwest::Url::parse("https://m.example/files/0bee/").unwrap();
		let html = r#"<html><body><h1>Index of /files/0bee/</h1>
			<a href="../">../</a>
			<a href="0bee.7z.001">0bee.7z.001</a> 01-Jan-2024 1073741824
			<a href='0bee.7z.002'>0bee.7z.002</a>
			<a href="/files/0bee/release%20notes.txt">release notes.txt</a>
			<a href="sub/">sub/</a>
			<a href="?C=M;O=A">Last modified</a>
			<a href="https://evil.example/x.7z.003">x</a>
			<a href="0bee.7z.001">duplicate</a>
			</body></html>"#;
		let names: Vec<_> = parse_index(&listing, html)
			.into_iter()
			.map(|f| f.name)
			.collect();
		assert_eq!(names, ["0bee.7z.001", "0bee.7z.002", "release notes.txt"]);
	}

	#[test]
	fn lsjson_output() {
		let json = br#"[{"Path":"a.7z.001","Name":"a.7z.001","Size":1048576,"IsDir":false},
			{"Path":"d","Name":"d","Size":-1,"IsDir":true}]"#;
		assert_eq!(
			parse_lsjson(json).unwrap(),
			[RemoteFile {
				name: "a.7z.001".into(),
				size: Some(1_048_576)
			}]
		);
		assert!(parse_lsjson(b"oops").is_err());
	}

	#[test]
	fn release_paths() {
		let http = http_mirror("https://m/".into());
		assert_eq!(
			release_file_path(&http, "abc", "x y.7z.001"),
			"0bee89b07a248e27c83fc3d5951213c1/x%20y.7z.001"
		);
		let rclone = Mirror {
			kind: MirrorKind::Rclone {
				config_file: "/c".into(),
				remote: "r".into(),
			},
			..http
		};
		assert_eq!(
			release_file_path(&rclone, "Game v1+2", "a.7z.001"),
			"Game v1+2/a.7z.001"
		);
	}

	#[tokio::test]
	async fn lists_http_release_folder() {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let (tx, rx) = tokio::sync::oneshot::channel();
		tokio::spawn(async move {
			let (mut s, _) = listener.accept().await.unwrap();
			let mut buf = vec![0u8; 4096];
			let n = s.read(&mut buf).await.unwrap();
			let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
			let body = r#"<a href="0bee.7z.001">x</a><a href="0bee.7z.002">y</a>"#;
			let head = format!(
				"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
				body.len()
			);
			s.write_all(head.as_bytes()).await.unwrap();
			s.write_all(body.as_bytes()).await.unwrap();
		});
		let files = Fetcher::new(None)
			.unwrap()
			.list_release(&http_mirror(format!("http://{addr}/files")), "abc")
			.await
			.unwrap();
		assert_eq!(files.len(), 2);
		let request = rx.await.unwrap();
		assert!(
			request.starts_with("GET /files/0bee89b07a248e27c83fc3d5951213c1/ "),
			"{request}"
		);
		assert!(request.to_ascii_lowercase().contains("x-test: yes"));
	}

	#[tokio::test]
	async fn lists_rclone_release_folder() {
		if std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_err()
		{
			eprintln!("rclone not installed, skipping");
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let release = dir.path().join("server/Quest Games/Game v1+2");
		std::fs::create_dir_all(release.join("nested")).unwrap();
		std::fs::write(release.join("h.7z.001"), b"12345").unwrap();
		std::fs::write(release.join("h.7z.002"), b"67").unwrap();
		let config = dir.path().join("rclone.conf");
		std::fs::write(
			&config,
			format!(
				"[vrp]\ntype = alias\nremote = {}\n",
				dir.path().join("server").display()
			),
		)
		.unwrap();
		let mirror = Mirror {
			kind: MirrorKind::Rclone {
				config_file: config,
				remote: "vrp".into(),
			},
			headers: Vec::new(),
			..http_mirror("https://unused/".into())
		};
		let mut files = Fetcher::new(None)
			.unwrap()
			.list_release(&mirror, "Game v1+2")
			.await
			.unwrap();
		files.sort_by(|a, b| a.name.cmp(&b.name));
		assert_eq!(
			files,
			[
				RemoteFile {
					name: "h.7z.001".into(),
					size: Some(5)
				},
				RemoteFile {
					name: "h.7z.002".into(),
					size: Some(2)
				},
			]
		);
	}

	#[test]
	fn rclone_limit_is_split_in_bytes() {
		let fetcher = Fetcher::new(None).unwrap();
		assert!(fetcher.rclone_bwlimit().is_empty());
		fetcher.set_bandwidth_limit(3_000_000, 3);
		assert_eq!(fetcher.rclone_bwlimit(), ["--bwlimit", "1000000B"]);
		fetcher.clone().set_bandwidth_limit(1000, 0);
		assert_eq!(
			fetcher.rclone_bwlimit(),
			["--bwlimit", "1000B"],
			"clones share the limit"
		);
	}

	/// Real rclone uploading to a local alias remote; skipped without rclone.
	#[tokio::test]
	async fn rclone_upload_copies_file() {
		if std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_err()
		{
			eprintln!("rclone not installed, skipping");
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let inbox = dir.path().join("inbox");
		std::fs::create_dir_all(&inbox).unwrap();
		let config = dir.path().join("upload.config");
		std::fs::write(
			&config,
			format!(
				"[RSL-gameuploads]\ntype = alias\nremote = {}\n",
				inbox.display()
			),
		)
		.unwrap();
		let file = dir.path().join("Game v1 com.x.y 1 Quest 3S.zip");
		std::fs::write(&file, b"zip bytes").unwrap();
		Fetcher::new(None)
			.unwrap()
			.rclone_upload(&config, "RSL-gameuploads", &file, &|_, _| {})
			.await
			.unwrap();
		assert_eq!(
			std::fs::read(inbox.join("Game v1 com.x.y 1 Quest 3S.zip")).unwrap(),
			b"zip bytes"
		);
	}
}
