//! File manager jobs: listing a folder, copying files both ways, deleting.

use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::task::{TaskEntry, TaskKind, TaskSnapshot, TaskState, Tracker};
use super::{Event, EventSink};
use crate::adb::Device;
use crate::adb::browse::join;
use crate::sideload::run::Update;

pub(super) fn spawn_list(device: Device, path: String, sink: EventSink) {
	tokio::spawn(async move {
		let result = device.list_dir(&path).await.map_err(|e| e.to_string());
		sink.emit(Event::DirListing {
			serial: device.serial().to_owned(),
			path,
			result,
		});
	});
}

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

fn title(paths: &[String]) -> String {
	match paths {
		[one] => one.rsplit('/').next().unwrap_or(one).to_owned(),
		_ => t!("files.n_items", n = paths.len()).into_owned(),
	}
}

pub(super) fn spawn_pull(
	id: u64,
	device: Device,
	paths: Vec<String>,
	dest: PathBuf,
	sink: EventSink,
) -> TaskEntry {
	let tracker = tracker(
		id,
		TaskKind::PullFiles,
		&device,
		t!("files.pull_title", name = title(&paths)).into_owned(),
		&sink,
	);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		let result = async {
			t.stage(t!("files.listing"));
			let (plan, total) = device.pull_plan(&paths, &dest).await?;
			t.stage(t!("files.copying_from"));
			let done = AtomicU64::new(0);
			let progress = |n| {
				let now = done.fetch_add(n, Ordering::Relaxed) + n;
				t.bytes(now, Some(total.max(now)));
			};
			let skipped = device.pull_readable(&plan, &progress).await?;
			if let Some(first) = skipped.first() {
				t.report(Update::Warning(
					t!("hs.skipped_unreadable", n = skipped.len(), first = first).into_owned(),
				));
			}
			Ok::<_, crate::adb::AdbError>(plan.len() - skipped.len())
		}
		.await;
		match result {
			Ok(n) => t.finish(
				TaskState::Done,
				Some(t!("files.pulled", n = n).into_owned()),
				Some(dest),
			),
			Err(e) => t.finish(TaskState::Failed(e.to_string()), None, None),
		}
	});
	entry(handle, tracker)
}

pub(super) fn spawn_push(
	id: u64,
	device: Device,
	local: Vec<PathBuf>,
	dir: String,
	sink: EventSink,
) -> TaskEntry {
	let names: Vec<String> = local
		.iter()
		.map(|p| {
			p.file_name()
				.unwrap_or_default()
				.to_string_lossy()
				.into_owned()
		})
		.collect();
	let tracker = tracker(
		id,
		TaskKind::PushFiles,
		&device,
		t!("files.push_title", name = title(&names)).into_owned(),
		&sink,
	);
	let t = tracker.clone();
	let list_sink = sink.clone();
	let handle = tokio::spawn(async move {
		let sizes = local.clone();
		let total = tokio::task::spawn_blocking(move || {
			sizes
				.iter()
				.map(|p| match std::fs::metadata(p) {
					Ok(m) if m.is_dir() => crate::adb::list_files(p)
						.map(|files| files.iter().map(|f| f.size).sum())
						.unwrap_or(0),
					Ok(m) => m.len(),
					Err(_) => 0,
				})
				.sum::<u64>()
		})
		.await
		.unwrap_or(0);
		t.stage(t!("files.copying_to"));
		let done = AtomicU64::new(0);
		let progress = |n| {
			let now = done.fetch_add(n, Ordering::Relaxed) + n;
			t.bytes(now, Some(total.max(now)));
		};
		let mut result = Ok(());
		for (path, name) in local.iter().zip(&names) {
			result = device.push(path, &join(&dir, name), &progress).await;
			if result.is_err() {
				break;
			}
		}
		match result {
			Ok(()) => t.finish(
				TaskState::Done,
				Some(t!("files.pushed", n = local.len()).into_owned()),
				None,
			),
			Err(e) => t.finish(TaskState::Failed(e.to_string()), None, None),
		}
		spawn_list(device, dir, list_sink);
	});
	entry(handle, tracker)
}

pub(super) fn spawn_delete(device: Device, paths: Vec<String>, dir: String, sink: EventSink) {
	tokio::spawn(async move {
		match device.remove_paths(&paths).await {
			Ok(()) => sink.emit(Event::Notice(
				t!("files.deleted", n = paths.len()).into_owned(),
			)),
			Err(e) => sink.emit(Event::Error(e.to_string())),
		}
		spawn_list(device, dir, sink);
	});
}

pub(super) fn spawn_make_dir(device: Device, dir: String, name: String, sink: EventSink) {
	tokio::spawn(async move {
		if let Err(e) = device.make_dir(&dir, &name).await {
			sink.emit(Event::Error(e.to_string()));
		}
		spawn_list(device, dir, sink);
	});
}

pub(super) fn spawn_rename(device: Device, dir: String, from: String, to: String, sink: EventSink) {
	tokio::spawn(async move {
		if let Err(e) = device.rename_in(&dir, &from, &to).await {
			sink.emit(Event::Error(e.to_string()));
		}
		spawn_list(device, dir, sink);
	});
}

pub(super) fn spawn_paste(
	id: u64,
	device: Device,
	paths: Vec<String>,
	dir: String,
	copy: bool,
	sink: EventSink,
) -> TaskEntry {
	let name = title(&paths);
	let title = if copy {
		t!("files.copy_title", name = name)
	} else {
		t!("files.move_title", name = name)
	};
	let tracker = tracker(id, TaskKind::PasteFiles, &device, title.into_owned(), &sink);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		t.stage(if copy {
			t!("files.copying")
		} else {
			t!("files.moving")
		});
		match device.paste_into(&paths, &dir, copy).await {
			Ok(()) => t.finish(
				TaskState::Done,
				Some(t!("files.pasted", n = paths.len()).into_owned()),
				None,
			),
			Err(e) => t.finish(TaskState::Failed(e.to_string()), None, None),
		}
		spawn_list(device, dir, sink);
	});
	entry(handle, tracker)
}
