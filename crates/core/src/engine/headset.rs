//! Jobs that manage what's on a headset: save backups, restores, uninstalls
//! and reinstalling an app whose signature changed.

use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;

use super::install::{self, InstallContext};
use super::task::{TaskEntry, TaskKind, TaskSnapshot, TaskState, Tracker};
use super::{Event, EventSink};
use crate::adb::Device;
use crate::saves::{self, Backup, SaveError};
use crate::sideload::run::Update;

fn tracker(
	id: u64,
	kind: TaskKind,
	device: &Device,
	title: String,
	sink: &EventSink,
) -> Arc<Tracker> {
	Tracker::new(
		TaskSnapshot {
			id,
			kind,
			serial: Some(device.serial().to_owned()),
			title,
			stage: t!("task.starting").into_owned(),
			progress: None,
			detail: None,
			state: TaskState::Running,
			warnings: Vec::new(),
			output: None,
			conflict: None,
		},
		sink.clone(),
	)
}

fn entry(handle: tokio::task::JoinHandle<()>, tracker: Arc<Tracker>) -> TaskEntry {
	TaskEntry {
		abort: Some(handle.abort_handle()),
		review: None,
		tracker,
		release: None,
	}
}

fn finish(tracker: &Tracker, result: Result<String, String>, sink: &EventSink, device: &Device) {
	match result {
		Ok(stage) => tracker.finish(TaskState::Done, Some(stage), None),
		Err(e) => {
			tracing::error!("{e}");
			tracker.finish(TaskState::Failed(e), None, None);
		}
	}
	sink.emit(Event::BackupsChanged);
	super::devices::spawn_fetch_info(device.clone(), sink.clone());
}

/// Byte progress for a pull or push whose total is known.
fn byte_progress(tracker: &Tracker, total: u64) -> impl Fn(u64) + Send + Sync + '_ {
	let done = std::sync::atomic::AtomicU64::new(0);
	move |n| {
		let now = done.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n;
		tracker.bytes(now, Some(total.max(now)));
	}
}

pub(super) struct AppRef {
	pub package: String,
	pub name: String,
	pub version_code: Option<u64>,
}

pub(super) fn spawn_backup(
	id: u64,
	device: Device,
	root: PathBuf,
	app: AppRef,
	sink: EventSink,
) -> TaskEntry {
	let tracker = tracker(
		id,
		TaskKind::Backup,
		&device,
		t!("hs.back_up_title", name = app.name).into_owned(),
		&sink,
	);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		t.stage(t!("hs.copying_from"));
		let progress = |n| t.bytes(n, None);
		let result = saves::back_up(
			&device,
			&root,
			&app.package,
			&app.name,
			app.version_code,
			&progress,
		)
		.await
		.map(|b| {
			if let Some(save) = b.meta.skipped.iter().find(|p| saves::looks_like_save(p)) {
				t.report(Update::Warning(
					t!("hs.save_unreadable", file = save).into_owned(),
				));
			} else if let Some(first) = b.meta.skipped.first() {
				t.report(Update::Warning(
					t!(
						"hs.skipped_unreadable",
						n = b.meta.skipped.len(),
						first = first
					)
					.into_owned(),
				));
			}
			t!("hs.backed_up", n = b.meta.files).into_owned()
		})
		.map_err(|e| e.to_string());
		finish(&t, result, &sink, &device);
	});
	entry(handle, tracker)
}

/// `package` is where the saves go: the backed-up app or another one.
pub(super) fn spawn_restore(
	id: u64,
	device: Device,
	backup: Backup,
	package: String,
	sink: EventSink,
) -> TaskEntry {
	let title = if package == backup.meta.package {
		t!("hs.restore_title", name = backup.meta.name)
	} else {
		t!(
			"hs.restore_into_title",
			name = backup.meta.name,
			package = package
		)
	};
	let tracker = tracker(id, TaskKind::Restore, &device, title.into_owned(), &sink);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		t.stage(t!("hs.copying_to"));
		let progress = byte_progress(&t, backup.meta.bytes);
		let result = saves::restore(&device, &backup, &package, &progress)
			.await
			.map(|()| t!("hs.restored").into_owned())
			.map_err(|e| e.to_string());
		finish(&t, result, &sink, &device);
	});
	entry(handle, tracker)
}

/// Backs up the app's saves, keeping going if it has none.
async fn backup_if_any(
	tracker: &Tracker,
	device: &Device,
	root: &std::path::Path,
	app: &AppRef,
) -> Result<Option<Backup>, String> {
	tracker.stage(t!("hs.backing_up"));
	let progress = |n| tracker.bytes(n, None);
	match saves::back_up(
		device,
		root,
		&app.package,
		&app.name,
		app.version_code,
		&progress,
	)
	.await
	{
		Ok(backup) => Ok(Some(backup)),
		Err(SaveError::NothingToBackUp(_)) => Ok(None),
		Err(e) => Err(t!("hs.backup_failed", error = e).into_owned()),
	}
}

