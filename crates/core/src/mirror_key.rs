//! Short-lived mirror keys from vrSrc's `/app-key`. Only official builds get
//! one: `QSR_CLIENT_ID` and `QSR_BUILD_SECRET` are read at build time. The
//! key goes to vrSrc's mirror only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::catalog::{Mirror, MirrorKind};

pub const HEADER: &str = "X-API-Key";

const ENDPOINT: &str = "https://vrsrc.fyi/api/mirror/app-key";
/// The key is sent to this origin only.
const KEY_ORIGIN: &str = "https://go.srcdl1.xyz";

const CLIENT_ID: Option<&str> = option_env!("QSR_CLIENT_ID");
const BUILD_SECRET: Option<&str> = option_env!("QSR_BUILD_SECRET");

/// The endpoint rate-limits requests per address.
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const RETRY_PENDING: Duration = Duration::from_secs(60);
const RETRY_REFUSED: Duration = Duration::from_secs(60 * 60);
const RETRY_FAILED: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
	/// The mirror isn't the one the key is for.
	NotUsed,
	/// The key's mirror, but this build can't get a key.
	NotInBuild,
	NotFetched,
	/// Unix seconds, if known.
	Ready {
		expires_at: Option<u64>,
	},
	Failed(String),
}

/// Clones share one cache. The default has no key.
#[derive(Clone, Default)]
pub struct MirrorKeys(Option<Arc<Inner>>);

struct Inner {
	source: Source,
	state: tokio::sync::Mutex<State>,
}

enum Source {
	Remote {
		client_id: String,
		build_secret: String,
		cache_file: Option<PathBuf>,
	},
	#[cfg(test)]
	Fixed { key: String, origin: String },
}

#[derive(Default)]
struct State {
	loaded: bool,
	install_id: String,
	key: Option<CachedKey>,
	/// The mirror refused the cached key.
	stale: bool,
	next_try: Option<Instant>,
	/// Cleared by a success.
	last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedKey {
	api_key: String,
	/// Unix seconds.
	refresh_after: u64,
	expires_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
	#[serde(default)]
	install_id: String,
	#[serde(default)]
	key: Option<CachedKey>,
}

impl MirrorKeys {
	/// No keys for builds made without the client ID and build secret.
	pub fn official(cache_file: Option<PathBuf>) -> Self {
		let (Some(client_id), Some(build_secret)) = (
			CLIENT_ID.map(str::trim).filter(|s| !s.is_empty()),
			BUILD_SECRET.map(str::trim).filter(|s| !s.is_empty()),
		) else {
			return Self::default();
		};
		Self::with_source(Source::Remote {
			client_id: client_id.to_owned(),
			build_secret: build_secret.to_owned(),
			cache_file,
		})
	}

	#[cfg(test)]
	pub(crate) fn fixed(key: &str, origin: &str) -> Self {
		Self::with_source(Source::Fixed {
			key: key.to_owned(),
			origin: origin.to_owned(),
		})
	}

	fn with_source(source: Source) -> Self {
		Self(Some(Arc::new(Inner {
			source,
			state: tokio::sync::Mutex::new(State::default()),
		})))
	}

	pub fn available(&self) -> bool {
		self.0.is_some()
	}

	pub fn is_key_mirror(&self, mirror: &Mirror) -> bool {
		let origin = match self.0.as_deref().map(|inner| &inner.source) {
			#[cfg(test)]
			Some(Source::Fixed { origin, .. }) => origin.as_str(),
			_ => KEY_ORIGIN,
		};
		mirror_origin(mirror).is_some_and(|o| o == origin)
	}

	pub fn sends_to(&self, mirror: &Mirror) -> bool {
		self.available() && self.is_key_mirror(mirror)
	}

	pub async fn key_for(&self, mirror: &Mirror) -> Option<String> {
		if !self.sends_to(mirror) {
			return None;
		}
		self.current().await
	}

	pub async fn status_for(&self, mirror: &Mirror) -> KeyStatus {
		if !self.is_key_mirror(mirror) {
			return KeyStatus::NotUsed;
		}
		let Some(inner) = &self.0 else {
			return KeyStatus::NotInBuild;
		};
		let cache_file = match &inner.source {
			Source::Remote { cache_file, .. } => cache_file.as_deref(),
			#[cfg(test)]
			Source::Fixed { .. } => return KeyStatus::Ready { expires_at: None },
		};
		let mut state = inner.state.lock().await;
		if !state.loaded {
			state.load(cache_file);
		}
		match (&state.key, &state.last_error) {
			(Some(key), _) if unix_now() < key.expires_at && !state.stale => KeyStatus::Ready {
				expires_at: Some(key.expires_at),
			},
			(_, Some(error)) => KeyStatus::Failed(error.clone()),
			_ => KeyStatus::NotFetched,
		}
	}

