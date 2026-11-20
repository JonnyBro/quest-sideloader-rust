use rust_i18n::t;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
	Editor, EditorState, FoldRange, HighlightStyleResolver, Input, InputEdit, InputEvent,
	InputHighlighter, InputState, Rope, RopeExt,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::{Command, Engine, ShellEvent};

use crate::theme::Mono;

/// Older lines are dropped past this, so `logcat` can run for hours.
const MAX_LINES: usize = 20_000;
const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
	Command,
	Stdout,
	Stderr,
	Info,
	Error,
}

struct Line {
	kind: Kind,
	text: String,
}

/// Colors of the line kinds, shared with the output's highlighter.
#[derive(Clone, Copy, Default)]
struct Palette {
	command: Hsla,
	stderr: Hsla,
	info: Hsla,
	error: Hsla,
}

/// The kind of every line in the output editor, in order.
type Kinds = Rc<RefCell<Vec<Kind>>>;

/// `adb shell` on the selected headset, line by line.
pub struct ConsoleView {
	engine: Rc<Engine>,
	/// Serial and name of the headset commands run on.
	target: Option<(String, String)>,
	input: Entity<InputState>,
	output: Entity<EditorState>,
	kinds: Kinds,
	palette: Rc<RefCell<Palette>>,
	history: Vec<String>,
	/// Position while walking the history with the arrow keys.
	browsing: Option<usize>,
	lines: VecDeque<Line>,
	/// The last line hasn't ended with a newline yet.
	open_line: bool,
	/// Lines changed since the editor last showed them.
	dirty: bool,
	running: Option<u64>,
	next_id: u64,
	_subscriptions: Vec<Subscription>,
	_sync: Task<()>,
}

