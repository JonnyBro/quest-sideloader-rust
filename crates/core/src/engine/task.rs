//! What the UI sees of a background job: installs and downloads report
//! through a [`Tracker`] that turns progress into [`TaskSnapshot`] events.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::install::ReviewDecision;
use super::{Event, EventSink};
use crate::format_bytes;
use crate::sideload::run::Update;

pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
	Install,
	Download,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
	Queued,
	Running,
	AwaitingReview,
	Done,
	Failed(String),
	Cancelled,
}

impl TaskState {
	pub fn is_finished(&self) -> bool {
		matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled)
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSnapshot {
	pub id: TaskId,
	pub kind: TaskKind,
	/// Headset the task installs to; `None` for download-only jobs.
	pub serial: Option<String>,
	pub title: String,
	pub stage: String,
	/// 0.0..=1.0 while bytes are moving.
	pub progress: Option<f32>,
	/// Speed and time left, when known.
	pub detail: Option<String>,
	pub state: TaskState,
	pub warnings: Vec<String>,
	/// Folder with the unpacked release, kept after a download-only job.
	pub output: Option<PathBuf>,
}

const SEND_INTERVAL: Duration = Duration::from_millis(100);
const RATE_WINDOW: Duration = Duration::from_millis(700);

struct Rate {
	since: Instant,
	bytes_at_since: u64,
	bytes_per_sec: f64,
}

struct Inner {
	snapshot: TaskSnapshot,
	last_sent: Instant,
	rate: Option<Rate>,
}

pub(super) struct Tracker {
	inner: Mutex<Inner>,
	pub sink: EventSink,
}

impl Tracker {
	pub fn new(snapshot: TaskSnapshot, sink: EventSink) -> Arc<Self> {
		let tracker = Arc::new(Self {
			inner: Mutex::new(Inner {
				snapshot,
				last_sent: Instant::now(),
				rate: None,
			}),
			sink,
		});
		tracker.update(|_| {}, true);
		tracker
	}

	pub fn update(&self, change: impl FnOnce(&mut TaskSnapshot), force: bool) {
		let mut inner = self.inner.lock().expect("tracker lock poisoned");
		change(&mut inner.snapshot);
		if force || inner.last_sent.elapsed() >= SEND_INTERVAL {
			inner.last_sent = Instant::now();
			self.sink.emit(Event::TaskUpdated(inner.snapshot.clone()));
		}
	}

	pub fn stage(&self, stage: impl Into<String>) {
		let stage = stage.into();
		self.inner.lock().expect("tracker lock poisoned").rate = None;
		self.update(
			|s| {
				s.stage = stage;
				s.progress = None;
				s.detail = None;
				if s.state == TaskState::Queued {
					s.state = TaskState::Running;
				}
			},
			true,
		);
	}

	pub fn bytes(&self, done: u64, total: Option<u64>) {
		let detail = self.measure(done, total);
		let fraction = total
			.filter(|&t| t > 0)
			.map(|t| (done as f64 / t as f64).min(1.0) as f32);
		let finished = total.is_some_and(|t| done >= t);
		self.update(
			|s| {
				s.progress = fraction;
				if detail.is_some() {
					s.detail = detail;
				}
			},
			finished,
		);
	}

	/// Smoothed speed over short windows; `None` until there's a measurement.
	fn measure(&self, done: u64, total: Option<u64>) -> Option<String> {
		let mut inner = self.inner.lock().expect("tracker lock poisoned");
		let now = Instant::now();
		let rate = inner.rate.get_or_insert(Rate {
			since: now,
			bytes_at_since: done,
			bytes_per_sec: 0.0,
		});
		let elapsed = now.duration_since(rate.since);
		if elapsed < RATE_WINDOW || done < rate.bytes_at_since {
			return None;
		}
		let instant = (done - rate.bytes_at_since) as f64 / elapsed.as_secs_f64();
		rate.bytes_per_sec = if rate.bytes_per_sec == 0.0 {
			instant
		} else {
			0.7 * rate.bytes_per_sec + 0.3 * instant
		};
		rate.since = now;
		rate.bytes_at_since = done;
		Some(describe_rate(
			rate.bytes_per_sec,
			total.map(|t| t.saturating_sub(done)),
		))
	}

	pub fn report(&self, update: Update) {
		match update {
			Update::Stage(stage) => self.stage(stage),
			Update::Bytes { done, total } => self.bytes(done, Some(total)),
			Update::Warning(warning) => {
				tracing::warn!("{warning}");
				self.update(|s| s.warnings.push(warning), true);
			}
		}
	}

	pub fn finish(&self, state: TaskState, stage: Option<String>, output: Option<PathBuf>) {
		self.update(
			|s| {
				s.state = state;
				s.progress = None;
				s.detail = None;
				s.output = output;
				if let Some(stage) = stage {
					s.stage = stage;
				}
			},
			true,
		);
	}
}

fn describe_rate(bytes_per_sec: f64, remaining: Option<u64>) -> String {
	let speed = format!("{}/s", format_bytes(bytes_per_sec as u64));
	match remaining {
		Some(left) if bytes_per_sec >= 1.0 => {
			format!(
				"{speed} · {} left",
				format_duration((left as f64 / bytes_per_sec) as u64)
			)
		}
		_ => speed,
	}
}

fn format_duration(secs: u64) -> String {
	match secs {
		0..60 => format!("{secs} s"),
		60..3600 => format!("{} min", secs.div_ceil(60)),
		_ => format!("{} h {} min", secs / 3600, (secs % 3600) / 60),
	}
}

pub(super) struct TaskEntry {
	/// `None` while a download is still waiting in the queue.
	pub abort: Option<tokio::task::AbortHandle>,
	pub review: Option<oneshot::Sender<ReviewDecision>>,
	pub tracker: Arc<Tracker>,
	/// Mirror release a download job fetches.
	pub release: Option<String>,
}

/// Removes a folder when dropped, so aborted tasks clean up after themselves.
pub(super) struct TempDir(pub PathBuf);

impl Drop for TempDir {
	fn drop(&mut self) {
		let path = std::mem::take(&mut self.0);
		if path.as_os_str().is_empty() {
			return;
		}
		std::thread::spawn(move || {
			if let Err(e) = std::fs::remove_dir_all(&path)
				&& e.kind() != std::io::ErrorKind::NotFound
			{
				tracing::warn!("could not remove {}: {e}", path.display());
			}
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rates_and_durations() {
		assert_eq!(format_duration(42), "42 s");
		assert_eq!(format_duration(61), "2 min");
		assert_eq!(format_duration(3725), "1 h 2 min");
		assert_eq!(
			describe_rate(2.0 * 1024.0 * 1024.0, Some(4 * 1024 * 1024)),
			"2.0 MiB/s · 2 s left"
		);
		assert_eq!(describe_rate(0.0, Some(10)), "0 B/s");
	}
}