	/// The mirror answered 403 to `key`: ask for a new one next time.
	pub async fn refused(&self, key: &str) {
		let Some(inner) = &self.0 else { return };
		let mut state = inner.state.lock().await;
		if state.key.as_ref().is_some_and(|k| k.api_key == key) {
			state.stale = true;
		}
	}

	async fn current(&self) -> Option<String> {
		let inner = self.0.as_ref()?;
		let (client_id, build_secret, cache_file) = match &inner.source {
			Source::Remote {
				client_id,
				build_secret,
				cache_file,
			} => (client_id, build_secret, cache_file.as_deref()),
			#[cfg(test)]
			Source::Fixed { key, .. } => return Some(key.clone()),
		};
		// Held across the request, so parallel downloads wait for one answer.
		let mut state = inner.state.lock().await;
		if !state.loaded {
			state.load(cache_file);
		}
		let now = unix_now();
		if let Some(key) = &state.key
			&& !state.stale
			&& now < key.refresh_after
		{
			return Some(key.api_key.clone());
		}
		if state.next_try.is_none_or(|t| Instant::now() >= t) {
			state.next_try = Some(Instant::now() + MIN_INTERVAL);
			match request(client_id, build_secret, &state.install_id).await {
				Ok(key) => {
					tracing::info!("got a mirror key");
					state.key = Some(key);
					state.stale = false;
					state.last_error = None;
					state.save(cache_file);
				}
				Err(e) => {
					tracing::warn!("couldn't get a mirror key: {}", e.message);
					state.last_error = Some(e.message);
					state.next_try = Some(Instant::now() + e.retry_in.max(MIN_INTERVAL));
				}
			}
		}
		// A key past `refreshAfter` still works until it expires.
		state
			.key
			.as_ref()
			.filter(|k| unix_now() < k.expires_at)
			.map(|k| k.api_key.clone())
	}
}

impl State {
	fn load(&mut self, cache_file: Option<&std::path::Path>) {
		self.loaded = true;
		let cached: CacheFile = cache_file
			.and_then(|f| std::fs::read_to_string(f).ok())
			.and_then(|text| serde_json::from_str(&text).ok())
			.unwrap_or_default();
		self.key = cached.key;
		if valid_install_id(&cached.install_id) {
			self.install_id = cached.install_id;
		} else {
			self.install_id = new_install_id();
			self.save(cache_file);
		}
	}

