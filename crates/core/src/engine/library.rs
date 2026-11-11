use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::task::JoinHandle;

use super::{Event, EventSink};
use crate::catalog::{self, Catalog, CatalogError, Fetcher, Mirror, SyncStage};

/// A synced catalog older than this is refreshed automatically.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, PartialEq)]
pub enum CatalogStatus {
	Idle,
	Syncing {
		stage: String,
		progress: Option<f32>,
	},
	Failed(String),
}

/// Remembers the catalog it last announced, for the engine's own use.
#[derive(Clone)]
struct LibrarySink {
	sink: EventSink,
	current: Arc<Mutex<Option<Arc<Catalog>>>>,
}

impl LibrarySink {
	fn emit(&self, event: Event) {
		if let Event::CatalogLoaded(catalog) = &event {
			*self.current.lock().expect("catalog lock poisoned") = catalog.clone();
		}
		self.sink.emit(event);
	}
}

pub(super) struct Library {
	fetcher: Fetcher,
	root: PathBuf,
	sink: LibrarySink,
	job: Option<JoinHandle<()>>,
	limit: (u64, u8),
	last_manual_sync: Option<Instant>,
}

/// Manual syncs closer together than this are refused, to spare the mirror.
const SYNC_COOLDOWN: Duration = Duration::from_secs(10 * 60);

impl Library {
	pub fn new(rclone_path: Option<PathBuf>, root: PathBuf, sink: EventSink) -> Self {
		let fetcher = Fetcher::new(rclone_path).expect("HTTP client with default TLS settings");
		let sink = LibrarySink {
			sink,
			current: Arc::new(Mutex::new(None)),
		};
		Self {
			fetcher,
			root,
			sink,
			job: None,
			limit: (0, 1),
			last_manual_sync: None,
		}
	}

	/// The catalog the UI was last given.
	pub fn current(&self) -> Option<Arc<Catalog>> {
		self.sink
			.current
			.lock()
			.expect("catalog lock poisoned")
			.clone()
	}

	pub fn fetcher(&self) -> &Fetcher {
		&self.fetcher
	}

	pub fn set_bandwidth_limit(&mut self, bytes_per_sec: u64, parallel: u8) {
		self.limit = (bytes_per_sec, parallel);
		self.fetcher.set_bandwidth_limit(bytes_per_sec, parallel);
	}

	pub fn set_rclone_path(&mut self, path: Option<PathBuf>) {
		match Fetcher::new(path) {
			Ok(fetcher) => {
				fetcher.set_bandwidth_limit(self.limit.0, self.limit.1);
				self.fetcher = fetcher;
			}
			Err(e) => self.sink.emit(Event::Error(e.to_string())),
		}
	}

	fn busy(&self) -> bool {
		self.job.as_ref().is_some_and(|j| !j.is_finished())
	}

	/// Shows the cached catalog of `mirror` right away and syncs it if it's
	/// missing or stale. Cancels whatever the previous mirror was doing.
	pub fn activate(&mut self, mirror: Option<Mirror>) {
		if let Some(job) = self.job.take() {
			job.abort();
		}
		let sink = self.sink.clone();
		let Some(mirror) = mirror else {
			sink.emit(Event::CatalogLoaded(None));
			sink.emit(Event::CatalogStatus(CatalogStatus::Idle));
			return;
		};
		let fetcher = self.fetcher.clone();
		let root = self.root.clone();
		self.job = Some(tokio::spawn(async move {
			let cached = {
				let (root, id) = (root.clone(), mirror.id.clone());
				tokio::task::spawn_blocking(move || Catalog::load(&root, &id)).await
			};
			let fresh = match cached {
				Ok(Ok(Some(catalog))) => {
					let age = SystemTime::now()
						.duration_since(catalog.synced_at)
						.unwrap_or_default();
					sink.emit(Event::CatalogLoaded(Some(Arc::new(catalog))));
					age < STALE_AFTER
				}
				Ok(Ok(None)) => {
					sink.emit(Event::CatalogLoaded(None));
					false
				}
				Ok(Err(e)) => {
					tracing::warn!("cached catalog unreadable: {e}");
					sink.emit(Event::CatalogLoaded(None));
					false
				}
				Err(e) => {
					tracing::error!("catalog load task failed: {e}");
					false
				}
			};
			if fresh {
				sink.emit(Event::CatalogStatus(CatalogStatus::Idle));
			} else {
				run_sync(&fetcher, &mirror, &root, &sink).await;
			}
		}));
	}

	pub fn sync(&mut self, mirror: Option<Mirror>) {
		if self.busy() {
			return;
		}
		if let Some(last) = self.last_manual_sync
			&& last.elapsed() < SYNC_COOLDOWN
		{
			let wait = (SYNC_COOLDOWN - last.elapsed()).as_secs().div_ceil(60);
			self.sink.emit(Event::Notice(format!(
				"The library was synced moments ago; try again in {wait} min"
			)));
			return;
		}
		self.last_manual_sync = Some(Instant::now());
		let Some(mirror) = mirror else {
			self.sink.emit(Event::Error(
				"No mirror configured. Add one in Settings.".into(),
			));
			return;
		};
		let (fetcher, root, sink) = (self.fetcher.clone(), self.root.clone(), self.sink.clone());
		self.job = Some(tokio::spawn(async move {
			run_sync(&fetcher, &mirror, &root, &sink).await;
		}));
	}

	pub fn abort(&mut self) {
		if let Some(job) = self.job.take() {
			job.abort();
		}
	}
}

async fn run_sync(fetcher: &Fetcher, mirror: &Mirror, root: &std::path::Path, sink: &LibrarySink) {
	let last = Mutex::new(Instant::now() - PROGRESS_INTERVAL);
	let report = |stage: SyncStage| {
		let mut last = last.lock().expect("progress lock poisoned");
		if last.elapsed() < PROGRESS_INTERVAL {
			return;
		}
		*last = Instant::now();
		let (stage, progress) = match stage {
			SyncStage::Downloading { done, total } => (
				format!("Downloading metadata ({})", crate::format_bytes(done)),
				total.filter(|&t| t > 0).map(|t| done as f32 / t as f32),
			),
			SyncStage::Extracting { done, total } => (
				"Unpacking metadata".to_owned(),
				(total > 0).then(|| done as f32 / total as f32),
			),
		};
		sink.emit(Event::CatalogStatus(CatalogStatus::Syncing {
			stage,
			progress,
		}));
	};
	sink.emit(Event::CatalogStatus(CatalogStatus::Syncing {
		stage: format!("Connecting to {}", mirror.name),
		progress: None,
	}));

	match catalog::sync(fetcher, mirror, root, &report).await {
		Ok(catalog) => {
			tracing::info!("synced {} games from {}", catalog.games.len(), mirror.name);
			sink.emit(Event::CatalogLoaded(Some(Arc::new(catalog))));
			sink.emit(Event::CatalogStatus(CatalogStatus::Idle));
		}
		Err(e) => {
			tracing::error!("sync with {} failed: {e}", mirror.name);
			sink.emit(Event::CatalogStatus(CatalogStatus::Failed(describe(&e))));
		}
	}
}

fn describe(e: &CatalogError) -> String {
	match e {
		CatalogError::Status {
			status: 401 | 403, ..
		} => {
			format!("{e}. The mirror refused access; check its URL and headers (API key).")
		}
		other => other.to_string(),
	}
}
