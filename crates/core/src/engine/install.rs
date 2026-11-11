use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, oneshot};

use super::{Event, EventSink};
use crate::adb::Device;
use crate::sideload::run::{self, Update};
use crate::sideload::script::{self, ScriptAnalysis};
use crate::sideload::{InstallPlan, SideloadError, Source, extract_zip, io_err};

pub type TaskId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
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
	pub serial: String,
	pub title: String,
	pub stage: String,
	/// 0.0..=1.0 while bytes are moving.
	pub progress: Option<f32>,
	pub state: TaskState,
	pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
	RunAll,
	SafeOnly,
	Cancel,
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Shared task state; every change is sent to the UI as a snapshot.
struct Tracker {
	snapshot: StdMutex<(TaskSnapshot, Instant)>,
	sink: EventSink,
}

impl Tracker {
	fn update(&self, change: impl FnOnce(&mut TaskSnapshot), force: bool) {
		let mut guard = self.snapshot.lock().expect("tracker lock poisoned");
		let (snapshot, last_sent) = &mut *guard;
		change(snapshot);
		if force || last_sent.elapsed() >= PROGRESS_INTERVAL {
			*last_sent = Instant::now();
			self.sink.emit(Event::TaskUpdated(snapshot.clone()));
		}
	}

	fn stage(&self, stage: impl Into<String>) {
		let stage = stage.into();
		self.update(
			|s| {
				s.stage = stage;
				s.progress = None;
			},
			true,
		);
	}

	fn report(&self, update: Update) {
		match update {
			Update::Stage(stage) => self.stage(stage),
			Update::Bytes { done, total } => {
				let fraction = if total == 0 {
					1.0
				} else {
					(done as f64 / total as f64).min(1.0) as f32
				};
				self.update(|s| s.progress = Some(fraction), done >= total);
			}
			Update::Warning(warning) => {
				tracing::warn!("{warning}");
				self.update(|s| s.warnings.push(warning), true);
			}
		}
	}
}

/// Removes the extraction folder even when the task is aborted mid-way.
struct TempDir(PathBuf);

impl Drop for TempDir {
	fn drop(&mut self) {
		let path = std::mem::take(&mut self.0);
		std::thread::spawn(move || {
			if let Err(e) = std::fs::remove_dir_all(&path)
				&& e.kind() != std::io::ErrorKind::NotFound
			{
				tracing::warn!("could not remove {}: {e}", path.display());
			}
		});
	}
}

pub(super) struct TaskEntry {
	pub abort: tokio::task::AbortHandle,
	pub review: Option<oneshot::Sender<ReviewDecision>>,
	pub serial: String,
	pub title: String,
}

/// One installation at a time per headset: parallel package installs and
/// pushes to the same device only slow each other down.
#[derive(Default, Clone)]
pub(super) struct DeviceLocks(Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>);

impl DeviceLocks {
	fn get(&self, serial: &str) -> Arc<Mutex<()>> {
		let mut map = self.0.lock().expect("device locks poisoned");
		map.entry(serial.to_owned()).or_default().clone()
	}
}

pub(super) struct NewTask {
	pub id: TaskId,
	pub device: Device,
	pub source: PathBuf,
	pub work_dir: PathBuf,
	pub locks: DeviceLocks,
	pub sink: EventSink,
}

pub(super) fn spawn(task: NewTask) -> TaskEntry {
	let (review_tx, review_rx) = oneshot::channel();
	let serial = task.device.serial().to_owned();
	let title = task.source.file_name().map_or_else(
		|| task.source.display().to_string(),
		|n| n.to_string_lossy().into_owned(),
	);
	let tracker = Arc::new(Tracker {
		snapshot: StdMutex::new((
			TaskSnapshot {
				id: task.id,
				serial: serial.clone(),
				title: title.clone(),
				stage: "Queued".into(),
				progress: None,
				state: TaskState::Running,
				warnings: Vec::new(),
			},
			Instant::now(),
		)),
		sink: task.sink.clone(),
	});
	tracker.update(|_| {}, true);

	let handle = tokio::spawn(async move {
		let device = task.device.clone();
		let result = run_task(task, &tracker, review_rx).await;
		let state = match result {
			Ok(()) => TaskState::Done,
			Err(SideloadError::Cancelled) => TaskState::Cancelled,
			Err(e) => {
				tracing::error!("install failed: {e}");
				TaskState::Failed(e.to_string())
			}
		};
		let done = state == TaskState::Done;
		tracker.update(
			|s| {
				s.state = state;
				s.progress = None;
				s.stage = if done {
					"Installed".into()
				} else {
					s.stage.clone()
				};
			},
			true,
		);
		if done {
			super::devices::spawn_fetch_info(device, tracker.sink.clone());
		}
	});

	TaskEntry {
		abort: handle.abort_handle(),
		review: Some(review_tx),
		serial,
		title,
	}
}

