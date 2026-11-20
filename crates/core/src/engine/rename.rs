//! Building renamed copies as tasks.

use rust_i18n::t;

use super::EventSink;
use super::task::{TaskEntry, TaskKind, TaskSnapshot, TaskState, Tracker};
use crate::rename::{BuildRequest, KeyStore, build};

pub(super) fn spawn(
	id: u64,
	request: BuildRequest,
	keys: KeyStore,
	title: String,
	sink: EventSink,
) -> TaskEntry {
	let tracker = Tracker::new(
		TaskSnapshot {
			id,
			kind: TaskKind::Rename,
			serial: None,
			title,
			stage: t!("task.starting").into_owned(),
			progress: None,
			detail: None,
			state: TaskState::Running,
			warnings: Vec::new(),
			output: None,
			conflict: None,
		},
		sink,
	);
	let t = tracker.clone();
	let handle = tokio::spawn(async move {
		let reporter = t.clone();
		let result =
			tokio::task::spawn_blocking(move || build(&request, &keys, &|u| reporter.report(u)))
				.await;
		match result {
			Ok(Ok(out)) => {
				let stage = t!("ren.built", path = out.display()).into_owned();
				t.finish(TaskState::Done, Some(stage), Some(out));
			}
			Ok(Err(e)) => {
				tracing::error!("{e}");
				t.finish(TaskState::Failed(e.to_string()), None, None);
			}
			Err(e) => t.finish(TaskState::Failed(e.to_string()), None, None),
		}
	});
	TaskEntry {
		abort: Some(handle.abort_handle()),
		review: None,
		tracker,
		release: None,
	}
}