impl ConsoleView {
	pub fn new(engine: Rc<Engine>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("console.hint")));
		let kinds = Kinds::default();
		let palette = Rc::new(RefCell::new(Palette::default()));
		let output = cx.new(|cx| {
			let mut editor = EditorState::new(window, cx)
				.line_number(false)
				.soft_wrap(false);
			editor.set_readonly(true, cx);
			let (kinds, palette) = (kinds.clone(), palette.clone());
			editor.set_highlighter_factory(
				Rc::new(move |_| {
					Some(Box::new(KindHighlighter {
						kinds: kinds.clone(),
						palette: palette.clone(),
						rows: Vec::new(),
					}))
				}),
				cx,
			);
			editor.set_highlighter("console", cx);
			editor
		});
		let input_events = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
			if let InputEvent::PressEnter { .. } = event {
				this.run(window, cx);
			}
		});
		// Output can arrive many times a second; the editor catches up a few times a second.
		let sync = cx.spawn_in(window, async move |this, cx| {
			loop {
				cx.background_executor()
					.timer(Duration::from_millis(150))
					.await;
				if this
					.update_in(cx, |this, window, cx| this.sync_output(window, cx))
					.is_err()
				{
					break;
				}
			}
		});
		Self {
			engine,
			target: None,
			input,
			output,
			kinds,
			palette,
			history: Vec::new(),
			browsing: None,
			lines: VecDeque::new(),
			open_line: false,
			dirty: false,
			running: None,
			next_id: 0,
			_subscriptions: vec![input_events],
			_sync: sync,
		}
	}

	pub fn set_target(&mut self, target: Option<(String, String)>, cx: &mut Context<Self>) {
		self.target = target;
		cx.notify();
	}

	pub fn on_output(&mut self, id: u64, output: ShellEvent, cx: &mut Context<Self>) {
		if self.running != Some(id) {
			return;
		}
		match output {
			ShellEvent::Stdout(text) => self.append(Kind::Stdout, &text),
			ShellEvent::Stderr(text) => self.append(Kind::Stderr, &text),
			ShellEvent::Exited(code) => {
				self.running = None;
				let text = match code {
					Some(code) => t!("console.exit_code", code = code),
					None => t!("console.ended"),
				};
				self.push(Kind::Info, text.into_owned());
				cx.notify();
			}
			ShellEvent::Failed(error) => {
				self.running = None;
				self.push(Kind::Error, error);
				cx.notify();
			}
		}
	}

	fn append(&mut self, kind: Kind, text: &str) {
		let mut pieces = text.split('\n').peekable();
		let mut first = true;
		while let Some(piece) = pieces.next() {
			let last = pieces.peek().is_none();
			match self.lines.back_mut() {
				Some(line) if first && self.open_line && line.kind == kind => {
					line.text.push_str(piece.trim_end_matches('\r'));
					self.dirty = true;
				}
				// The empty piece after a trailing newline: nothing to show yet.
				_ if last && piece.is_empty() => {}
				_ => self.add(kind, piece.to_owned()),
			}
			first = false;
		}
		self.open_line = !text.ends_with('\n');
	}

	fn push(&mut self, kind: Kind, text: String) {
		self.add(kind, text);
		self.open_line = false;
	}

	fn add(&mut self, kind: Kind, text: String) {
		if self.lines.len() == MAX_LINES {
			self.lines.pop_front();
		}
		self.lines.push_back(Line {
			kind,
			text: text.trim_end_matches('\r').to_owned(),
		});
		self.dirty = true;
	}

	fn sync_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		if !self.dirty {
			return;
		}
		let text: Vec<&str> = self.lines.iter().map(|l| l.text.as_str()).collect();
		let text = text.join("\n");
		let kinds: Vec<Kind> = self.lines.iter().map(|l| l.kind).collect();
		let shown = self.kinds.borrow().len();
		let kinds_cell = self.kinds.clone();
		let updated = self.output.update(cx, |editor, cx| {
			// Replacing the text drops the selection; wait until the user is done with it.
			if !editor.selected_range().is_empty() {
				return false;
			}
			let at_bottom = editor
				.visible_row_range()
				.is_none_or(|rows| rows.end + 1 >= shown);
			*kinds_cell.borrow_mut() = kinds;
			editor.set_value(text, window, cx);
			if at_bottom {
				// Clamped to the last line on the next layout.
				editor.set_scroll_offset(point(px(0.), px(-1e9)), cx);
			}
			true
		});
		if updated {
			self.dirty = false;
		}
	}

	fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		if self.running.is_some() {
			return;
		}
		let Some((serial, _)) = self.target.clone() else {
			return;
		};
		let typed = self.input.read(cx).value().trim().to_owned();
		if typed.is_empty() {
			return;
		}
		self.input
			.update(cx, |input, cx| input.set_value("", window, cx));
		self.browsing = None;
		if self.history.last() != Some(&typed) {
			self.history.push(typed.clone());
			if self.history.len() > MAX_HISTORY {
				self.history.remove(0);
			}
		}
		self.push(Kind::Command, format!("$ {typed}"));
		let Some(command) = shell_command(&typed) else {
			self.push(Kind::Error, t!("console.only_shell").into_owned());
			cx.notify();
			return;
		};
		self.next_id += 1;
		self.running = Some(self.next_id);
		self.engine.send(Command::Shell {
			id: self.next_id,
			serial,
			command: command.to_owned(),
		});
		cx.notify();
	}

	fn walk_history(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
		if self.history.is_empty() {
			return;
		}
		let next = match (self.browsing, back) {
			(None, true) => Some(self.history.len() - 1),
			(None, false) => None,
			(Some(i), true) => Some(i.saturating_sub(1)),
			(Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
			(Some(_), false) => None,
		};
		self.browsing = next;
		let text = next.map(|i| self.history[i].clone()).unwrap_or_default();
		self.input
			.update(cx, |input, cx| input.set_value(text, window, cx));
	}

	fn clear(&mut self, cx: &mut Context<Self>) {
		self.lines.clear();
		self.open_line = false;
		self.dirty = true;
		cx.notify();
	}
}

/// Colors each line of the output by its kind.
struct KindHighlighter {
	kinds: Kinds,
	palette: Rc<RefCell<Palette>>,
	/// Byte range of every line, newline included.
	rows: Vec<Range<usize>>,
}

impl InputHighlighter for KindHighlighter {
	fn language(&self) -> SharedString {
		"console".into()
	}

	fn update(
		&mut self,
		_edit: Option<InputEdit>,
		text: &Rope,
		_folding: bool,
		_window: &mut Window,
		_cx: &mut Context<EditorState>,
	) {
		self.rows.clear();
		let count = text.lines_len();
		let end = if count == 0 {
			0
		} else {
			text.line_end_offset(count - 1)
		};
		for row in 0..count {
			let start = text.line_start_offset(row);
			let next = if row + 1 < count {
				text.line_start_offset(row + 1)
			} else {
				end
			};
			self.rows.push(start..next);
		}
	}

