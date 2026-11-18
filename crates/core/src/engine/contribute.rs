//! Uploading an app version from the headset to the mirror. Reading from the
//! headset only: the APK and OBB are pulled, nothing on the device changes.

use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::EventSink;
use super::task::{TaskEntry, TaskId, TaskKind, TaskSnapshot, TaskState, TempDir, Tracker};
use crate::adb::Device;
use crate::catalog::{Fetcher, RcloneAccess, UploadTarget};
use crate::contribute::{archive_name, hwid, pack};
use crate::format_bytes;
use crate::sideload::read_apk_meta;

pub(super) struct Upload {
	pub id: TaskId,
	pub device: Device,
	pub package: String,
	/// Shown until the APK's own label is known.
	pub name: String,
	pub target: UploadTarget,
	pub work_dir: PathBuf,
	pub fetcher: Fetcher,
	/// Uploads go one at a time.
	pub lock: Arc<Mutex<()>>,
	pub sink: EventSink,
}

pub(super) fn spawn(upload: Upload) -> TaskEntry {
	let tracker = Tracker::new(
		TaskSnapshot {
			id: upload.id,
			kind: TaskKind::Upload,
			serial: Some(upload.device.serial().to_owned()),
			title: upload.name.clone(),
			stage: t!("task.queued").into_owned(),
			progress: None,
			detail: None,
			state: TaskState::Running,
			warnings: Vec::new(),
			output: None,
			conflict: None,
		},
		upload.sink.clone(),
	);
	let task_tracker = tracker.clone();
	let handle = tokio::spawn(async move {
		let _cleanup = TempDir(upload.work_dir.clone());
		match run(&upload, &task_tracker).await {
			Ok(()) => task_tracker.finish(TaskState::Done, Some(t!("up.done").into_owned()), None),
			Err(e) => {
				tracing::error!("upload of {} failed: {e}", upload.package);
				task_tracker.finish(TaskState::Failed(e), None, None);
			}
		}
	});
	TaskEntry {
		abort: Some(handle.abort_handle()),
		review: None,
		tracker,
		release: None,
	}
}

async fn run(upload: &Upload, tracker: &Tracker) -> Result<(), String> {
	let device = &upload.device;
	let package = &upload.package;

	tracker.stage(t!("up.looking_up"));
	let apk_remote = device.apk_path(package).await.map_err(|e| e.to_string())?;
	let apk_dir = apk_remote.rsplit_once('/').map_or("/", |(dir, _)| dir);
	let apk_size = device
		.list_files(apk_dir)
		.await
		.map_err(|e| e.to_string())?
		.into_iter()
		.find(|(path, _)| *path == apk_remote)
		.map_or(0, |(_, size)| size);
	let obb_root = format!("/sdcard/Android/obb/{package}");
	let obb_files = device
		.list_files(&obb_root)
		.await
		.map_err(|e| e.to_string())?;

	let package_dir = upload.work_dir.join(package);
	let local_apk = package_dir.join(format!("{package}.apk"));
	let mut pulls = vec![(apk_remote, local_apk.clone())];
	for (remote_path, _) in &obb_files {
		let Some(relative) = remote_path.strip_prefix(&format!("{obb_root}/")) else {
			continue;
		};
		if relative
			.split('/')
			.any(|part| part == ".." || part.is_empty())
		{
			continue;
		}
		pulls.push((
			remote_path.clone(),
			package_dir.join(package).join(relative),
		));
	}
	let total = apk_size + obb_files.iter().map(|(_, size)| size).sum::<u64>();

	tokio::fs::create_dir_all(&package_dir)
		.await
		.map_err(|e| format!("{}: {e}", package_dir.display()))?;
	// The pulled files and their zip exist side by side.
	let available = fs4::available_space(&package_dir).map_err(|e| e.to_string())?;
	if available < total.saturating_mul(2) {
		return Err(t!(
			"up.no_space",
			need = format_bytes(total.saturating_mul(2)),
			have = format_bytes(available)
		)
		.into_owned());
	}

	tracker.stage(t!("up.copying", size = format_bytes(total)));
	let done = std::sync::atomic::AtomicU64::new(0);
	let progress = |n: u64| {
		let now = done.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n;
		tracker.bytes(now, Some(total.max(now)));
	};
	device
		.pull(&pulls, &progress)
		.await
		.map_err(|e| e.to_string())?;

	let apk_path = local_apk.clone();
	let meta = tokio::task::spawn_blocking(move || read_apk_meta(&apk_path))
		.await
		.map_err(|e| e.to_string())?
		.map_err(|e| e.to_string())?;
	if meta.package != *package {
		return Err(t!("up.wrong_apk", got = meta.package, package = package).into_owned());
	}
	let version = meta
		.version_code
		.ok_or_else(|| t!("up.no_version").into_owned())?;
	let name = meta.label.clone().unwrap_or_else(|| upload.name.clone());
	tracker.update(|s| s.title = format!("{name} v{version}"), true);

	let model = device
		.shell("getprop ro.product.model")
		.await
		.map(|out| out.stdout.trim().to_owned())
		.unwrap_or_default();
	let id = hwid(device.serial());
	tokio::fs::write(package_dir.join("HWID.txt"), &id)
		.await
		.map_err(|e| e.to_string())?;

	tracker.stage(t!("up.packing"));
	let zip_path = upload.work_dir.join(archive_name(
		&name,
		version,
		package,
		&id,
		if model.is_empty() { "unknown" } else { &model },
	));
	{
		let (dir, zip) = (package_dir.clone(), zip_path.clone());
		let progress = |done: u64, total: u64| tracker.bytes(done, Some(total));
		tokio::task::block_in_place(|| pack(&dir, &zip, &progress)).map_err(|e| e.to_string())?;
	}
	let _ = tokio::fs::remove_dir_all(&package_dir).await;

	let _turn = match upload.lock.try_lock() {
		Ok(guard) => guard,
		Err(_) => {
			tracker.stage(t!("up.waiting"));
			upload.lock.lock().await
		}
	};
	tracker.stage(t!("up.uploading"));
	let progress = |done: u64, total: Option<u64>| tracker.bytes(done, total);
	upload
		.fetcher
		.rclone_upload(
			&RcloneAccess {
				config_file: &upload.target.config_file,
			},
			&upload.target.remote,
			&upload.target.dir,
			&[],
			&zip_path,
			&progress,
		)
		.await
		.map_err(|e| e.to_string())
}
