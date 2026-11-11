use quest_sideloader_core::settings::{MAX_CONCURRENT_DOWNLOADS, Theme};
use quest_sideloader_core::{Command, Settings};

use crate::app::SideloaderApp;

pub struct SettingsView {
    saved: Settings,
    draft: Settings,
    downloads_dir_text: String,
    saving: bool,
}

impl SettingsView {
    pub fn new(settings: Settings) -> Self {
        Self {
            downloads_dir_text: settings.downloads_dir.display().to_string(),
            draft: settings.clone(),
            saved: settings,
            saving: false,
        }
    }

    pub fn saved(&mut self, settings: Settings) {
        *self = Self::new(settings);
    }

    fn is_dirty(&self) -> bool {
        self.draft != self.saved
    }
}

pub fn show(app: &mut SideloaderApp, ui: &mut egui::Ui) {
    let default_downloads = app.paths().default_downloads_dir();
    let mut save = false;
    let view = &mut app.settings_view;

    egui::Grid::new("settings_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Downloads folder");
            ui.horizontal(|ui| {
                if ui
                    .text_edit_singleline(&mut view.downloads_dir_text)
                    .changed()
                {
                    view.draft.downloads_dir = view.downloads_dir_text.trim().into();
                }
                if ui.button("Browse…").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_directory(&view.draft.downloads_dir)
                        .pick_folder()
                {
                    view.downloads_dir_text = dir.display().to_string();
                    view.draft.downloads_dir = dir;
                }
                if ui.button("Default").clicked() {
                    view.downloads_dir_text = default_downloads.display().to_string();
                    view.draft.downloads_dir = default_downloads;
                }
            });
            ui.end_row();

            ui.label("Parallel downloads");
            ui.add(egui::Slider::new(
                &mut view.draft.max_concurrent_downloads,
                1..=MAX_CONCURRENT_DOWNLOADS,
            ));
            ui.end_row();

            ui.label("Theme");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut view.draft.theme, Theme::System, "System");
                ui.selectable_value(&mut view.draft.theme, Theme::Dark, "Dark");
                ui.selectable_value(&mut view.draft.theme, Theme::Light, "Light");
            });
            ui.end_row();
        });

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        let can_save =
            view.is_dirty() && !view.saving && !view.draft.downloads_dir.as_os_str().is_empty();
        save = ui
            .add_enabled(can_save, egui::Button::new("Save"))
            .clicked();
        if ui
            .add_enabled(view.is_dirty() && !view.saving, egui::Button::new("Revert"))
            .clicked()
        {
            *view = SettingsView::new(view.saved.clone());
        }
        if view.saving {
            ui.spinner();
        }
    });

    if save {
        view.saving = true;
        let mut settings = view.draft.clone();
        settings.normalize();
        app.send(Command::SaveSettings(settings));
    }
}
