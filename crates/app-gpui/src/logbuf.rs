use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

const MAX_LINES: usize = 2000;

/// The last log lines, for the logs panel.
#[derive(Clone, Default)]
pub struct LogBuffer {
	inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
	lines: VecDeque<String>,
	/// Bumped on every line, so the panel knows when to redraw.
	generation: u64,
	/// Error lines written so far, for the badge on the logs button.
	errors: u64,
	partial: String,
}

impl LogBuffer {
	pub fn generation(&self) -> u64 {
		self.inner.lock().map_or(0, |i| i.generation)
	}

	pub fn errors(&self) -> u64 {
		self.inner.lock().map_or(0, |i| i.errors)
	}

	pub fn lines(&self) -> Vec<String> {
		self.inner
			.lock()
			.map(|i| i.lines.iter().cloned().collect())
			.unwrap_or_default()
	}
}

impl Write for LogBuffer {
	fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
		if let Ok(mut inner) = self.inner.lock() {
			inner.partial.push_str(&String::from_utf8_lossy(buf));
			while let Some(end) = inner.partial.find('\n') {
				let line: String = inner.partial.drain(..=end).collect();
				if level_of(&line) == Some(Level::Error) {
					inner.errors += 1;
				}
				inner.lines.push_back(line.trim_end().to_owned());
				if inner.lines.len() > MAX_LINES {
					inner.lines.pop_front();
				}
				inner.generation += 1;
			}
		}
		Ok(buf.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
	Trace,
	Debug,
	Info,
	Warn,
	Error,
}

/// The level the fmt layer wrote near the start of a line, padded like `" WARN "`.
pub fn level_of(line: &str) -> Option<Level> {
	let head = line.get(..line.len().min(48)).unwrap_or(line);
	[
		(" ERROR ", Level::Error),
		(" WARN ", Level::Warn),
		(" INFO ", Level::Info),
		(" DEBUG ", Level::Debug),
		(" TRACE ", Level::Trace),
	]
	.into_iter()
	.find(|(tag, _)| head.contains(tag))
	.map(|(_, level)| level)
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
	type Writer = LogBuffer;

	fn make_writer(&'a self) -> Self::Writer {
		self.clone()
	}
}

/// Colors log lines by level in the logs editor.
pub mod highlight {
	use std::cell::Cell;
	use std::ops::Range;
	use std::rc::Rc;

	use gpui_kit::component::input::{
		EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope, RopeExt,
	};
	use gpui_kit::*;

	#[derive(Clone, Copy, Default)]
	pub struct LevelColors {
		pub error: Hsla,
		pub warn: Hsla,
		pub debug: Hsla,
	}

	pub type SharedColors = Rc<Cell<LevelColors>>;

	#[derive(Clone, Copy)]
	enum Level {
		Error,
		Warn,
		Debug,
	}

	pub struct LogHighlighter {
		colors: SharedColors,
		/// Byte range of every line, newline included, and its level.
		lines: Vec<(Range<usize>, Option<Level>)>,
	}

	impl LogHighlighter {
		pub fn new(colors: SharedColors) -> Self {
			Self {
				colors,
				lines: Vec::new(),
			}
		}
	}

	impl InputHighlighter for LogHighlighter {
		fn language(&self) -> SharedString {
			"log".into()
		}

		fn update(
			&mut self,
			_edit: Option<InputEdit>,
			text: &Rope,
			_folding: bool,
			_window: &mut Window,
			_cx: &mut Context<EditorState>,
		) {
			self.lines.clear();
			let rows = text.lines_len();
			let end = if rows == 0 {
				0
			} else {
				text.line_end_offset(rows - 1)
			};
			for row in 0..rows {
				let start = text.line_start_offset(row);
				let next = if row + 1 < rows {
					text.line_start_offset(row + 1)
				} else {
					end
				};
				// The fmt layer pads the level, e.g. "  WARN"; check only the line's start.
				let head = text.slice_line(row).to_string();
				let head = head.get(..head.len().min(48)).unwrap_or(&head);
				let level = if head.contains(" ERROR ") {
					Some(Level::Error)
				} else if head.contains(" WARN ") {
					Some(Level::Warn)
				} else if head.contains(" DEBUG ") || head.contains(" TRACE ") {
					Some(Level::Debug)
				} else {
					None
				};
				self.lines.push((start..next, level));
			}
		}

		fn styles(
			&self,
			range: &Range<usize>,
			_resolver: &dyn HighlightStyleResolver,
		) -> Vec<(Range<usize>, HighlightStyle)> {
			let colors = self.colors.get();
			let first = self.lines.partition_point(|(r, _)| r.end <= range.start);
			let mut runs = Vec::new();
			let mut at = range.start;
			for (line, level) in &self.lines[first..] {
				if line.start >= range.end {
					break;
				}
				let end = line.end.min(range.end);
				if end <= at {
					continue;
				}
				let style = match level {
					Some(Level::Error) => HighlightStyle::color(colors.error),
					Some(Level::Warn) => HighlightStyle::color(colors.warn),
					Some(Level::Debug) => HighlightStyle::color(colors.debug),
					None => HighlightStyle::default(),
				};
				runs.push((at..end, style));
				at = end;
			}
			if at < range.end {
				runs.push((at..range.end, HighlightStyle::default()));
			}
			runs
		}

		fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
			Vec::new()
		}
	}
}
