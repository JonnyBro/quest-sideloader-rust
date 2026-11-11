use std::path::{Path, PathBuf};

use quest_sideloader_core::settings::{MAX_CONCURRENT_DOWNLOADS, Theme};
use quest_sideloader_core::{Command, Settings};

pub struct SettingsView {
	saved: Settings,
	draft: Settings,
	downloads_dir_text: String,
	adb_path_text: String,
	saving: bool,
}

impl SettingsView {
	pub fn new(settings: Settings) -> Self {
		Self {
			downloads_dir_text: settings.downloads_dir.display().to_string(),
			adb_path_text: settings
				.adb_path
				.as_ref()
				.map(|p| p.display().to_string())
				.unwrap_or_default(),
			draft: settings.clone(),
			saved: settings,
			saving: false,
		}
	}

	pub fn saved(&mut self, settings: Settings) {
		*self = Self::new(settings);
	}

	pub fn save_failed(&mut self) {
		self.saving = false;
	}

	fn is_dirty(&self) -> bool {
		self.draft != self.saved
	}

	fn set_adb_path(&mut self, path: Option<PathBuf>) {
		self.adb_path_text = path
			.as_ref()
			.map(|p| p.display().to_string())
			.unwrap_or_default();
		self.draft.adb_path = path;
	}

	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		default_downloads: &Path,
		commands: &mut Vec<Command>,
	) {
		egui::Grid::new("settings_grid")
			.num_columns(2)
			.spacing([16.0, 10.0])
			.show(ui, |ui| {
				ui.label("Downloads folder");
				ui.horizontal(|ui| {
					if ui
						.text_edit_singleline(&mut self.downloads_dir_text)
						.changed()
					{
						self.draft.downloads_dir = self.downloads_dir_text.trim().into();
					}
					if ui.button("Browse…").clicked()
						&& let Some(dir) = rfd::FileDialog::new()
							.set_directory(&self.draft.downloads_dir)
							.pick_folder()
					{
						self.downloads_dir_text = dir.display().to_string();
						self.draft.downloads_dir = dir;
					}
					if ui.button("Default").clicked() {
						self.downloads_dir_text = default_downloads.display().to_string();
						self.draft.downloads_dir = default_downloads.to_path_buf();
					}
				});
				ui.end_row();

				ui.label("Parallel downloads");
				ui.add(egui::Slider::new(
					&mut self.draft.max_concurrent_downloads,
					1..=MAX_CONCURRENT_DOWNLOADS,
				));
				ui.end_row();

				ui.label("ADB binary");
				ui.horizontal(|ui| {
					let edit = egui::TextEdit::singleline(&mut self.adb_path_text)
						.hint_text("adb from PATH");
					if ui.add(edit).changed() {
						let text = self.adb_path_text.trim();
						self.draft.adb_path = (!text.is_empty()).then(|| text.into());
					}
					if ui.button("Browse…").clicked()
						&& let Some(file) = rfd::FileDialog::new().pick_file()
					{
						self.set_adb_path(Some(file));
					}
					if ui.button("Use PATH").clicked() {
						self.set_adb_path(None);
					}
				});
				ui.end_row();

				ui.label("Theme");
				ui.horizontal(|ui| {
					ui.selectable_value(&mut self.draft.theme, Theme::System, "System");
					ui.selectable_value(&mut self.draft.theme, Theme::Dark, "Dark");
					ui.selectable_value(&mut self.draft.theme, Theme::Light, "Light");
				});
				ui.end_row();
			});

		ui.add_space(12.0);
		ui.horizontal(|ui| {
			let can_save =
				self.is_dirty() && !self.saving && !self.draft.downloads_dir.as_os_str().is_empty();
			if ui
				.add_enabled(can_save, egui::Button::new("Save"))
				.clicked()
			{
				self.saving = true;
				let mut settings = self.draft.clone();
				settings.normalize();
				commands.push(Command::SaveSettings(settings));
			}
			if ui
				.add_enabled(self.is_dirty() && !self.saving, egui::Button::new("Revert"))
				.clicked()
			{
				*self = SettingsView::new(self.saved.clone());
			}
			if self.saving {
				ui.spinner();
			}
		});
	}
}
