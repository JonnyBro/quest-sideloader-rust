//! Downloading files from a mirror. Everything goes through rclone: HTTP
//! mirrors through its `http` backend, which some mirrors require.

use rust_i18n::t;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, BufReader};

use super::CatalogError;
use super::health::{HealthTracker, MirrorHealth};
use super::mirror::{Mirror, MirrorKind, release_hash};
use crate::mirror_key::MirrorKeys;

/// Receives `(bytes so far, total if known)`.
pub type FetchProgress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

const RCLONE_ROOT: &str = "Quest Games";

/// An empty config, so rclone neither reads nor writes the user's own.
const NO_CONFIG: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

#[derive(Clone)]
pub struct Fetcher {
	rclone: PathBuf,
	/// Total bytes per second for all downloads; `0` is unlimited.
	limit: Arc<AtomicU64>,
	parallel: Arc<AtomicU8>,
	/// Sent as [`crate::mirror_key::HEADER`] to vrSrc's mirror only; never logged.
	keys: MirrorKeys,
	health: HealthTracker,
}

/// How rclone reaches one mirror.
struct Location<'a> {
	config_file: &'a Path,
	/// `remote:path` that relative paths are appended to.
	prefix: String,
	flags: Vec<String>,
	/// For error messages; without credentials.
	shown: String,
}

impl Location<'_> {
	fn path(&self, relative: &str) -> String {
		if self.prefix.ends_with(':') {
			format!("{}{relative}", self.prefix)
		} else {
			format!("{}/{relative}", self.prefix)
		}
	}
}

fn location(mirror: &Mirror) -> Location<'_> {
	match &mirror.kind {
		// No leading slash: rclone's http backend would resolve it against
		// the server root and lose the path in the mirror's URL.
		MirrorKind::Http { base_uri } => Location {
			config_file: Path::new(NO_CONFIG),
			prefix: ":http:".into(),
			flags: vec!["--http-url".into(), base_uri.clone()],
			shown: redact_str(base_uri),
		},
		// VRP mirrors have always been addressed from the remote's root.
		MirrorKind::Rclone {
			config_file,
			remote,
		} => Location {
			config_file,
			prefix: format!("{remote}:/{RCLONE_ROOT}"),
			flags: Vec::new(),
			shown: format!("{remote}:/{RCLONE_ROOT}"),
		},
	}
}

impl Fetcher {
	pub fn new(rclone_path: Option<PathBuf>) -> Result<Self, CatalogError> {
		Ok(Self {
			rclone: rclone_path.unwrap_or_else(|| crate::paths::default_tool("rclone")),
			limit: Arc::new(AtomicU64::new(0)),
			parallel: Arc::new(AtomicU8::new(1)),
			keys: MirrorKeys::default(),
			health: HealthTracker::default(),
		})
	}

	/// Calls `listener` whenever a request to a mirror succeeds or fails.
	pub fn on_health(mut self, listener: impl Fn(MirrorHealth) + Send + Sync + 'static) -> Self {
		self.health = self.health.with_listener(listener);
		self
	}

	/// Keeps what `other` knows about mirrors and keys, with its listener.
	pub fn sharing_state_of(mut self, other: &Fetcher) -> Self {
		self.keys = other.keys.clone();
		self.health = other.health.clone();
		self
	}

	/// What is known about `mirror` so far; sends no request to it.
	pub async fn mirror_health(&self, mirror: &Mirror) -> MirrorHealth {
		let key = self.keys.status_for(mirror).await;
		self.health.snapshot(&mirror.id, key)
	}

	/// Tells the listener what is known about `mirror` so far.
	pub async fn announce_health(&self, mirror: &Mirror) {
		let key = self.keys.status_for(mirror).await;
		self.health.announce(&mirror.id, key);
	}

	async fn record<T>(&self, mirror: &Mirror, result: &Result<T, CatalogError>) {
		let error = match result {
			Ok(_) => None,
			// Problems on this computer, not the mirror's.
			Err(CatalogError::RcloneNotFound(_) | CatalogError::Io { .. }) => return,
			Err(e) => Some(e.to_string()),
		};
		let key = self.keys.status_for(mirror).await;
		self.health.record(&mirror.id, error, key);
	}

