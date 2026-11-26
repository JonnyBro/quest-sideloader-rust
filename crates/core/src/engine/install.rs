use rust_i18n::t;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{Mutex, oneshot};

use super::task::{
	BrokenStream, SignatureConflict, TaskEntry, TaskKind, TaskSnapshot, TaskState, TempDir, Tracker,
};
use super::{Event, EventSink};
use crate::adb::Device;
use crate::sideload::run;
use crate::sideload::script::{self, ScriptAnalysis};
use crate::sideload::{InstallPlan, SideloadError, Source, extract_zip, io_err};

pub use super::task::TaskId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
	RunAll,
	SafeOnly,
	Cancel,
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

#[derive(Clone)]
pub(super) struct InstallContext {
	pub id: TaskId,
	pub device: Device,
	/// Scratch space for unpacking ZIPs.
	pub work_dir: PathBuf,
	pub locks: DeviceLocks,
	pub sink: EventSink,
	/// Name the task after the APK's label; downloads keep the catalog name.
	pub title_from_files: bool,
	/// `None` skips that: turned off, or the caller backed them up already.
	pub backups: Option<PathBuf>,
	pub streaming: bool,
}

fn file_title(path: &Path) -> String {
	path.file_name().map_or_else(
		|| path.display().to_string(),
		|n| n.to_string_lossy().into_owned(),
	)
}

pub(super) fn spawn(ctx: InstallContext, source: PathBuf, remove_source: bool) -> TaskEntry {
	let (review_tx, review_rx) = oneshot::channel();
	let tracker = Tracker::new(
		TaskSnapshot {
			id: ctx.id,
			kind: TaskKind::Install,
			serial: Some(ctx.device.serial().to_owned()),
			title: file_title(&source),
			stage: t!("task.queued").into_owned(),
			progress: None,
			detail: None,
			state: TaskState::Running,
			warnings: Vec::new(),
			output: None,
			conflict: None,
			broken_stream: None,
		},
		ctx.sink.clone(),
	);
	let task_tracker = tracker.clone();
	let handle = tokio::spawn(async move {
		let result = install_from(&ctx, &source, &task_tracker, review_rx).await;
		if result.is_ok() && remove_source {
			remove_release(&source, &task_tracker).await;
		}
		if result.is_err() && remove_source {
			task_tracker.update(|s| s.keep_removing_source(), true);
		}
		finish_install(&ctx, &task_tracker, result);
	});
	TaskEntry {
		abort: Some(handle.abort_handle()),
		review: Some(review_tx),
		tracker,
		release: None,
	}
}

pub(super) async fn remove_release(source: &Path, tracker: &Tracker) {
	let dir = source.to_path_buf();
	let removed = tokio::task::spawn_blocking(move || {
		if dir.is_dir() {
			std::fs::remove_dir_all(&dir)
		} else {
			std::fs::remove_file(&dir)
		}
	})
	.await;
	let error = match removed {
		Ok(Ok(())) => return,
		Ok(Err(e)) => e.to_string(),
		Err(e) => e.to_string(),
	};
	tracker.report(run::Update::Warning(
		t!("dl.remove_failed", error = error).into_owned(),
	));
}

pub(super) fn finish_install(
	ctx: &InstallContext,
	tracker: &Tracker,
	result: Result<(), SideloadError>,
) {
	match result {
		Ok(()) => {
			tracker.finish(
				TaskState::Done,
				Some(t!("task.installed").into_owned()),
				None,
			);
			super::devices::spawn_fetch_info(ctx.device.clone(), ctx.sink.clone());
		}
		Err(SideloadError::Cancelled) => tracker.finish(
			TaskState::Cancelled,
			Some(t!("task.cancelled").into_owned()),
			None,
		),
		Err(e) => {
			tracing::error!("install failed: {e}");
			tracker.finish(TaskState::Failed(e.to_string()), None, None);
		}
	}
}

pub(super) async fn blocking<T: Send + 'static>(
	f: impl FnOnce() -> Result<T, SideloadError> + Send + 'static,
) -> Result<T, SideloadError> {
	tokio::task::spawn_blocking(f)
		.await
		.map_err(|e| SideloadError::Io {
			path: PathBuf::new(),
			source: std::io::Error::other(e.to_string()),
		})?
}