	fn styles(
		&self,
		range: &Range<usize>,
		_resolver: &dyn HighlightStyleResolver,
	) -> Vec<(Range<usize>, HighlightStyle)> {
		let kinds = self.kinds.borrow();
		let palette = *self.palette.borrow();
		let first = self.rows.partition_point(|r| r.end <= range.start);
		let mut runs = Vec::new();
		let mut at = range.start;
		for (row, line) in self.rows.iter().enumerate().skip(first) {
			if line.start >= range.end {
				break;
			}
			let end = line.end.min(range.end);
			if end <= at {
				continue;
			}
			let style = match kinds.get(row) {
				Some(Kind::Command) => HighlightStyle {
					color: Some(palette.command),
					font_weight: Some(FontWeight::SEMIBOLD),
					..Default::default()
				},
				Some(Kind::Stderr) => HighlightStyle::color(palette.stderr),
				Some(Kind::Info) => HighlightStyle::color(palette.info),
				Some(Kind::Error) => HighlightStyle::color(palette.error),
				_ => HighlightStyle::default(),
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

/// What to run in `adb shell`; `adb shell` typed in front is dropped, and
/// other adb commands aren't supported.
fn shell_command(typed: &str) -> Option<&str> {
	let Some(rest) = typed.strip_prefix("adb ") else {
		return Some(typed);
	};
	let rest = rest.trim_start();
	match rest.strip_prefix("shell") {
		Some("") => None,
		Some(command) if command.starts_with(' ') => Some(command.trim_start()),
		_ => None,
	}
}

impl Render for ConsoleView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		*self.palette.borrow_mut() = Palette {
			command: theme.primary,
			stderr: theme.warning,
			info: theme.muted_foreground,
			error: theme.danger,
		};
		let empty = self.lines.is_empty();
		let header = h_flex()
			.px_5()
			.pt_4()
			.pb_3()
			.gap_3()
			.border_b_1()
			.border_color(theme.border)
			.child(div().text_xl().child(t!("tab.console").into_owned()))
			.child(
				div()
					.flex_1()
					.min_w_0()
					.truncate()
					.text_color(theme.muted_foreground)
					.child(match &self.target {
						Some((_, name)) => t!("console.on", name = name).into_owned(),
						None => t!("console.no_headset").into_owned(),
					}),
			)
			.child(
				Button::new("copy-all")
					.label(t!("console.copy"))
					.small()
					.outline()
					.disabled(empty)
					.on_click(cx.listener(|this, _, _, cx| {
						let text: Vec<&str> = this.lines.iter().map(|l| l.text.as_str()).collect();
						cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
					})),
			)
			.child(
				Button::new("clear")
					.label(t!("console.clear"))
					.small()
					.outline()
					.disabled(empty)
					.on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
			);
		let output = if empty {
			div()
				.size_full()
				.flex()
				.items_center()
				.justify_center()
				.p_8()
				.text_color(theme.muted_foreground)
				.child(t!("console.empty").into_owned())
				.into_any_element()
		} else {
			Editor::new(&self.output)
				.readonly(true)
				.bordered(false)
				.appearance(false)
				.h_full()
				.into_any_element()
		};
		let running = self.running;
		let can_run = self.target.is_some() && !self.input.read(cx).value().trim().is_empty();
		let input_row = h_flex()
			.px_5()
			.py_3()
			.gap_2()
			.border_t_1()
			.border_color(theme.border)
			.child(div().text_color(theme.muted_foreground).mono(cx).child("$"))
			.child(
				div()
					.flex_1()
					.mono(cx)
					// Arrows walk the history before the input moves its cursor.
					.capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
						let key = event.keystroke.key.as_str();
						if event.keystroke.modifiers.modified() {
							return;
						}
						if key == "up" || key == "down" {
							this.walk_history(key == "up", window, cx);
							cx.stop_propagation();
						}
					}))
					.child(Input::new(&self.input).disabled(self.target.is_none())),
			)
			.child(match running {
				Some(id) => h_flex()
					.gap_2()
					.child(Spinner::new().small())
					.child(
						Button::new("stop")
							.label(t!("console.stop"))
							.danger()
							.on_click(cx.listener(move |this, _, _, _| {
								this.engine.send(Command::StopShell(id))
							})),
					)
					.into_any_element(),
				None => Button::new("run")
					.label(t!("console.run"))
					.primary()
					.disabled(!can_run)
					.on_click(cx.listener(|this, _, window, cx| this.run(window, cx)))
					.into_any_element(),
			});
		v_flex()
			.size_full()
			.child(header)
			.child(div().flex_1().min_h_0().bg(theme.sidebar).child(output))
			.child(input_row)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ::core::prelude::v1::test;

	#[test]
	fn adb_shell_prefix_is_dropped() {
		assert_eq!(shell_command("ls /sdcard"), Some("ls /sdcard"));
		assert_eq!(shell_command("adb shell ls /sdcard"), Some("ls /sdcard"));
		assert_eq!(
			shell_command("adb  shell  pm list packages"),
			Some("pm list packages")
		);
		assert_eq!(shell_command("adb shell"), None);
		assert_eq!(shell_command("adb install x.apk"), None);
		assert_eq!(shell_command("adb shellx"), None);
	}
}