	pub fn with_keys(mut self, keys: MirrorKeys) -> Self {
		self.keys = keys;
		self
	}

	pub fn keys(&self) -> &MirrorKeys {
		&self.keys
	}

	/// Caps the combined speed of all downloads; `0` is unlimited. Clones of
	/// this fetcher share the limit. `parallel` is how many downloads may run
	/// at once, which rclone needs to split the limit.
	pub fn set_bandwidth_limit(&self, bytes_per_sec: u64, parallel: u8) {
		self.limit.store(bytes_per_sec, Ordering::Relaxed);
		self.parallel.store(parallel.max(1), Ordering::Relaxed);
	}

	/// Downloads `remote_path` (relative to the mirror root) to `dest`.
	/// `concurrent` is how many files this download fetches at once, for
	/// splitting the bandwidth limit.
	pub async fn download_file(
		&self,
		mirror: &Mirror,
		remote_path: &str,
		dest: &Path,
		progress: FetchProgress<'_>,
		concurrent: usize,
	) -> Result<(), CatalogError> {
		if let Some(parent) = dest.parent() {
			tokio::fs::create_dir_all(parent)
				.await
				.map_err(super::io_err(parent))?;
		}
		let at = location(mirror);
		let copy = |key: Option<&str>| {
			let mut command = self.rclone_command(at.config_file, key);
			command
				.arg("copyto")
				.arg(at.path(remote_path))
				.arg(dest)
				.args(&at.flags)
				// One connection per file: rclone splits files over 256M into
				// parallel streams otherwise.
				.args(["--multi-thread-streams", "0"])
				.args(["--retries", "3", "--low-level-retries", "10"])
				.args(self.rclone_bwlimit(concurrent));
			command
		};
		let key = self.keys.key_for(mirror).await;
		let result = self
			.run_rclone(copy(key.as_deref()), progress)
			.await
			.map_err(|e| with_status(e, &at.shown));
		let result = match self.renewed_key(mirror, key, &result).await {
			Some(fresh) => self
				.run_rclone(copy(Some(&fresh)), progress)
				.await
				.map_err(|e| with_status(e, &at.shown)),
			None => result,
		};
		self.record(mirror, &result).await;
		result
	}

	/// After a 403 to a key: a different key to try once, if there is one.
	async fn renewed_key<T>(
		&self,
		mirror: &Mirror,
		sent: Option<String>,
		result: &Result<T, CatalogError>,
	) -> Option<String> {
		let sent = sent?;
		if !matches!(result, Err(CatalogError::Status { status: 403, .. })) {
			return None;
		}
		self.keys.refused(&sent).await;
		let fresh = self.keys.key_for(mirror).await?;
		(fresh != sent).then_some(fresh)
	}

	/// Uploads one file into `dir` (empty for the root) of `remote`. The upload
	/// config points at the mirror's own upload server, which doesn't get the key.
	pub async fn rclone_upload(
		&self,
		access: &RcloneAccess<'_>,
		remote: &str,
		dir: &str,
		file: &Path,
		progress: FetchProgress<'_>,
	) -> Result<(), CatalogError> {
		let mut command = self.rclone_command(access.config_file, None);
		command
			.arg("copy")
			.arg(file)
			.arg(format!("{remote}:{}", dir.trim_matches('/')))
			.args(["--checkers", "1", "--retries", "2"])
			.env("RCLONE_INPLACE", "true");
		self.run_rclone(command, progress).await
	}

	/// Files in a release's folder on the mirror.
	pub async fn list_release(
		&self,
		mirror: &Mirror,
		release_name: &str,
	) -> Result<Vec<RemoteFile>, CatalogError> {
		let at = location(mirror);
		let folder = match mirror.kind {
			MirrorKind::Http { .. } => release_hash(release_name),
			MirrorKind::Rclone { .. } => release_name.to_owned(),
		};
		let key = self.keys.key_for(mirror).await;
		let result = self.lsjson(&at, &folder, key.as_deref()).await;
		let result = match self.renewed_key(mirror, key, &result).await {
			Some(fresh) => self.lsjson(&at, &folder, Some(&fresh)).await,
			None => result,
		};
		self.record(mirror, &result).await;
		result
	}