/// Asks for an install.txt review through `review` when the script needs one.
pub(super) async fn install_from(
	ctx: &InstallContext,
	source: &Path,
	tracker: &Tracker,
	review: oneshot::Receiver<ReviewDecision>,
) -> Result<(), SideloadError> {
	tracker.stage(t!("inst.reading"));
	let source_path = source.to_path_buf();
	let source = {
		let path = source.to_path_buf();
		blocking(move || Source::classify(&path)).await?
	};

	let mut _temp = None;
	let plan = match source {
		Source::Apk(path) => blocking(move || InstallPlan::from_apk(&path)).await?,
		Source::Folder(path) => blocking(move || InstallPlan::from_folder(&path)).await?,
		Source::Zip(path) => {
			tracker.stage(t!("inst.extracting"));
			let dest = ctx.work_dir.join(format!("task-{}", ctx.id));
			_temp = Some(TempDir(dest.clone()));
			blocking(move || {
				extract_zip(&path, &dest)?;
				InstallPlan::from_folder(&dest)
			})
			.await?
		}
	};
	if ctx.title_from_files {
		let title = plan.title.clone();
		tracker.update(|s| s.title = title, true);
	}

	let actions = match &plan.script {
		None => None,
		Some(script_path) => {
			let analysis = read_script(script_path, &plan).await?;
			let allow_suspicious = if analysis.needs_review() {
				tracker.update(
					|s| {
						s.state = TaskState::AwaitingReview;
						s.stage = t!("inst.review").into_owned();
					},
					true,
				);
				ctx.sink.emit(Event::ScriptReview {
					task: ctx.id,
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

	if plan.script.is_some() {
		unpack_side_archives(&plan.root, tracker).await?;
	}

	let lock = ctx.locks.get(ctx.device.serial());
	let _guard = match lock.try_lock() {
		Ok(guard) => guard,
		Err(_) => {
			tracker.stage(t!("inst.waiting"));
			lock.lock().await
		}
	};

	if let (Some(root), Some(package)) = (&ctx.backups, plan.package()) {
		back_up_before_update(ctx, tracker, root, package, &plan.title).await;
	}

	let report = |u| tracker.report(u);
	let result = match actions {
		Some(actions) => run::run_script(&ctx.device, &actions, ctx.streaming, &report).await,
		None => run::run_default(&ctx.device, &plan, ctx.streaming, &report).await,
	};
	match result {
		Err(SideloadError::Adb(crate::adb::AdbError::InstallFailed(message)))
			if message.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") =>
		{
			let package = plan.package().unwrap_or_default().to_owned();
			let conflict = SignatureConflict {
				package: package.clone(),
				source: source_path,
				streaming: ctx.streaming,
				remove_after: false,
			};
			tracker.update(|s| s.conflict = Some(conflict), true);
			Err(SideloadError::Adb(crate::adb::AdbError::InstallFailed(
				t!("inst.signature", package = package).into_owned(),
			)))
		}
		Err(e @ SideloadError::Adb(crate::adb::AdbError::StreamBroken(_))) => {
			let broken = BrokenStream {
				source: source_path,
				remove_after: false,
			};
			tracker.update(|s| s.broken_stream = Some(broken), true);
			Err(e)
		}
		other => other,
	}
}

/// A failed backup only leaves a warning.
async fn back_up_before_update(
	ctx: &InstallContext,
	tracker: &Tracker,
	root: &Path,
	package: &str,
	title: &str,
) {
	let installed = match ctx.device.third_party_packages().await {
		Ok(packages) => packages.into_iter().find(|p| p.name == package),
		Err(_) => None,
	};
	let Some(installed) = installed else {
		return;
	};
	tracker.stage(t!("inst.backing_up"));
	let progress = |n| tracker.bytes(n, None);
	match crate::saves::back_up(
		&ctx.device,
		root,
		package,
		title,
		installed.version_code,
		&progress,
	)
	.await
	{
		Ok(_) => ctx.sink.emit(Event::BackupsChanged),
		Err(crate::saves::SaveError::NothingToBackUp(_)) => {}
		Err(e) => tracker.report(run::Update::Warning(
			t!("inst.backup_failed", error = e).into_owned(),
		)),
	}
}

/// Releases with an install.txt may keep their payload in a `.7z` next to it
/// (`_data.7z`). Mirror downloads are unpacked already; this covers folders
/// installed by hand. The archives stay.
async fn unpack_side_archives(root: &Path, tracker: &Tracker) -> Result<(), SideloadError> {
	let root = root.to_path_buf();
	let archives = {
		let root = root.clone();
		blocking(move || {
			let entries = std::fs::read_dir(&root).map_err(io_err(&root))?;
			Ok(entries
				.filter_map(|e| e.ok())
				.map(|e| e.path())
				.filter(|p| {
					p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("7z"))
				})
				.collect::<Vec<_>>())
		})
		.await?
	};
	for archive in archives {
		let name = archive
			.file_name()
			.unwrap_or_default()
			.to_string_lossy()
			.into_owned();
		tracker.stage(t!("inst.unpacking", name = name));
		let progress = |done: u64, total: u64| tracker.bytes(done, Some(total));
		let never = std::sync::atomic::AtomicBool::new(false);
		let result = tokio::task::block_in_place(|| {
			crate::archive::extract_7z(
				std::slice::from_ref(&archive),
				&root,
				None,
				&progress,
				&never,
			)
		});
		result.map_err(|e| SideloadError::Zip {
			path: archive.clone(),
			message: match e {
				crate::archive::ArchiveError::WrongPassword => {
					t!("inst.data_password").into_owned()
				}
				other => other.to_string(),
			},
		})?;
	}
	Ok(())
}

async fn read_script(path: &Path, plan: &InstallPlan) -> Result<ScriptAnalysis, SideloadError> {
	let bytes = tokio::fs::read(path).await.map_err(io_err(path))?;
	let text = String::from_utf8_lossy(&bytes);
	Ok(script::analyze(&text, &plan.root, plan.package()))
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

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
			streaming: true,
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
			streaming: true,
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
			streaming: true,
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
	fn broken_stream_can_be_retried_without_streaming() {
		let s = setup("adb install game.apk\n");
		std::fs::write(s.release.join("game.apk"), b"apk bytes here").unwrap();
		s.fake.recorded.lock().unwrap().break_streams = true;
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
			streaming: true,
		});
		let failed = wait_for(&s.engine, |e| match e {
			Event::TaskUpdated(t) if t.state.is_finished() => Some(t),
			_ => None,
		});
		assert!(matches!(failed.state, TaskState::Failed(_)), "{failed:?}");
		let broken = failed.broken_stream.expect("broken stream is reported");
		assert_eq!(broken.source, s.release);

		s.engine.send(Command::RetryWithoutStreaming {
			serial: "SER1".into(),
			source: broken.source,
			remove_source: false,
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::TaskUpdated(t) if t.id != failed.id => Some(t.id),
			_ => None,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		let rec = s.fake.recorded.lock().unwrap();
		assert_eq!(rec.installs, [b"apk bytes here".len()]);
		let install = rec
			.shell
			.iter()
			.find(|c| c.starts_with("pm install -r -g /data/local/tmp/"))
			.expect("installed from the copy");
		let copy = install.trim_start_matches("pm install -r -g ");
		assert!(
			rec.shell.contains(&format!("rm -f {copy}")),
			"{:?}",
			rec.shell
		);
		assert!(s.release.is_dir(), "the user's folder stays");
	}

	#[test]
	fn cancel_during_review_runs_nothing() {
		let s = setup(SCRIPT);
		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
			streaming: true,
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
			streaming: true,
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::ScriptReview { .. } => panic!("safe script must not ask for review"),
			Event::TaskUpdated(t) => Some(t.id),
			_ => None,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		assert_eq!(s.fake.recorded.lock().unwrap().pushed.len(), 1);
	}

	#[test]
	fn side_archive_is_unpacked_before_the_script() {
		let s = setup("adb push extra.bin /sdcard/Android/data/com.example.game/files/extra.bin\n");
		let packed = s.release.parent().unwrap().join("packed");
		std::fs::create_dir_all(&packed).unwrap();
		std::fs::write(packed.join("extra.bin"), b"payload").unwrap();
		sevenz_rust2::compress_to_path(&packed, s.release.join("_data.7z")).unwrap();

		s.engine.send(Command::Install {
			serial: "SER1".into(),
			path: s.release.clone(),
			streaming: true,
		});
		let id = wait_for(&s.engine, |e| match e {
			Event::TaskUpdated(t) => Some(t.id),
			_ => None,
		});
		assert_eq!(finished_state(&s.engine, id), TaskState::Done);
		let rec = s.fake.recorded.lock().unwrap();
		assert_eq!(
			rec.pushed,
			[(
				"/sdcard/Android/data/com.example.game/files/extra.bin".to_owned(),
				b"payload".to_vec()
			)]
		);
		assert!(
			s.release.join("_data.7z").is_file(),
			"the user's archive stays"
		);
	}
}
