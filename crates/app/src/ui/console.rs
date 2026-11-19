use rust_i18n::t;
use std::collections::VecDeque;

use quest_sideloader_core::{Command, ShellEvent};

/// Older lines are dropped past this, so `logcat` can run for hours.
const MAX_LINES: usize = 20_000;
const MAX_HISTORY: usize = 200;
const INPUT_ID: &str = "console_command";

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

pub struct ConsoleView {
	input: String,
	history: Vec<String>,
	/// Position while walking the history with the arrow keys.
	browsing: Option<usize>,
	lines: VecDeque<Line>,
	/// The last line hasn't ended with a newline yet.
	open_line: bool,
	running: Option<u64>,
	next_id: u64,
}

impl ConsoleView {
	pub fn new() -> Self {
		Self {
			input: String::new(),
			history: Vec::new(),
			browsing: None,
			lines: VecDeque::new(),
			open_line: false,
			running: None,
			next_id: 0,
		}
	}

	pub fn on_output(&mut self, id: u64, output: ShellEvent) {
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
			}
			ShellEvent::Failed(error) => {
				self.running = None;
				self.push(Kind::Error, error);
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
	}

	fn run(&mut self, serial: &str, commands: &mut Vec<Command>) {
		let typed = self.input.trim().to_owned();
		if typed.is_empty() {
			return;
		}
		self.input.clear();
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
			return;
		};
		self.next_id += 1;
		self.running = Some(self.next_id);
		commands.push(Command::Shell {
			id: self.next_id,
			serial: serial.to_owned(),
			command: command.to_owned(),
		});
	}

	fn walk_history(&mut self, back: bool) {
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
		self.input = next.map(|i| self.history[i].clone()).unwrap_or_default();
	}

	/// `target` is the headset's serial and name.
	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		target: Option<(&str, &str)>,
		commands: &mut Vec<Command>,
	) {
		let input_id = egui::Id::unique(INPUT_ID);
		egui::Panel::top("console_toolbar").show(ui, |ui| {
			ui.horizontal(|ui| {
				match target {
					Some((_, name)) => ui.strong(t!("console.on", name = name)),
					None => ui.weak(t!("console.no_headset")),
				};
				ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
					if ui
						.add_enabled(
							!self.lines.is_empty(),
							egui::Button::new(t!("console.clear")),
						)
						.clicked()
					{
						self.lines.clear();
						self.open_line = false;
					}
					if ui
						.add_enabled(
							!self.lines.is_empty(),
							egui::Button::new(t!("console.copy")),
						)
						.clicked()
					{
						let text: Vec<&str> = self.lines.iter().map(|l| l.text.as_str()).collect();
						ui.ctx().copy_text(text.join("\n"));
					}
				});
			});
		});
		egui::Panel::bottom("console_input").show(ui, |ui| {
			ui.add_space(4.0);
			ui.horizontal(|ui| {
				let running = self.running;
				ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
					if let Some(id) = running {
						if ui.button(t!("console.stop")).clicked() {
							commands.push(Command::StopShell(id));
						}
						ui.spinner();
					} else if ui
						.add_enabled(
							target.is_some() && !self.input.trim().is_empty(),
							egui::Button::new(t!("console.run")),
						)
						.clicked()
						&& let Some((serial, _)) = target
					{
						self.run(serial, commands);
						ui.memory_mut(|m| m.request_focus(input_id));
					}
					let field = ui.add_sized(
						ui.available_size(),
						egui::TextEdit::singleline(&mut self.input)
							.id(input_id)
							.font(egui::TextStyle::Monospace)
							.hint_text(t!("console.hint")),
					);
					if field.has_focus() {
						let (up, down) = ui.input_mut(|i| {
							(
								i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
								i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
							)
						});
						if up || down {
							self.walk_history(up);
						}
					}
					if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
						if running.is_none()
							&& let Some((serial, _)) = target
						{
							self.run(serial, commands);
						}
						ui.memory_mut(|m| m.request_focus(input_id));
					}
				});
			});
			ui.add_space(2.0);
		});
		egui::CentralPanel::default().show(ui, |ui| {
			if self.lines.is_empty() {
				ui.centered_and_justified(|ui| ui.weak(t!("console.empty")));
				return;
			}
			let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
			let visuals = ui.visuals().clone();
			egui::ScrollArea::both()
				.auto_shrink(false)
				.stick_to_bottom(true)
				.show_rows(ui, row_height, self.lines.len(), |ui, range| {
					ui.spacing_mut().item_spacing.y = 0.0;
					for line in self.lines.range(range) {
						let text = egui::RichText::new(&line.text).monospace();
						let text = match line.kind {
							Kind::Command => text.strong(),
							Kind::Stdout => text,
							Kind::Stderr => text.color(visuals.warn_fg_color),
							Kind::Info => text.weak(),
							Kind::Error => text.color(visuals.error_fg_color),
						};
						ui.add(egui::Label::new(text).extend());
					}
				});
		});
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

#[cfg(test)]
mod tests {
	use super::*;

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

	#[test]
	fn output_is_split_into_lines() {
		let mut view = ConsoleView::new();
		view.running = Some(1);
		view.on_output(1, ShellEvent::Stdout("one\ntw".into()));
		view.on_output(1, ShellEvent::Stdout("o\nthree\n".into()));
		view.on_output(1, ShellEvent::Stderr("bad\r\n".into()));
		view.on_output(2, ShellEvent::Stdout("not ours".into()));
		view.on_output(1, ShellEvent::Exited(Some(1)));
		let lines: Vec<(Kind, &str)> = view
			.lines
			.iter()
			.map(|l| (l.kind, l.text.as_str()))
			.collect();
		assert_eq!(
			lines[..4],
			[
				(Kind::Stdout, "one"),
				(Kind::Stdout, "two"),
				(Kind::Stdout, "three"),
				(Kind::Stderr, "bad"),
			]
		);
		assert_eq!(lines[4].0, Kind::Info);
		assert_eq!(view.running, None);
	}

	#[test]
	fn history_walks_back_and_forth() {
		let mut view = ConsoleView::new();
		view.history = vec!["a".into(), "b".into()];
		view.walk_history(true);
		assert_eq!(view.input, "b");
		view.walk_history(true);
		view.walk_history(true);
		assert_eq!(view.input, "a");
		view.walk_history(false);
		assert_eq!(view.input, "b");
		view.walk_history(false);
		assert_eq!(view.input, "");
	}
}
