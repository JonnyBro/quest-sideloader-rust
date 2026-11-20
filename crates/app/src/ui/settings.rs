use rust_i18n::t;
use std::path::{Path, PathBuf};

use quest_sideloader_core::settings::{Language, MAX_CONCURRENT_DOWNLOADS, SpeedUnit, Theme};
use quest_sideloader_core::{Command, Settings};

use super::mirrors::{self, MirrorForm};
use crate::theme::{Accent, Appearance, Corners, Density};

fn path_text(path: Option<&PathBuf>) -> String {
	path.map(|p| p.display().to_string()).unwrap_or_default()
}

/// The parts of Settings kept outside the settings file: they apply at once.
pub struct Prefs<'a> {
	pub keys: &'a mut super::keys::KeysView,
	pub advanced: &'a mut bool,
	pub appearance: &'a mut Appearance,
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

	pub fn downloads_dir(&self) -> &Path {
		&self.saved.downloads_dir
	}

	pub fn notifications(&self) -> bool {
		self.saved.notifications
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
		prefs: Prefs<'_>,
	) {
		egui::Panel::bottom("settings_actions").show(ui, |ui| {
			ui.add_space(4.0);
			self.actions(ui, commands);
			ui.add_space(2.0);
		});
		egui::CentralPanel::default().show(ui, |ui| {
			egui::ScrollArea::vertical().show(ui, |ui| {
				let Prefs {
					keys,
					advanced,
					appearance,
				} = prefs;
				self.general(ui, default_downloads, advanced);
				ui.add_space(16.0);
				self.appearance(ui, appearance);
				ui.add_space(16.0);
				mirrors::show(ui, &mut self.draft, &mut self.mirror_form, rclone_dir);
				ui.add_space(16.0);
				keys.show(ui);
			});
		});
	}

	fn general(&mut self, ui: &mut egui::Ui, default_downloads: &Path, advanced: &mut bool) {
		ui.heading(t!("set.general"));
		egui::Grid::new("settings_grid")
			.num_columns(2)
			.spacing([16.0, 10.0])
			.show(ui, |ui| {
				ui.label(t!("set.downloads_folder"));
				ui.horizontal(|ui| {
					if ui
						.text_edit_singleline(&mut self.downloads_dir_text)
						.changed()
					{
						self.draft.downloads_dir = self.downloads_dir_text.trim().into();
					}
					if ui.button(t!("common.browse")).clicked()
						&& let Some(dir) = rfd::FileDialog::new()
							.set_directory(&self.draft.downloads_dir)
							.pick_folder()
					{
						self.downloads_dir_text = dir.display().to_string();
						self.draft.downloads_dir = dir;
					}
					if ui.button(t!("set.default")).clicked() {
						self.downloads_dir_text = default_downloads.display().to_string();
						self.draft.downloads_dir = default_downloads.to_path_buf();
					}
				});
				ui.end_row();

				// Applied at once and kept with the window state, like the scale.
				ui.label(t!("set.mode"));
				ui.checkbox(advanced, t!("set.advanced"))
					.on_hover_text(t!("set.advanced_hint"));
				ui.end_row();

				ui.label(t!("set.after_installing"));
				ui.checkbox(&mut self.draft.keep_downloads, t!("set.keep_downloads"));
				ui.end_row();

				ui.label(t!("set.notifications"));
				ui.checkbox(&mut self.draft.notifications, t!("set.notify"));
				ui.end_row();

				ui.label(t!("set.parallel"));
				ui.add(egui::Slider::new(
					&mut self.draft.max_concurrent_downloads,
					1..=MAX_CONCURRENT_DOWNLOADS,
				));
				ui.end_row();

				ui.label(t!("set.speed_limit"));
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
						t!("set.unlimited")
					} else {
						t!("set.all_downloads")
					});
				});
				ui.end_row();

				ui.label(t!("set.adb_binary"));
				binary_field(
					ui,
					&mut self.adb_path_text,
					&mut self.draft.adb_path,
					&t!("set.adb_hint"),
				);
				ui.end_row();

				ui.label(t!("set.rclone_binary"));
				binary_field(
					ui,
					&mut self.rclone_path_text,
					&mut self.draft.rclone_path,
					&t!("set.rclone_hint"),
				);
				ui.end_row();

				ui.label(t!("set.language"));
				ui.horizontal(|ui| {
					let language = &mut self.draft.language;
					ui.selectable_value(language, Language::System, t!("set.language_system"));
					// Language names stay in their own language.
					ui.selectable_value(language, Language::English, "English");
					ui.selectable_value(language, Language::Russian, "Русский");
				});
				ui.end_row();
			});
	}

	fn appearance(&mut self, ui: &mut egui::Ui, appearance: &mut Appearance) {
		ui.heading(t!("set.appearance"));
		ui.weak(t!("set.appearance_hint"));
		ui.add_space(4.0);
		egui::Grid::new("appearance_grid")
			.num_columns(2)
			.spacing([16.0, 10.0])
			.show(ui, |ui| {
				ui.label(t!("set.theme"));
				ui.horizontal(|ui| {
					let theme = &mut self.draft.theme;
					ui.selectable_value(theme, Theme::System, t!("set.theme_system"));
					ui.selectable_value(theme, Theme::Dark, t!("set.theme_dark"));
					ui.selectable_value(theme, Theme::Light, t!("set.theme_light"));
				});
				// Shown right away; saved with the other settings.
				if self.draft.theme != self.saved.theme {
					crate::app::apply_theme(ui.ctx(), self.draft.theme);
				}
				ui.end_row();

				ui.label(t!("set.accent"));
				ui.horizontal(|ui| {
					for accent in Accent::PRESETS {
						let selected = appearance.accent == accent;
						let (rect, response) =
							ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
						ui.painter()
							.circle_filled(rect.center(), 10.0, accent.color());
						if selected {
							ui.painter().circle_stroke(
								rect.center(),
								10.0,
								egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
							);
						}
						if response.on_hover_text(accent_name(accent)).clicked() {
							appearance.accent = accent;
						}
					}
					ui.add_space(6.0);
					let previous = appearance.accent_color();
					if ui
						.selectable_label(
							appearance.accent == Accent::Custom,
							t!("set.accent_custom"),
						)
						.clicked()
						&& appearance.accent != Accent::Custom
					{
						// Start the picker from the color in use, not from black.
						if appearance.custom == [0; 3] {
							appearance.custom = [previous.r(), previous.g(), previous.b()];
						}
						appearance.accent = Accent::Custom;
					}
					if appearance.accent == Accent::Custom {
						ui.color_edit_button_srgb(&mut appearance.custom);
					}
				});
				ui.end_row();

				ui.label(t!("set.corners"));
				ui.horizontal(|ui| {
					let corners = &mut appearance.corners;
					ui.selectable_value(corners, Corners::Square, t!("set.corners_square"));
					ui.selectable_value(corners, Corners::Rounded, t!("set.corners_rounded"));
					ui.selectable_value(corners, Corners::Round, t!("set.corners_round"));
				});
				ui.end_row();

				ui.label(t!("set.density"));
				ui.horizontal(|ui| {
					let density = &mut appearance.density;
					ui.selectable_value(density, Density::Compact, t!("set.density_compact"));
					ui.selectable_value(density, Density::Normal, t!("set.density_normal"));
					ui.selectable_value(density, Density::Spacious, t!("set.density_spacious"));
				});
				ui.end_row();

				// Applied at once and kept by egui with the window state, like
				// its own Ctrl+Plus/Minus/0 shortcuts, so it isn't in the draft.
				ui.label(t!("set.scale"));
				ui.horizontal(|ui| {
					let zoom = ui.ctx().zoom_factor();
					let step = |z: f32| ((z * 10.0).round() / 10.0).clamp(0.5, 3.0);
					if ui.button("-").clicked() {
						ui.ctx().set_zoom_factor(step(zoom - 0.1));
					}
					ui.label(format!("{:.0}%", zoom * 100.0));
					if ui.button("+").clicked() {
						ui.ctx().set_zoom_factor(step(zoom + 0.1));
					}
					if ui
						.add_enabled(zoom != 1.0, egui::Button::new(t!("set.scale_reset")))
						.clicked()
					{
						ui.ctx().set_zoom_factor(1.0);
					}
					ui.weak(t!("set.scale_hint"));
				});
				ui.end_row();
			});
	}

	fn actions(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		ui.horizontal(|ui| {
			let can_save =
				self.is_dirty() && !self.saving && !self.draft.downloads_dir.as_os_str().is_empty();
			if ui
				.add_enabled(can_save, egui::Button::new(t!("set.save")))
				.clicked()
			{
				self.saving = true;
				let mut settings = self.draft.clone();
				settings.normalize();
				commands.push(Command::SaveSettings(settings));
			}
			if ui
				.add_enabled(
					self.is_dirty() && !self.saving,
					egui::Button::new(t!("set.revert")),
				)
				.clicked()
			{
				// The theme shows as soon as it's picked, so undo that too.
				crate::app::apply_theme(ui.ctx(), self.saved.theme);
				*self = SettingsView::new(self.saved.clone());
			}
			if self.saving {
				ui.spinner();
			} else if self.is_dirty() {
				ui.weak(t!("set.unsaved"));
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
		if ui.button(t!("common.browse")).clicked()
			&& let Some(file) = rfd::FileDialog::new().pick_file()
		{
			*text = file.display().to_string();
			*value = Some(file);
		}
		if ui.button(t!("set.use_path")).clicked() {
			text.clear();
			*value = None;
		}
	});
}

fn accent_name(accent: Accent) -> std::borrow::Cow<'static, str> {
	match accent {
		Accent::Violet => t!("set.accent_violet"),
		Accent::Blue => t!("set.accent_blue"),
		Accent::Teal => t!("set.accent_teal"),
		Accent::Green => t!("set.accent_green"),
		Accent::Orange => t!("set.accent_orange"),
		Accent::Pink => t!("set.accent_pink"),
		Accent::Custom => t!("set.accent_custom"),
	}
}
