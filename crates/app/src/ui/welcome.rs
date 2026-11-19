use rust_i18n::t;
use std::path::Path;

use quest_sideloader_core::settings::{Language, Theme};
use quest_sideloader_core::{Command, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
	Look,
	Folder,
	Headset,
}

/// What the app should do after the wizard closes.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
	Closed,
	OpenMirrors,
}

/// The first-run setup, shown while there is no settings file yet. Closing it
/// any way saves the settings, so it doesn't come back.
pub struct Welcome {
	open: bool,
	step: Step,
	draft: Settings,
	downloads_text: String,
}

impl Welcome {
	pub fn new(first_run: bool, settings: &Settings) -> Self {
		Self {
			open: first_run,
			step: Step::Look,
			downloads_text: settings.downloads_dir.display().to_string(),
			draft: settings.clone(),
		}
	}

	pub fn is_open(&self) -> bool {
		self.open
	}

	pub fn show(
		&mut self,
		ctx: &egui::Context,
		default_downloads: &Path,
		commands: &mut Vec<Command>,
	) -> Option<Outcome> {
		if !self.open {
			return None;
		}
		let (theme, language) = (self.draft.theme, self.draft.language);
		let mut outcome = None;
		egui::Modal::new(egui::Id::unique("welcome")).show(ctx, |ui| {
			ui.set_width(520.0);
			ui.heading(t!("welcome.title"));
			ui.weak(t!("welcome.step", n = self.step as usize + 1, total = 3));
			ui.add_space(8.0);
			match self.step {
				Step::Look => self.look(ui),
				Step::Folder => self.folder(ui, default_downloads),
				Step::Headset => headset(ui),
			}
			ui.add_space(12.0);
			ui.separator();
			outcome = self.buttons(ui);
		});
		// Applied right away, so the wizard itself shows the choice.
		if self.draft.theme != theme {
			crate::app::apply_theme(ctx, self.draft.theme);
		}
		if self.draft.language != language {
			crate::app::apply_language(self.draft.language);
		}
		if outcome.is_some() {
			self.open = false;
			let mut settings = self.draft.clone();
			if settings.downloads_dir.as_os_str().is_empty() {
				settings.downloads_dir = default_downloads.to_path_buf();
			}
			settings.normalize();
			commands.push(Command::SaveSettings(settings));
		}
		outcome
	}

	fn look(&mut self, ui: &mut egui::Ui) {
		ui.label(t!("welcome.intro"));
		ui.add_space(8.0);
		egui::Grid::new("welcome_look")
			.num_columns(2)
			.spacing([16.0, 10.0])
			.show(ui, |ui| {
				ui.label(t!("set.language"));
				ui.horizontal(|ui| {
					let language = &mut self.draft.language;
					ui.selectable_value(language, Language::System, t!("set.language_system"));
					// Language names stay in their own language.
					ui.selectable_value(language, Language::English, "English");
					ui.selectable_value(language, Language::Russian, "Русский");
				});
				ui.end_row();

				ui.label(t!("set.theme"));
				ui.horizontal(|ui| {
					let theme = &mut self.draft.theme;
					ui.selectable_value(theme, Theme::System, t!("set.theme_system"));
					ui.selectable_value(theme, Theme::Dark, t!("set.theme_dark"));
					ui.selectable_value(theme, Theme::Light, t!("set.theme_light"));
				});
				ui.end_row();
			});
	}

	fn folder(&mut self, ui: &mut egui::Ui, default_downloads: &Path) {
		ui.label(t!("welcome.folder_text"));
		ui.add_space(8.0);
		ui.horizontal(|ui| {
			if ui
				.add(egui::TextEdit::singleline(&mut self.downloads_text).desired_width(300.0))
				.changed()
			{
				self.draft.downloads_dir = self.downloads_text.trim().into();
			}
			if ui.button(t!("common.browse")).clicked()
				&& let Some(dir) = rfd::FileDialog::new()
					.set_directory(&self.draft.downloads_dir)
					.pick_folder()
			{
				self.downloads_text = dir.display().to_string();
				self.draft.downloads_dir = dir;
			}
			if ui.button(t!("set.default")).clicked() {
				self.downloads_text = default_downloads.display().to_string();
				self.draft.downloads_dir = default_downloads.to_path_buf();
			}
		});
		ui.add_space(4.0);
		ui.checkbox(&mut self.draft.keep_downloads, t!("set.keep_downloads"));
	}

	fn buttons(&mut self, ui: &mut egui::Ui) -> Option<Outcome> {
		let mut outcome = None;
		ui.horizontal(|ui| {
			if ui.button(t!("welcome.skip")).clicked() {
				outcome = Some(Outcome::Closed);
			}
			ui.with_layout(
				egui::Layout::right_to_left(egui::Align::Center),
				|ui| match self.step {
					Step::Look => {
						if ui.button(t!("welcome.next")).clicked() {
							self.step = Step::Folder;
						}
					}
					Step::Folder => {
						if ui.button(t!("welcome.next")).clicked() {
							self.step = Step::Headset;
						}
						if ui.button(t!("welcome.back")).clicked() {
							self.step = Step::Look;
						}
					}
					Step::Headset => {
						if ui.button(t!("welcome.finish")).clicked() {
							outcome = Some(Outcome::Closed);
						}
						if ui.button(t!("welcome.add_mirror")).clicked() {
							outcome = Some(Outcome::OpenMirrors);
						}
						if ui.button(t!("welcome.back")).clicked() {
							self.step = Step::Folder;
						}
					}
				},
			);
		});
		outcome
	}
}

fn headset(ui: &mut egui::Ui) {
	ui.label(t!("welcome.headset_text"));
	ui.add_space(8.0);
	ui.label(t!("welcome.mirror_text"));
}
