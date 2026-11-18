//! Keeps the latest log lines in memory for the in-app log window.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing::{Level, Metadata};
use tracing_subscriber::fmt::MakeWriter;

const MAX_LINES: usize = 5000;

#[derive(Debug, Clone)]
pub struct LogLine {
	pub level: Level,
	pub text: String,
}

#[derive(Default)]
struct Inner {
	lines: VecDeque<LogLine>,
	/// Errors logged since the app started; the UI compares it with what it
	/// has shown to know whether something new went wrong.
	errors: usize,
}

#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Inner>>);

impl LogBuffer {
	fn push(&self, level: Level, text: String) {
		let mut inner = self.0.lock().expect("log buffer lock poisoned");
		if level == Level::ERROR {
			inner.errors += 1;
		}
		if inner.lines.len() == MAX_LINES {
			inner.lines.pop_front();
		}
		inner.lines.push_back(LogLine { level, text });
	}

	pub fn errors(&self) -> usize {
		self.0.lock().expect("log buffer lock poisoned").errors
	}

	/// Lines at `min_level` or more severe that contain `needle` (lowercase).
	pub fn matching(&self, min_level: Level, needle: &str) -> Vec<LogLine> {
		let inner = self.0.lock().expect("log buffer lock poisoned");
		inner
			.lines
			.iter()
			// tracing orders levels by verbosity: ERROR < WARN < ... < TRACE.
			.filter(|l| l.level <= min_level)
			.filter(|l| needle.is_empty() || l.text.to_lowercase().contains(needle))
			.cloned()
			.collect()
	}
}

/// Collects one formatted event and files it when the formatter is done.
pub struct LineWriter {
	buffer: LogBuffer,
	level: Level,
	bytes: Vec<u8>,
}

impl Write for LineWriter {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		self.bytes.extend_from_slice(buf);
		Ok(buf.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

impl Drop for LineWriter {
	fn drop(&mut self) {
		let text = String::from_utf8_lossy(&self.bytes).trim_end().to_owned();
		if !text.is_empty() {
			self.buffer.push(self.level, text);
		}
	}
}

impl<'a> MakeWriter<'a> for LogBuffer {
	type Writer = LineWriter;

	fn make_writer(&'a self) -> Self::Writer {
		LineWriter {
			buffer: self.clone(),
			level: Level::INFO,
			bytes: Vec::new(),
		}
	}

	fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
		LineWriter {
			buffer: self.clone(),
			level: *meta.level(),
			bytes: Vec::new(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tracing_subscriber::prelude::*;

	#[test]
	fn collects_and_filters_lines() {
		let buffer = LogBuffer::default();
		let subscriber = tracing_subscriber::registry().with(
			tracing_subscriber::fmt::layer()
				.with_ansi(false)
				.with_writer(buffer.clone()),
		);
		tracing::subscriber::with_default(subscriber, || {
			tracing::info!("sync started");
			tracing::warn!("slow mirror");
			tracing::error!("sync failed: 403");
		});
		assert_eq!(buffer.errors(), 1);
		assert_eq!(buffer.matching(Level::TRACE, "").len(), 3);
		let warnings = buffer.matching(Level::WARN, "");
		assert_eq!(warnings.len(), 2);
		assert!(warnings[1].text.contains("sync failed: 403"));
		assert_eq!(buffer.matching(Level::TRACE, "mirror").len(), 1);
	}
}