	async fn lsjson(
		&self,
		at: &Location<'_>,
		folder: &str,
		key: Option<&str>,
	) -> Result<Vec<RemoteFile>, CatalogError> {
		let mut command = self.rclone_command(at.config_file, key);
		command
			.arg("lsjson")
			.arg("--files-only")
			.arg(at.path(&format!("{folder}/")))
			.args(&at.flags)
			.stdout(Stdio::piped())
			.stderr(Stdio::piped());
		tracing::debug!(command = %describe_command(&command), "listing a release");
		let output = command
			.output()
			.await
			.map_err(|e| Self::spawn_error(&command, e))?;
		if !output.status.success() {
			let stderr = String::from_utf8_lossy(&output.stderr);
			let message = stderr
				.lines()
				.last()
				.unwrap_or("lsjson failed")
				.trim()
				.to_owned();
			return Err(with_status(CatalogError::Rclone(message), &at.shown));
		}
		parse_lsjson(&output.stdout)
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
		if cfg!(debug_assertions) {
			command.arg("-v");
		}
		tracing::debug!(command = %describe_command(&command), "running rclone");
		let mut child = command
			.spawn()
			.map_err(|e| Self::spawn_error(&command, e))?;

		let stderr = child.stderr.take().expect("stderr is piped");
		let mut lines = BufReader::new(stderr).lines();
		let mut last_error = None;
		let mut plain_error = None;
		while let Some(line) = lines
			.next_line()
			.await
			.map_err(|e| CatalogError::Rclone(e.to_string()))?
		{
			match parse_rclone_log(&line) {
				Some(RcloneLog::Stats { bytes, total }) => progress(bytes, total),
				Some(RcloneLog::Error(message)) => {
					tracing::debug!(%message, "rclone error");
					last_error = Some(message);
				}
				None => {
					tracing::debug!(line = %rclone_log_text(&line), "rclone");
					// Startup errors such as an unknown flag aren't JSON.
					if !line.trim_start().starts_with('{') && !line.trim().is_empty() {
						plain_error = Some(line.trim().to_owned());
					}
				}
			}
		}
		let status = child
			.wait()
			.await
			.map_err(|e| CatalogError::Rclone(e.to_string()))?;
		tracing::debug!(%status, "rclone finished");
		if status.success() {
			Ok(())
		} else {
			Err(CatalogError::Rclone(
				last_error
					.or(plain_error)
					.unwrap_or_else(|| t!("cat.rclone_exit", status = status).into_owned()),
			))
		}
	}

