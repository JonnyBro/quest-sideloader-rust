//! Shell commands typed in the console tab.

use tokio::task::AbortHandle;

use super::{Event, EventSink};
use crate::adb::{Device, OutputStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
	Stdout(String),
	Stderr(String),
	Exited(Option<u8>),
	Failed(String),
}

pub(super) fn spawn(id: u64, device: Device, command: String, sink: EventSink) -> AbortHandle {
	tokio::spawn(async move {
		let emit = |output| sink.emit(Event::Shell { id, output });
		let result = device
			.shell_streaming(&command, |stream, text| {
				emit(match stream {
					OutputStream::Stdout => ShellEvent::Stdout(text),
					OutputStream::Stderr => ShellEvent::Stderr(text),
				})
			})
			.await;
		emit(match result {
			Ok(code) => ShellEvent::Exited(code),
			Err(e) => ShellEvent::Failed(e.to_string()),
		});
	})
	.abort_handle()
}