pub(super) struct UninstallOptions {
	pub back_up: bool,
	/// Also delete `/sdcard/Android/obb/<package>`.
	pub remove_obb: bool,
}

pub(super) fn spawn_uninstall(
	id: u64,
	device: Device,
	root: PathBuf,
	app: AppRef,
	options: UninstallOptions,
	sink: EventSink,
) -> TaskEntry {
	let tracker = tracker(
		id,
		TaskKind::Uninstall,
		&device,
		t!("hs.uninstall_title", name = app.name).into_owned(),
		&sink,
	);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		let result = async {
			let backup = if options.back_up {
				backup_if_any(&t, &device, &root, &app).await?
			} else {
				None
			};
			t.stage(t!("hs.uninstalling"));
			device
				.uninstall(&app.package, false)
				.await
				.map_err(|e| e.to_string())?;
			if options.remove_obb {
				t.stage(t!("hs.removing_obb"));
				device
					.remove_dir(&format!("/sdcard/Android/obb/{}", app.package))
					.await
					.map_err(|e| e.to_string())?;
			}
			Ok(match backup {
				Some(b) => t!("hs.uninstalled_backed_up", n = b.meta.files).into_owned(),
				None => t!("hs.uninstalled").into_owned(),
			})
		}
		.await;
		finish(&t, result, &sink, &device);
	});
	entry(handle, tracker)
}