	fn rclone_command(&self, config_file: &Path, key: Option<&str>) -> tokio::process::Command {
		let mut command = tokio::process::Command::new(&self.rclone);
		// Through the environment rather than `--header`, so the key doesn't
		// show up in other users' process lists.
		if let Some(key) = key {
			command.env(
				"RCLONE_HEADER",
				rclone_header_env(crate::mirror_key::HEADER, key),
			);
		}
		command
			.arg("--config")
			.arg(config_file)
			// Never stop to ask for a config password on a terminal.
			.arg("--ask-password=false")
			// Same request pacing as other clients of these mirrors.
			.args(["--tpslimit", "1", "--tpslimit-burst", "3"])
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
	/// limit divided by the most that may run at once: `concurrent` files per
	/// download, times the downloads. The `B` suffix keeps it in plain bytes,
	/// whatever unit the user picked.
	fn rclone_bwlimit(&self, concurrent: usize) -> Vec<String> {
		let parallel =
			u64::from(self.parallel.load(Ordering::Relaxed).max(1)) * (concurrent.max(1) as u64);
		match self.limit.load(Ordering::Relaxed) {
			0 => Vec::new(),
			bytes => vec![
				"--bwlimit".into(),
				format!("{}B", (bytes / parallel).max(1)),
			],
		}
	}

	fn spawn_error(command: &tokio::process::Command, e: std::io::Error) -> CatalogError {
		if e.kind() == std::io::ErrorKind::NotFound {
			CatalogError::RcloneNotFound(PathBuf::from(command.as_std().get_program()))
		} else {
			CatalogError::Rclone(e.to_string())
		}
	}
}

/// rclone reports refused requests as text (`HTTP Error: 403 Forbidden`);
/// they become statuses so they aren't retried and get a clear message.
fn with_status(e: CatalogError, shown: &str) -> CatalogError {
	let CatalogError::Rclone(message) = &e else {
		return e;
	};
	let status = message
		.split("HTTP Error: ")
		.nth(1)
		.and_then(|rest| rest.get(..3))
		.and_then(|code| code.parse::<u16>().ok());
	match status {
		Some(status) => CatalogError::Status {
			status,
			url: shown.to_owned(),
		},
		None => e,
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
	pub name: String,
	pub size: Option<u64>,
}

/// Some rclone builds leave fields out or spell them differently, so every
/// field is optional and the path falls back to the name.
fn parse_lsjson(stdout: &[u8]) -> Result<Vec<RemoteFile>, CatalogError> {
	#[derive(serde::Deserialize)]
	struct Entry {
		/// Relative to the listed folder, so it includes subfolders when recursive.
		#[serde(default, alias = "path")]
		#[serde(rename = "Path")]
		path: Option<String>,
		#[serde(default, alias = "name")]
		#[serde(rename = "Name")]
		name: Option<String>,
		#[serde(default, alias = "size")]
		#[serde(rename = "Size")]
		size: Option<i64>,
		#[serde(default, alias = "isDir", alias = "is_dir")]
		#[serde(rename = "IsDir")]
		is_dir: bool,
	}
	let entries: Vec<Entry> = serde_json::from_slice(stdout).map_err(|e| {
		let start = String::from_utf8_lossy(&stdout[..stdout.len().min(400)]).into_owned();
		tracing::debug!(output = %start, "unreadable lsjson output");
		CatalogError::Rclone(format!("unexpected lsjson output: {e}"))
	})?;
	Ok(entries
		.into_iter()
		.filter(|e| !e.is_dir)
		.filter_map(|e| {
			Some(RemoteFile {
				name: e.path.or(e.name)?,
				size: e.size.and_then(|s| u64::try_from(s).ok()),
			})
		})
		.collect())
}

/// rclone reads multi-value flags from the environment as one CSV line.
fn rclone_header_env(name: &str, value: &str) -> String {
	let field = format!("{name}: {value}");
	if field.contains([',', '"', '\n', '\r']) {
		format!("\"{}\"", field.replace('"', "\"\""))
	} else {
		field
	}
}

/// Path of a release file relative to the mirror root, as `download_file` wants it.
pub fn release_file_path(mirror: &Mirror, release_name: &str, file: &str) -> String {
	match mirror.kind {
		MirrorKind::Http { .. } => format!("{}/{file}", release_hash(release_name)),
		MirrorKind::Rclone { .. } => format!("{release_name}/{file}"),
	}
}

/// How to run rclone for an upload config.
#[derive(Debug, Clone, Copy)]
pub struct RcloneAccess<'a> {
	pub config_file: &'a Path,
}

/// Like [`redact`] for a URL that may not parse.
fn redact_str(url: &str) -> String {
	match reqwest::Url::parse(url) {
		Ok(parsed) => redact(&parsed),
		Err(_) => url.split('?').next().unwrap_or_default().to_owned(),
	}
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

/// The level and message of a JSON log line, or the line as it is.
fn rclone_log_text(line: &str) -> String {
	let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
		return line.trim().to_owned();
	};
	let level = value.get("level").and_then(|v| v.as_str()).unwrap_or("?");
	let msg = value
		.get("msg")
		.and_then(|v| v.as_str())
		.unwrap_or_default()
		.trim();
	let object = value.get("object").and_then(|v| v.as_str());
	match object {
		Some(object) => format!("{level}: {object}: {msg}"),
		None => format!("{level}: {msg}"),
	}
}

/// Program and arguments for the log. The key travels in the environment and
/// is only mentioned, never shown.
fn describe_command(command: &tokio::process::Command) -> String {
	let std = command.as_std();
	let mut text = std.get_program().to_string_lossy().into_owned();
	for arg in std.get_args() {
		text.push(' ');
		let arg = arg.to_string_lossy();
		if arg.contains(' ') {
			text.push_str(&format!("{arg:?}"));
		} else {
			text.push_str(&arg);
		}
	}
	if std
		.get_envs()
		.any(|(k, v)| k == "RCLONE_HEADER" && v.is_some())
	{
		text.push_str(" [with RCLONE_HEADER]");
	}
	text
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
	let msg = value.get("msg")?.as_str()?.trim();
	// A failed `copyto` ends with a notice ("Failed to copyto: ...") and may
	// log nothing at error level.
	if level == "error"
		|| level == "critical"
		|| (level == "notice" && msg.starts_with("Failed to"))
	{
		return Some(RcloneLog::Error(msg.to_owned()));
	}
	None
}

#[cfg(test)]
pub(crate) mod tests {
	use tokio::io::{AsyncReadExt, AsyncWriteExt};
	use tokio::net::TcpListener;

