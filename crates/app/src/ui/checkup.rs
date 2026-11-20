use rust_i18n::t;

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
	Ok,
	Warning,
	Problem,
	/// Not set up, and that's fine.
	Skipped,
}

pub struct Check {
	pub level: Level,
	pub title: String,
	pub detail: String,
}

impl Check {
	pub fn new(level: Level, title: impl Into<String>, detail: impl Into<String>) -> Self {
		Self {
			level,
			title: title.into(),
			detail: detail.into(),
		}
	}
}

/// "Check everything": what's ready and what to do about what isn't.
#[derive(Default)]
pub struct Checkup {
	open: bool,
}

impl Checkup {
	pub fn open(&mut self) {
		self.open = true;
	}

	pub fn is_open(&self) -> bool {
		self.open
	}

	pub fn show(&mut self, ctx: &egui::Context, checks: &[Check]) {
		if !self.open {
			return;
		}
		let mut close = false;
		let modal = egui::Modal::new(egui::Id::unique("checkup")).show(ctx, |ui| {
			ui.set_width(520.0);
			ui.heading(t!("chk.title"));
			ui.add_space(6.0);
			egui::Grid::new("checkup_grid")
				.num_columns(2)
				.spacing([12.0, 10.0])
				.show(ui, |ui| {
					for check in checks {
						let (label, color) = match check.level {
							Level::Ok => (t!("chk.ok"), theme::GOOD),
							Level::Warning => (t!("chk.warning"), theme::WARN),
							Level::Problem => (t!("chk.problem"), theme::BAD),
							Level::Skipped => (t!("chk.skipped"), theme::MUTED),
						};
						theme::pill(ui, label, color);
						ui.vertical(|ui| {
							ui.strong(&check.title);
							if !check.detail.is_empty() {
								ui.add(egui::Label::new(&check.detail).wrap());
							}
						});
						ui.end_row();
					}
				});
			ui.add_space(8.0);
			close = ui.button(t!("common.close")).clicked();
		});
		if close || modal.should_close() {
			self.open = false;
		}
	}
}