async fn blocking<T: Send + 'static>(
	f: impl FnOnce() -> Result<T, SideloadError> + Send + 'static,
) -> Result<T, SideloadError> {
	tokio::task::spawn_blocking(f)
		.await
		.map_err(|e| SideloadError::Io {
			path: PathBuf::new(),
			source: std::io::Error::other(e.to_string()),
		})?
}

async fn run_task(
	task: NewTask,
	tracker: &Tracker,
	review: oneshot::Receiver<ReviewDecision>,
) -> Result<(), SideloadError> {
	tracker.stage("Reading files");
	let source = {
		let path = task.source.clone();
		blocking(move || Source::classify(&path)).await?
	};

	let mut _temp = None;
	let plan = match source {
		Source::Apk(path) => blocking(move || InstallPlan::from_apk(&path)).await?,
		Source::Folder(path) => blocking(move || InstallPlan::from_folder(&path)).await?,
		Source::Zip(path) => {
			tracker.stage("Extracting");
			let dest = task.work_dir.join(format!("task-{}", task.id));
			_temp = Some(TempDir(dest.clone()));
			blocking(move || {
				extract_zip(&path, &dest)?;
				InstallPlan::from_folder(&dest)
			})
			.await?
		}
	};
	let title = plan.title.clone();
	tracker.update(|s| s.title = title, true);

	let actions = match &plan.script {
		None => None,
		Some(script_path) => {
			let analysis = read_script(script_path, &plan).await?;
			let allow_suspicious = if analysis.needs_review() {
				tracker.update(
					|s| {
						s.state = TaskState::AwaitingReview;
						s.stage = "Waiting for install.txt review".into();
					},
					true,
				);
				task.sink.emit(Event::ScriptReview {
					task: task.id,
					title: plan.title.clone(),
					analysis: analysis.clone(),
				});
				let decision = review.await.unwrap_or(ReviewDecision::Cancel);
				tracker.update(|s| s.state = TaskState::Running, true);
				match decision {
					ReviewDecision::RunAll => true,
					ReviewDecision::SafeOnly => false,
					ReviewDecision::Cancel => return Err(SideloadError::Cancelled),
				}
			} else {
				false
			};
			Some(analysis.actions(allow_suspicious))
		}
	};

	let lock = task.locks.get(task.device.serial());
	let _guard = match lock.try_lock() {
		Ok(guard) => guard,
		Err(_) => {
			tracker.stage("Waiting for another installation on this headset");
			lock.lock().await
		}
	};

	let report = |u| tracker.report(u);
	match actions {
		Some(actions) => run::run_script(&task.device, &actions, &report).await,
		None => run::run_default(&task.device, &plan, &report).await,
	}
}