	use super::*;

	fn http_mirror(base: String) -> Mirror {
		Mirror {
			id: "m".into(),
			name: "test".into(),
			kind: MirrorKind::Http { base_uri: base },
			password: "aGk=".into(),
		}
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
		let failed = r#"{"level":"notice","msg":"Failed to copyto: failed to stat: HTTP Error: 403 Forbidden"}"#;
		assert!(matches!(
			parse_rclone_log(failed),
			Some(RcloneLog::Error(_))
		));
		assert_eq!(parse_rclone_log(r#"{"level":"info","msg":"hi"}"#), None);
		assert_eq!(parse_rclone_log("not json"), None);
	}

	#[test]
	fn refused_requests_become_statuses() {
		let refused = CatalogError::Rclone("failed to stat: HTTP Error: 403 Forbidden".into());
		assert!(matches!(
			with_status(refused, "https://m/"),
			CatalogError::Status { status: 403, .. }
		));
		let other = CatalogError::Rclone("directory not found".into());
		assert!(matches!(with_status(other, "x"), CatalogError::Rclone(_)));
	}

	#[test]
	fn rclone_header_csv() {
		assert_eq!(rclone_header_env("X-API-Key", "abc"), "X-API-Key: abc");
		assert_eq!(
			rclone_header_env("X-API-Key", "a,\"b\""),
			r#""X-API-Key: a,""b""""#
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

	/// Where an rclone remote points isn't known, so it never gets the key;
	/// skipped when rclone isn't installed.
	#[tokio::test]
	async fn rclone_remote_never_gets_the_key() {
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
		let mirror = Mirror {
			id: "r".into(),
			name: "r".into(),
			kind: MirrorKind::Rclone {
				config_file: config,
				remote: "vrp".into(),
			},
			password: "aGk=".into(),
		};
		let dest = dir.path().join("meta.7z");

		let origin = url.trim_end_matches('/');
		let denied = Fetcher::new(None)
			.unwrap()
			.with_keys(MirrorKeys::fixed("secret", origin))
			.download_file(&mirror, "meta.7z", &dest, &|_, _| {}, 1)
			.await;
		assert!(denied.is_err(), "the key must not go to rclone remotes");
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
			"0bee89b07a248e27c83fc3d5951213c1/x y.7z.001"
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

	pub(crate) fn have_rclone() -> bool {
		let found = std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_ok();
		if !found {
			eprintln!("rclone not installed, skipping");
		}
		found
	}

	/// A key-protected HTTP mirror under `/files/`: `meta.7z` and one release
	/// folder with an autoindex page; 403 without `X-API-Key: secret`.
	async fn http_mirror_server() -> String {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let hash = release_hash("Game v1");
		tokio::spawn(async move {
			while let Ok((mut s, _)) = listener.accept().await {
				let hash = hash.clone();
				tokio::spawn(async move {
					let mut req = Vec::new();
					let mut buf = [0u8; 4096];
					while !req.windows(4).any(|w| w == b"\r\n\r\n") {
						match s.read(&mut buf).await {
							Ok(0) | Err(_) => return,
							Ok(n) => req.extend_from_slice(&buf[..n]),
						}
					}
					let req = String::from_utf8_lossy(&req).into_owned();
					let mut first = req.lines().next().unwrap_or("").split(' ');
					let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
					let authorized = req
						.lines()
						.any(|l| l.to_ascii_lowercase().trim() == "x-api-key: secret");
					let listing = "<a href=\"Game%20v1.7z.001\">Game v1.7z.001</a>".to_string();
					let (status, payload): (&str, Vec<u8>) = if !authorized {
						("403 Forbidden", Vec::new())
					} else if path == "/files/meta.7z" {
						("200 OK", b"meta bytes".to_vec())
					} else if path == format!("/files/{hash}/") {
						("200 OK", listing.into_bytes())
					} else if path == format!("/files/{hash}/Game%20v1.7z.001") {
						("200 OK", b"part one".to_vec())
					} else {
						("404 Not Found", Vec::new())
					};
					let kind = if path.ends_with('/') {
						"text/html"
					} else {
						"application/octet-stream"
					};
					let head = format!(
						"HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {kind}\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\nConnection: close\r\n\r\n",
						payload.len()
					);
					let _ = s.write_all(head.as_bytes()).await;
					if method != "HEAD" {
						let _ = s.write_all(&payload).await;
					}
				});
			}
		});
		format!("http://{addr}/files/")
	}

	/// HTTP mirrors go through rclone's http backend, with the key in
	/// `RCLONE_HEADER` for the key's own mirror only; skipped when rclone
	/// isn't installed.
	#[tokio::test]
	async fn http_mirror_goes_through_rclone() {
		if !have_rclone() {
			return;
		}
		let base = http_mirror_server().await;
		let origin = reqwest::Url::parse(&base)
			.unwrap()
			.origin()
			.ascii_serialization();
		let mirror = http_mirror(base);
		let dir = tempfile::tempdir().unwrap();
		let dest = dir.path().join("meta.7z");

		for keys in [
			MirrorKeys::default(),
			MirrorKeys::fixed("secret", "https://go.srcdl1.xyz"),
		] {
			let refused = Fetcher::new(None)
				.unwrap()
				.with_keys(keys)
				.download_file(&mirror, "meta.7z", &dest, &|_, _| {}, 1)
				.await;
			assert!(
				matches!(refused, Err(CatalogError::Status { status: 403, .. })),
				"{refused:?}"
			);
		}

		let fetcher = Fetcher::new(None)
			.unwrap()
			.with_keys(MirrorKeys::fixed("secret", &origin));
		fetcher
			.download_file(&mirror, "meta.7z", &dest, &|_, _| {}, 1)
			.await
			.unwrap();
		assert_eq!(std::fs::read(&dest).unwrap(), b"meta bytes");

		let files = fetcher.list_release(&mirror, "Game v1").await.unwrap();
		assert_eq!(files.len(), 1, "{files:?}");
		assert_eq!(files[0].name, "Game v1.7z.001");
		let part = dir.path().join("part");
		fetcher
			.download_file(
				&mirror,
				&release_file_path(&mirror, "Game v1", &files[0].name),
				&part,
				&|_, _| {},
				1,
			)
			.await
			.unwrap();
		assert_eq!(std::fs::read(&part).unwrap(), b"part one");
	}

	#[test]
	fn rclone_limit_is_split_in_bytes() {
		let fetcher = Fetcher::new(None).unwrap();
		assert!(fetcher.rclone_bwlimit(1).is_empty());
		fetcher.set_bandwidth_limit(3_000_000, 3);
		assert_eq!(fetcher.rclone_bwlimit(1), ["--bwlimit", "1000000B"]);
		assert_eq!(
			fetcher.rclone_bwlimit(4),
			["--bwlimit", "250000B"],
			"split between parts too"
		);
		fetcher.clone().set_bandwidth_limit(1000, 0);
		assert_eq!(
			fetcher.rclone_bwlimit(1),
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
			.rclone_upload(
				&RcloneAccess {
					config_file: &config,
				},
				"RSL-gameuploads",
				"",
				&file,
				&|_, _| {},
			)
			.await
			.unwrap();
		assert_eq!(
			std::fs::read(inbox.join("Game v1 com.x.y 1 Quest 3S.zip")).unwrap(),
			b"zip bytes"
		);
	}

	#[test]
	fn lsjson_without_path() {
		let json = br#"[{"Name":"a.7z.001","Size":5,"IsDir":false,"ModTime":"x"},
			{"Name":"sub","IsDir":true},
			{"path":"c/d.obb","size":7}]"#;
		assert_eq!(
			parse_lsjson(json).unwrap(),
			[
				RemoteFile {
					name: "a.7z.001".into(),
					size: Some(5)
				},
				RemoteFile {
					name: "c/d.obb".into(),
					size: Some(7)
				},
			]
		);
	}
}
