//! How a mirror has been answering, from the requests the app makes anyway.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::mirror_key::KeyStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorHealth {
	pub mirror_id: String,
	/// The last request the mirror answered.
	pub last_ok: Option<SystemTime>,
	/// The last failed request and why.
	pub last_error: Option<(SystemTime, String)>,
	pub key: KeyStatus,
}

impl MirrorHealth {
	/// Whether the latest request failed.
	pub fn failing(&self) -> bool {
		match (&self.last_error, self.last_ok) {
			(Some((failed, _)), Some(ok)) => *failed > ok,
			(Some(_), None) => true,
			(None, _) => false,
		}
	}
}

type Listener = Arc<dyn Fn(MirrorHealth) + Send + Sync>;

#[derive(Default)]
struct Entry {
	last_ok: Option<SystemTime>,
	last_error: Option<(SystemTime, String)>,
}

/// Shared by clones of a fetcher; tells the listener about every change.
#[derive(Clone, Default)]
pub(crate) struct HealthTracker {
	entries: Arc<Mutex<HashMap<String, Entry>>>,
	listener: Option<Listener>,
}

impl HealthTracker {
	pub fn with_listener(
		mut self,
		listener: impl Fn(MirrorHealth) + Send + Sync + 'static,
	) -> Self {
		self.listener = Some(Arc::new(listener));
		self
	}

	pub fn record(&self, mirror_id: &str, error: Option<String>, key: KeyStatus) {
		let now = SystemTime::now();
		{
			let mut entries = self.entries.lock().expect("health lock poisoned");
			let entry = entries.entry(mirror_id.to_owned()).or_default();
			match error {
				Some(message) => entry.last_error = Some((now, message)),
				None => entry.last_ok = Some(now),
			}
		}
		self.announce(mirror_id, key);
	}

	pub fn snapshot(&self, mirror_id: &str, key: KeyStatus) -> MirrorHealth {
		let entries = self.entries.lock().expect("health lock poisoned");
		let entry = entries.get(mirror_id);
		MirrorHealth {
			mirror_id: mirror_id.to_owned(),
			last_ok: entry.and_then(|e| e.last_ok),
			last_error: entry.and_then(|e| e.last_error.clone()),
			key,
		}
	}

	pub fn announce(&self, mirror_id: &str, key: KeyStatus) {
		if let Some(listener) = &self.listener {
			listener(self.snapshot(mirror_id, key));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn latest_request_decides_failing() {
		let seen = Arc::new(Mutex::new(Vec::new()));
		let sink = seen.clone();
		let tracker = HealthTracker::default().with_listener(move |h| sink.lock().unwrap().push(h));
		tracker.record("m", Some("HTTP 403".into()), KeyStatus::NotUsed);
		assert!(seen.lock().unwrap()[0].failing());
		std::thread::sleep(std::time::Duration::from_millis(2));
		tracker.record("m", None, KeyStatus::NotUsed);
		let last = seen.lock().unwrap().last().cloned().unwrap();
		assert!(!last.failing());
		assert_eq!(last.last_error.unwrap().1, "HTTP 403");
		assert!(!tracker.snapshot("other", KeyStatus::NotUsed).failing());
	}
}