	fn save(&self, cache_file: Option<&std::path::Path>) {
		let Some(file) = cache_file else { return };
		let text = serde_json::to_string(&CacheFile {
			install_id: self.install_id.clone(),
			key: self.key.clone(),
		})
		.expect("plain struct serializes");
		let written = file
			.parent()
			.map_or(Ok(()), std::fs::create_dir_all)
			.and_then(|()| std::fs::write(file, text));
		if let Err(e) = written {
			tracing::warn!("couldn't save {}: {e}", file.display());
		}
	}
}

struct KeyError {
	message: String,
	retry_in: Duration,
}

async fn request(
	client_id: &str,
	build_secret: &str,
	install_id: &str,
) -> Result<CachedKey, KeyError> {
	let failed = |message: String| KeyError {
		message,
		retry_in: RETRY_FAILED,
	};
	let client = crate::platform_tools::http_client().map_err(failed)?;
	let body = serde_json::json!({
		"client_id": client_id,
		"build_secret": build_secret,
		"install_id": install_id,
		"app_version": env!("CARGO_PKG_VERSION"),
	});
	let response = client
		.post(ENDPOINT)
		.header(reqwest::header::CONTENT_TYPE, "application/json")
		.body(body.to_string())
		.send()
		.await
		.map_err(|e| failed(e.without_url().to_string()))?;
	let status = response.status();
	let text = response.text().await.unwrap_or_default();
	if status.is_success() {
		return parse_key(&text, unix_now()).ok_or_else(|| failed("unexpected answer".into()));
	}
	let reason = error_reason(&text);
	let retry_in = match status.as_u16() {
		503 => RETRY_PENDING,
		403 => RETRY_REFUSED,
		_ => RETRY_FAILED,
	};
	Err(KeyError {
		message: format!("HTTP {status}: {reason}"),
		retry_in,
	})
}

fn parse_key(text: &str, now: u64) -> Option<CachedKey> {
	#[derive(Deserialize)]
	#[serde(rename_all = "camelCase")]
	struct Answer {
		api_key: String,
		#[serde(default)]
		base_uri: Option<String>,
		#[serde(default)]
		refresh_after: Option<String>,
		#[serde(default)]
		expires_at: Option<String>,
	}
	let answer: Answer = serde_json::from_str(text).ok()?;
	let api_key = answer.api_key.trim().to_owned();
	if api_key.is_empty() {
		return None;
	}
	if let Some(base) = &answer.base_uri
		&& origin_of(base).as_deref() != Some(KEY_ORIGIN)
	{
		tracing::warn!(base_uri = %base, "the key is for another mirror than expected");
	}
	let refresh_after = answer
		.refresh_after
		.as_deref()
		.and_then(parse_utc)
		.unwrap_or(now + 60 * 60);
	let expires_at = answer
		.expires_at
		.as_deref()
		.and_then(parse_utc)
		.unwrap_or(refresh_after);
	Some(CachedKey {
		api_key,
		refresh_after,
		expires_at: expires_at.max(refresh_after),
	})
}

/// `reason` or `error` of a JSON error answer, else the start of the body.
fn error_reason(text: &str) -> String {
	let json: Option<serde_json::Value> = serde_json::from_str(text).ok();
	let field = json.as_ref().and_then(|v| {
		["reason", "error", "message"]
			.iter()
			.find_map(|k| v.get(k)?.as_str())
	});
	match field {
		Some(reason) => reason.to_owned(),
		None => text.trim().chars().take(200).collect(),
	}
}

fn mirror_origin(mirror: &Mirror) -> Option<String> {
	match &mirror.kind {
		MirrorKind::Http { base_uri } => origin_of(base_uri),
		// Where an rclone remote points isn't known, so it never gets the key.
		MirrorKind::Rclone { .. } => None,
	}
}

fn origin_of(url: &str) -> Option<String> {
	let url = reqwest::Url::parse(url.trim()).ok()?;
	let origin = url.origin();
	origin.is_tuple().then(|| origin.ascii_serialization())
}

fn valid_install_id(id: &str) -> bool {
	(8..=64).contains(&id.len())
		&& id
			.bytes()
			.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Random, not taken from anything on the computer.
fn new_install_id() -> String {
	use std::hash::{BuildHasher, Hasher};
	let part = || {
		let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
		hasher.write_u128(
			SystemTime::now()
				.duration_since(SystemTime::UNIX_EPOCH)
				.map_or(0, |d| d.as_nanos()),
		);
		hasher.finish()
	};
	format!("{:016x}{:016x}", part(), part())
}

fn unix_now() -> u64 {
	SystemTime::now()
		.duration_since(SystemTime::UNIX_EPOCH)
		.map_or(0, |d| d.as_secs())
}

/// Unix seconds of a UTC time such as `2026-11-20T10:00:00.000Z`.
fn parse_utc(text: &str) -> Option<u64> {
	let text = text.trim();
	let text = text
		.strip_suffix('Z')
		.or_else(|| text.strip_suffix("+00:00"))?;
	let (date, time) = text.split_once('T')?;
	let numbers = |s: &str, sep: char| -> Option<[i64; 3]> {
		let mut parts = s.split(sep).map(|p| p.parse::<i64>().ok());
		let out = [parts.next()??, parts.next()??, parts.next()??];
		parts.next().is_none().then_some(out)
	};
	let [year, month, day] = numbers(date, '-')?;
	let [hour, minute, second] = numbers(time.split('.').next()?, ':')?;
	if !(1..=12).contains(&month)
		|| !(1..=31).contains(&day)
		|| !(0..24).contains(&hour)
		|| !(0..60).contains(&minute)
		|| !(0..=60).contains(&second)
	{
		return None;
	}
	// Days since 1970-01-01 in the proleptic Gregorian calendar.
	let y = if month <= 2 { year - 1 } else { year };
	let era = y.div_euclid(400);
	let yoe = y - era * 400;
	let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
	let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
	u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn http(base: &str) -> Mirror {
		Mirror {
			id: "m".into(),
			name: "m".into(),
			kind: MirrorKind::Http {
				base_uri: base.into(),
			},
			password: "aGk=".into(),
		}
	}

	#[test]
	fn key_only_goes_to_the_vrsrc_mirror_over_https() {
		let keys = MirrorKeys::with_source(Source::Remote {
			client_id: "c".into(),
			build_secret: "s".into(),
			cache_file: None,
		});
		assert!(keys.sends_to(&http("https://go.srcdl1.xyz/")));
		assert!(keys.sends_to(&http("https://go.srcdl1.xyz:443/x/")));
		assert!(keys.sends_to(&http("https://GO.srcdl1.xyz")));
		for other in [
			"http://go.srcdl1.xyz/",
			"https://go.srcdl1.xyz:8443/",
			"https://go.srcdl1.xyz.evil.example/",
			"https://evil.example/go.srcdl1.xyz/",
			"https://user@other.example/",
			"not a url",
		] {
			assert!(!keys.sends_to(&http(other)), "{other}");
		}
		let rclone = Mirror {
			kind: MirrorKind::Rclone {
				config_file: "/c".into(),
				remote: "r".into(),
			},
			..http("https://go.srcdl1.xyz/")
		};
		assert!(!keys.sends_to(&rclone));
		assert!(!MirrorKeys::default().sends_to(&http("https://go.srcdl1.xyz/")));
	}

	#[test]
	fn reads_the_key_answer() {
		let key = parse_key(
			r#"{"apiKey":"k1","baseUri":"https://go.srcdl1.xyz","refreshAfter":"2026-11-20T10:00:00.000Z","expiresAt":"2026-11-26T09:00:00.000Z"}"#,
			0,
		)
		.unwrap();
		assert_eq!(key.api_key, "k1");
		assert_eq!(key.refresh_after, 1_795_168_800);
		assert_eq!(key.expires_at, 1_795_683_600);
		assert!(parse_key(r#"{"apiKey":""}"#, 0).is_none());
		assert!(parse_key("oops", 0).is_none());
		let bare = parse_key(r#"{"apiKey":"k"}"#, 100).unwrap();
		assert_eq!((bare.refresh_after, bare.expires_at), (3700, 3700));
	}

	#[test]
	fn utc_times() {
		assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
		assert_eq!(parse_utc("2000-03-01T00:00:00Z"), Some(951_868_800));
		assert_eq!(parse_utc("2026-11-20T10:00:00+00:00"), Some(1_795_168_800));
		assert_eq!(parse_utc("2026-11-20T10:00:00"), None);
		assert_eq!(parse_utc("2026-13-20T10:00:00Z"), None);
	}

	#[test]
	fn install_ids() {
		let id = new_install_id();
		assert!(valid_install_id(&id), "{id}");
		assert_ne!(id, new_install_id());
		assert!(!valid_install_id("short"));
		assert!(!valid_install_id("has space in it"));
	}

	#[test]
	fn error_reasons() {
		assert_eq!(error_reason(r#"{"reason":"retired"}"#), "retired");
		assert_eq!(error_reason(r#"{"error":"key_pending"}"#), "key_pending");
		assert_eq!(error_reason(" key_pending\n"), "key_pending");
	}

	#[tokio::test]
	async fn refused_key_is_replaced_once_asked_again() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join("key.json");
		let keys = MirrorKeys::with_source(Source::Remote {
			client_id: "c".into(),
			build_secret: "s".into(),
			cache_file: Some(file.clone()),
		});
		let far = unix_now() + 3600;
		std::fs::write(
			&file,
			format!(
				r#"{{"install_id":"abcdefgh12","key":{{"api_key":"old","refresh_after":{far},"expires_at":{far}}}}}"#
			),
		)
		.unwrap();
		// No request goes out while the cached key is fresh.
		assert_eq!(keys.current().await.as_deref(), Some("old"));
		keys.refused("other").await;
		assert_eq!(keys.current().await.as_deref(), Some("old"));
		let inner = keys.0.as_ref().unwrap();
		inner.state.lock().await.next_try = Some(Instant::now() + Duration::from_secs(600));
		keys.refused("old").await;
		assert!(inner.state.lock().await.stale);
		// Inside the wait no request goes out and the unexpired key stays.
		assert_eq!(keys.current().await.as_deref(), Some("old"));
		assert_eq!(inner.state.lock().await.install_id, "abcdefgh12");
	}
}