/// For a signature mismatch: back up the saves, uninstall, install `source`
/// again and put the saves back. With `remove_source` the release goes once
/// it is installed, as it would have after a plain download and install.
pub(super) fn spawn_reinstall(
	ctx: InstallContext,
	root: PathBuf,
	app: AppRef,
	source: PathBuf,
	remove_source: bool,
	sink: EventSink,
) -> TaskEntry {
	let device = ctx.device.clone();
	let tracker = tracker(
		ctx.id,
		TaskKind::Install,
		&device,
		t!("hs.reinstall_title", name = app.name).into_owned(),
		&sink,
	);
	let (review_tx, review_rx) = tokio::sync::oneshot::channel();
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		let result = async {
			let backup = backup_if_any(&t, &device, &root, &app).await?;
			t.stage(t!("hs.uninstalling_old"));
			device
				.uninstall(&app.package, false)
				.await
				.map_err(|e| e.to_string())?;
			let kept = backup
				.as_ref()
				.map(|b| t!("hs.saves_kept", dir = b.dir.display()).into_owned())
				.unwrap_or_default();
			install::install_from(&ctx, &source, &t, review_rx)
				.await
				.map_err(|e| t!("hs.new_not_installed", error = e, kept = kept).into_owned())?;
			if remove_source {
				let dir = source.clone();
				let removed = tokio::task::spawn_blocking(move || {
					if dir.is_dir() {
						std::fs::remove_dir_all(&dir)
					} else {
						std::fs::remove_file(&dir)
					}
				})
				.await
				.map_err(|e| e.to_string())?;
				if let Err(e) = removed {
					t.report(crate::sideload::run::Update::Warning(
						t!("dl.remove_failed", error = e).into_owned(),
					));
				}
			}
			if let Some(backup) = &backup {
				t.stage(t!("hs.restoring"));
				let progress = byte_progress(&t, backup.meta.bytes);
				saves::restore(&device, backup, &backup.meta.package, &progress)
					.await
					.map_err(|e| t!("hs.restore_failed", error = e, kept = kept).into_owned())?;
			}
			Ok(match backup {
				Some(_) => t!("hs.reinstalled_saves").into_owned(),
				None => t!("hs.reinstalled").into_owned(),
			})
		}
		.await;
		finish(&t, result, &sink, &device);
	});
	TaskEntry {
		abort: Some(handle.abort_handle()),
		review: Some(review_tx),
		tracker,
		release: None,
	}
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use crate::adb::testing::FakeAdb;
	use crate::engine::{AppInfo, Command, Engine, Event, TaskId, TaskSnapshot, TaskState};
	use crate::paths::AppPaths;
	use crate::settings::Settings;

	fn wait_finished(engine: &Engine, id: Option<TaskId>) -> TaskSnapshot {
		let deadline = Instant::now() + Duration::from_secs(20);
		loop {
			assert!(Instant::now() < deadline, "task did not finish");
			if let Some(s) = engine.poll_events().find_map(|e| match e {
				Event::TaskUpdated(s)
					if s.state.is_finished() && id.is_none_or(|id| s.id == id) =>
				{
					Some(s)
				}
				_ => None,
			}) {
				return s;
			}
			std::thread::sleep(Duration::from_millis(20));
		}
	}

	#[test]
	fn backs_up_saves_before_installing_over_an_app() {
		let rt = tokio::runtime::Runtime::new().unwrap();
		let fake = rt.block_on(FakeAdb::start());
		{
			let mut rec = fake.recorded.lock().unwrap();
			rec.installed.push(("com.example.game".into(), 3));
			rec.device_files.push((
				"/sdcard/Android/data/com.example.game/files/save.dat".into(),
				b"progress".to_vec(),
			));
		}
		let dir = tempfile::tempdir().unwrap();
		let release = dir.path().join("Game");
		std::fs::create_dir_all(&release).unwrap();
		std::fs::write(release.join("game.pkg"), b"not really an apk").unwrap();
		std::fs::write(
			release.join("install.txt"),
			"adb install game.pkg\nadb push com.example.game /sdcard/Android/obb/\n",
		)
		.unwrap();
		std::fs::create_dir_all(release.join("com.example.game")).unwrap();
		std::fs::write(release.join("com.example.game/main.obb"), b"obb").unwrap();

		let paths = AppPaths::under(dir.path().join("app"));
		let engine = Engine::start_with_adb_port(
			paths.clone(),
			Settings::defaults_for(&paths),
			std::sync::Arc::new(|| {}),
			fake.port,
		)
		.unwrap();
		engine.send(Command::Install {
			serial: "SER1".into(),
			path: release,
		});
		let done = wait_finished(&engine, None);
		assert_eq!(done.state, TaskState::Done, "{done:?}");
		let backups = crate::saves::list(&paths.backups_dir());
		assert_eq!(
			backups.len(),
			1,
			"the installed version's saves are backed up"
		);
		assert_eq!(backups[0].meta.version_code, Some(3));
	}

	#[test]
	fn signature_conflict_then_reinstall_keeping_saves() {
		let rt = tokio::runtime::Runtime::new().unwrap();
		let fake = rt.block_on(FakeAdb::start());
		{
			let mut rec = fake.recorded.lock().unwrap();
			rec.install_failures
				.push("INSTALL_FAILED_UPDATE_INCOMPATIBLE: signatures do not match".into());
			rec.device_files.push((
				"/sdcard/Android/data/com.example.game/files/save.dat".into(),
				b"progress".to_vec(),
			));
		}
		let dir = tempfile::tempdir().unwrap();
		let release = dir.path().join("Game");
		std::fs::create_dir_all(release.join("com.example.game")).unwrap();
		std::fs::write(release.join("com.example.game/main.obb"), b"obb").unwrap();
		// Not named .apk, so planning doesn't try to parse it as one.
		std::fs::write(release.join("game.pkg"), b"not really an apk").unwrap();
		std::fs::write(
			release.join("install.txt"),
			"adb install game.pkg\nadb push com.example.game /sdcard/Android/obb/\n",
		)
		.unwrap();

		let paths = AppPaths::under(dir.path().join("app"));
		let engine = Engine::start_with_adb_port(
			paths.clone(),
			Settings::defaults_for(&paths),
			std::sync::Arc::new(|| {}),
			fake.port,
		)
		.unwrap();

		engine.send(Command::Install {
			serial: "SER1".into(),
			path: release.clone(),
		});
		let failed = wait_finished(&engine, None);
		assert!(matches!(failed.state, TaskState::Failed(_)), "{failed:?}");
		let conflict = failed
			.conflict
			.clone()
			.unwrap_or_else(|| panic!("no conflict: {failed:?}"));
		assert_eq!(conflict.package, "com.example.game");
		assert_eq!(conflict.source, release);
		assert!(!conflict.remove_after, "a folder installed by hand stays");

		engine.send(Command::ReinstallKeepingSaves {
			serial: "SER1".into(),
			app: AppInfo {
				package: conflict.package.clone(),
				name: "Game".into(),
				version_code: None,
			},
			source: conflict.source.clone(),
			remove_source: conflict.remove_after,
		});
		let done = wait_finished(&engine, Some(failed.id + 1));
		assert_eq!(done.state, TaskState::Done, "{done:?}");
		assert_eq!(done.stage, "Reinstalled with saves");
		assert!(release.exists());

		let rec = fake.recorded.lock().unwrap();
		assert!(
			rec.shell
				.iter()
				.any(|c| c == "pm uninstall com.example.game"),
			"{:?}",
			rec.shell
		);
		assert_eq!(rec.installs.len(), 2);
		let restored = rec
			.pushed
			.iter()
			.find(|(p, _)| p == "/sdcard/Android/data/com.example.game/files/save.dat")
			.expect("saves restored");
		assert_eq!(restored.1, b"progress");
		assert_eq!(
			crate::saves::list(&paths.backups_dir()).len(),
			1,
			"the backup is kept"
		);
		drop(rec);

		// A download that only stayed because of the conflict goes afterwards.
		engine.send(Command::ReinstallKeepingSaves {
			serial: "SER1".into(),
			app: AppInfo {
				package: conflict.package,
				name: "Game".into(),
				version_code: None,
			},
			source: conflict.source,
			remove_source: true,
		});
		let done = wait_finished(&engine, Some(failed.id + 2));
		assert_eq!(done.state, TaskState::Done, "{done:?}");
		assert!(!release.exists(), "the release is removed after installing");
	}
}
