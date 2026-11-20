use rust_i18n::t;
use std::path::PathBuf;

use tracing::Level;

use crate::logbuf::LogBuffer;

pub struct LogsView {
	buffer: LogBuffer,
	folder: PathBuf,
	open: bool,
	min_level: Level,
	search: String,
	/// Error count when the window was last looked at.
	errors_seen: usize,
}

impl LogsView {
	pub fn new(buffer: LogBuffer, folder: PathBuf) -> Self {
		Self {
			buffer,
			folder,
			open: false,
			min_level: Level::INFO,
			search: String::new(),
			errors_seen: 0,
		}
	}

	/// The status bar button; red while there are errors not looked at yet.
	pub fn button(&mut self, ui: &mut egui::Ui) {
		let unseen = self.buffer.errors().saturating_sub(self.errors_seen);
		let text = if unseen > 0 {
			egui::RichText::new(t!("logs.button_errors", n = unseen))
				.color(ui.visuals().error_fg_color)
		} else {
			egui::RichText::new(t!("logs.button"))
		};
		let color = if unseen > 0 {
			ui.visuals().error_fg_color
		} else {
			ui.visuals().text_color()
		};
		let icon = crate::icons::Icon::Logs.image(14.0, color);
		if ui
			.add(egui::Button::image_and_text(icon, text).small())
			.on_hover_text(t!("logs.button_hint"))
			.clicked()
		{
			self.open = !self.open;
		}
	}

	pub fn window(&mut self, ctx: &egui::Context) {
		if !self.open {
			return;
		}
		self.errors_seen = self.buffer.errors();
		let mut open = self.open;
		egui::Window::new(t!("logs.title"))
			.id(egui::Id::unique("logs_window"))
			.open(&mut open)
			.default_size([820.0, 420.0])
			.show(ctx, |ui| {
				// Filters from the previous frame; one frame of lag is invisible.
				let lines = self
					.buffer
					.matching(self.min_level, &self.search.trim().to_lowercase());
				ui.horizontal(|ui| {
					egui::ComboBox::from_id_salt("log_level")
						.selected_text(level_name(self.min_level))
						.show_ui(ui, |ui| {
							for level in [
								Level::ERROR,
								Level::WARN,
								Level::INFO,
								Level::DEBUG,
								Level::TRACE,
							] {
								ui.selectable_value(&mut self.min_level, level, level_name(level));
							}
						});
					ui.add(
						egui::TextEdit::singleline(&mut self.search)
							.hint_text(t!("common.search"))
							.desired_width(220.0),
					);
					if ui
						.button(t!("logs.copy"))
						.on_hover_text(t!("logs.copy_hint"))
						.clicked()
					{
						let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
						ui.ctx().copy_text(text.join("\n"));
					}
					if ui.button(t!("common.open_folder")).clicked() {
						super::open_folder(&self.folder);
					}
					ui.weak(t!("logs.count", n = lines.len()));
				});
				ui.separator();
				let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
				egui::ScrollArea::both()
					.auto_shrink([false, false])
					.stick_to_bottom(true)
					.show_rows(ui, row_height, lines.len(), |ui, range| {
						for line in &lines[range] {
							let color = match line.level {
								Level::ERROR => ui.visuals().error_fg_color,
								Level::WARN => ui.visuals().warn_fg_color,
								Level::DEBUG | Level::TRACE => ui.visuals().weak_text_color(),
								_ => ui.visuals().text_color(),
							};
							ui.add(
								egui::Label::new(
									egui::RichText::new(&line.text).monospace().color(color),
								)
								.extend(),
							);
						}
					});
			});
		self.open = open;
	}
}

fn level_name(level: Level) -> std::borrow::Cow<'static, str> {
	match level {
		Level::ERROR => t!("logs.errors"),
		Level::WARN => t!("logs.warnings"),
		Level::INFO => t!("logs.info"),
		Level::DEBUG => t!("logs.debug"),
		Level::TRACE => t!("logs.everything"),
	}
}