async fn read_script(path: &Path, plan: &InstallPlan) -> Result<ScriptAnalysis, SideloadError> {
	let bytes = tokio::fs::read(path).await.map_err(io_err(path))?;
	let text = String::from_utf8_lossy(&bytes);
	Ok(script::analyze(&text, &plan.root, plan.package()))
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::adb::testing::FakeAdb;
	use crate::engine::{Command, Engine};
	use crate::paths::AppPaths;
	use crate::settings::Settings;

	fn wait_for<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
		let deadline = Instant::now() + Duration::from_secs(10);
		loop {
			assert!(Instant::now() < deadline, "expected event did not arrive");
			if let Some(found) = engine.poll_events().find_map(&mut pick) {
				return found;
			}
			std::thread::sleep(Duration::from_millis(10));
		}
	}

	fn finished_state(engine: &Engine, id: TaskId) -> TaskState {
		wait_for(engine, |e| match e {
			Event::TaskUpdated(s) if s.id == id && s.state.is_finished() => Some(s.state),
			_ => None,
		})
	}

	struct Setup {
		_dir: tempfile::TempDir,
		release: PathBuf,
		fake: FakeAdb,
		engine: Engine,
		_rt: tokio::runtime::Runtime,
	}

	fn setup(script: &str) -> Setup {
		let rt = tokio::runtime::Runtime::new().unwrap();
		let fake = rt.block_on(FakeAdb::start());
		let dir = tempfile::tempdir().unwrap();
		let release = dir.path().join("Game");
		std::fs::create_dir_all(release.join("com.example.game")).unwrap();
		std::fs::write(release.join("com.example.game/main.obb"), b"obb data").unwrap();
		std::fs::write(release.join("install.txt"), script).unwrap();

		let paths = AppPaths::under(dir.path().join("app"));
		let engine = Engine::start_with_adb_port(
			paths.clone(),
			Settings::defaults_for(&paths),
			Arc::new(|| {}),
			fake.port,
		)
		.unwrap();
		Setup {
			_dir: dir,
			release,
			fake,
			engine,
			_rt: rt,
		}
	}

	const SCRIPT: &str = "adb push com.example.game /sdcard/Android/obb/\n\
		adb shell settings put global x 1\n\
		adb reboot\n";

	#[test]
	fn review_safe_only_skips_suspicious_lines() {
		let s = setup(SCRIPT);
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
		});
		let (id, analysis) = wait_for(&s.engine, |e| match e {
			Event::ScriptReview { task, analysis, .. } => Some((task, analysis)),
			_ => None,
		});
		assert_eq!(analysis.package.as_deref(), Some("com.example.game"));
		s.engine.send(Command::ReviewScript {
			task: id,
			decision: ReviewDecision::SafeOnly,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);

		let rec = s.fake.recorded.lock().unwrap();
		assert_eq!(
			rec.pushed,
			[(
				"/sdcard/Android/obb/com.example.game/main.obb".to_owned(),
				b"obb data".to_vec()
			)]
		);
		assert!(
			!rec.shell.iter().any(|c| c.contains("settings")),
			"{:?}",
			rec.shell
		);
	}

	#[test]
	fn review_run_all_runs_suspicious_but_not_blocked() {
		let s = setup(SCRIPT);
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::ScriptReview { task, .. } => Some(task),
			_ => None,
		});
		s.engine.send(Command::ReviewScript {
			task: id,
			decision: ReviewDecision::RunAll,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		let rec = s.fake.recorded.lock().unwrap();
		assert!(rec.shell.iter().any(|c| c == "settings put global x 1"));
		assert!(!rec.shell.iter().any(|c| c.contains("reboot")));
	}

	#[test]
	fn push_targets_follow_adb_rules() {
		let s = setup(
			"adb push com.example.game/main.obb /sdcard/Android/obb/com.example.game/main.obb\n\
			adb push com.example.game /sdcard/Android/obb/com.example.game\n\
			adb push com.example.game/main.obb /sdcard/Android/data/com.example.game/files/x.obb\n",
		);
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::TaskUpdated(t) => Some(t.id),
			_ => None,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		let rec = s.fake.recorded.lock().unwrap();
		let paths: Vec<_> = rec.pushed.iter().map(|(p, _)| p.as_str()).collect();
		assert_eq!(
			paths,
			[
				"/sdcard/Android/obb/com.example.game/main.obb",
				"/sdcard/Android/obb/com.example.game/main.obb",
				"/sdcard/Android/data/com.example.game/files/x.obb",
			]
		);
	}

	#[test]
	fn cancel_during_review_runs_nothing() {
		let s = setup(SCRIPT);
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::ScriptReview { task, .. } => Some(task),
			_ => None,
		});
		s.engine.send(Command::ReviewScript {
			task: id,
			decision: ReviewDecision::Cancel,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Cancelled);
		assert!(s.fake.recorded.lock().unwrap().pushed.is_empty());
	}

	#[test]
	fn safe_script_runs_without_review() {
		let s = setup("adb push com.example.game /sdcard/Android/obb/\n");
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::ScriptReview { .. } => panic!("safe script must not ask for review"),
			Event::TaskUpdated(t) => Some(t.id),
			_ => None,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		assert_eq!(s.fake.recorded.lock().unwrap().pushed.len(), 1);
	}
}
