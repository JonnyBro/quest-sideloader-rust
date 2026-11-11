use std::path::{Path, PathBuf};

use quest_sideloader_core::settings::{MAX_CONCURRENT_DOWNLOADS, SpeedUnit, Theme};
use quest_sideloader_core::{Command, Settings};

use super::mirrors::{self, MirrorForm};

fn path_text(path: Option<&PathBuf>) -> String {
	path.map(|p| p.display().to_string()).unwrap_or_default()
}

pub struct SettingsView {
	saved: Settings,
	draft: Settings,
	downloads_dir_text: String,
	adb_path_text: String,
	rclone_path_text: String,
	mirror_form: MirrorForm,
	saving: bool,
}

impl SettingsView {
	pub fn new(settings: Settings) -> Self {
		Self {
			downloads_dir_text: settings.downloads_dir.display().to_string(),
			adb_path_text: path_text(settings.adb_path.as_ref()),
			rclone_path_text: path_text(settings.rclone_path.as_ref()),
			mirror_form: MirrorForm::new(),
			draft: settings.clone(),
			saved: settings,
			saving: false,
		}
	}

	pub fn saved(&mut self, settings: Settings, rclone_dir: &Path) {
		mirrors::remove_unused_rclone_configs(&settings, rclone_dir);
		*self = Self::new(settings);
	}

	pub fn save_failed(&mut self) {
		self.saving = false;
	}

	pub fn contribute_ignored(&self) -> std::collections::HashSet<String> {
		self.saved.contribute_ignored.iter().cloned().collect()
	}

	pub fn active_mirror_name(&self) -> Option<&str> {
		self.saved.active_mirror().map(|m| m.name.as_str())
	}

	fn is_dirty(&self) -> bool {
		self.draft != self.saved
	}

	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		default_downloads: &Path,
		rclone_dir: &Path,
		commands: &mut Vec<Command>,
	) {
		egui::Panel::bottom("settings_actions").show(ui, |ui| {
			ui.add_space(4.0);
			self.actions(ui, commands);
			ui.add_space(2.0);
		});
		egui::CentralPanel::default().show(ui, |ui| {
			egui::ScrollArea::vertical().show(ui, |ui| {
				self.general(ui, default_downloads);
				ui.add_space(16.0);
				mirrors::show(ui, &mut self.draft, &mut self.mirror_form, rclone_dir);
			});
		});
	}

	fn general(&mut self, ui: &mut egui::Ui, default_downloads: &Path) {
		ui.heading("General");
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

				ui.label("After installing");
				ui.checkbox(
					&mut self.draft.keep_downloads,
					"Keep downloaded files for reinstalling",
				);
				ui.end_row();

				ui.label("Parallel downloads");
				ui.add(egui::Slider::new(
					&mut self.draft.max_concurrent_downloads,
					1..=MAX_CONCURRENT_DOWNLOADS,
				));
				ui.end_row();

				ui.label("Speed limit");
				ui.horizontal(|ui| {
					let limit = &mut self.draft.download_limit;
					ui.add(
						egui::DragValue::new(&mut limit.value)
							.range(0..=10_000_000)
							.speed(64),
					);
					egui::ComboBox::from_id_salt("speed_unit")
						.selected_text(limit.unit.label())
						.width(70.0)
						.show_ui(ui, |ui| {
							for unit in [SpeedUnit::KiB, SpeedUnit::KB] {
								ui.selectable_value(&mut limit.unit, unit, unit.label());
							}
						});
					ui.weak(if limit.value == 0 {
						"unlimited"
					} else {
						"for all downloads together"
					});
				});
				ui.end_row();

				ui.label("ADB binary");
				binary_field(
					ui,
					&mut self.adb_path_text,
					&mut self.draft.adb_path,
					"adb from PATH",
				);
				ui.end_row();

				ui.label("rclone binary");
				binary_field(
					ui,
					&mut self.rclone_path_text,
					&mut self.draft.rclone_path,
					"rclone from PATH (only for rclone mirrors)",
				);
				ui.end_row();

				ui.label("Theme");
				ui.horizontal(|ui| {
					ui.selectable_value(&mut self.draft.theme, Theme::System, "System");
					ui.selectable_value(&mut self.draft.theme, Theme::Dark, "Dark");
					ui.selectable_value(&mut self.draft.theme, Theme::Light, "Light");
				});
				ui.end_row();
			});
	}

	fn actions(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
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
			} else if self.is_dirty() {
				ui.weak("Unsaved changes");
			}
		});
	}
}

fn binary_field(ui: &mut egui::Ui, text: &mut String, value: &mut Option<PathBuf>, hint: &str) {
	ui.horizontal(|ui| {
		if ui
			.add(
				egui::TextEdit::singleline(text)
					.hint_text(hint)
					.desired_width(320.0),
			)
			.changed()
		{
			let trimmed = text.trim();
			*value = (!trimmed.is_empty()).then(|| trimmed.into());
		}
		if ui.button("Browse…").clicked()
			&& let Some(file) = rfd::FileDialog::new().pick_file()
		{
			*text = file.display().to_string();
			*value = Some(file);
		}
		if ui.button("Use PATH").clicked() {
			text.clear();
			*value = None;
		}
	});
}
