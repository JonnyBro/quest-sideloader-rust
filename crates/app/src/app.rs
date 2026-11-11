use std::sync::Arc;

use quest_sideloader_core::settings::Theme;
use quest_sideloader_core::{AppPaths, Engine, Event, Settings};

use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
	Devices,
	Library,
	Downloads,
	Settings,
}

impl Tab {
	const ALL: [Tab; 4] = [Tab::Devices, Tab::Library, Tab::Downloads, Tab::Settings];

	fn label(self) -> &'static str {
		match self {
			Tab::Devices => "Devices",
			Tab::Library => "Library",
			Tab::Downloads => "Downloads",
			Tab::Settings => "Settings",
		}
	}
}

pub enum Status {
	Info(String),
	Error(String),
}

pub struct SideloaderApp {
	engine: Engine,
	paths: AppPaths,
	tab: Tab,
	status: Option<Status>,
	devices_view: ui::devices::DevicesView,
	settings_view: ui::settings::SettingsView,
}

impl SideloaderApp {
	pub fn new(
		cc: &eframe::CreationContext<'_>,
		paths: AppPaths,
		settings: Settings,
		load_error: Option<String>,
	) -> Result<Self, quest_sideloader_core::Error> {
		let ctx = cc.egui_ctx.clone();
		let engine = Engine::start(
			paths.clone(),
			settings.clone(),
			Arc::new(move || ctx.request_repaint()),
		)?;
		apply_theme(&cc.egui_ctx, settings.theme);

		Ok(Self {
			engine,
			paths,
			tab: Tab::Devices,
			status: load_error.map(|e| {
				Status::Error(format!(
					"{e}. Using defaults; saving will overwrite the file."
				))
			}),
			devices_view: ui::devices::DevicesView::new(),
			settings_view: ui::settings::SettingsView::new(settings),
		})
	}

	fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
		match event {
			Event::SettingsSaved(settings) => {
				apply_theme(ctx, settings.theme);
				self.settings_view.saved(settings);
				self.status = Some(Status::Info("Settings saved".into()));
			}
			Event::SettingsSaveFailed(message) => {
				self.settings_view.save_failed();
				self.status = Some(Status::Error(message));
			}
			Event::AdbStatus(status) => self.devices_view.on_adb_status(status),
			Event::DevicesChanged(devices) => self.devices_view.on_devices_changed(devices),
			Event::DeviceInfo { serial, info } => self.devices_view.on_info(serial, info),
			Event::Notice(message) => {
				tracing::info!("{message}");
				self.status = Some(Status::Info(message));
			}
			Event::Error(message) => {
				tracing::error!("{message}");
				self.status = Some(Status::Error(message));
			}
		}
	}

	fn status_bar(&mut self, ui: &mut egui::Ui) {
		let Some(status) = &self.status else {
			ui.weak("Ready");
			return;
		};
		let mut dismiss = false;
		ui.horizontal(|ui| {
			match status {
				Status::Info(text) => ui.label(text),
				Status::Error(text) => ui.colored_label(ui.visuals().error_fg_color, text),
			};
			dismiss = ui.small_button("✕").clicked();
		});
		if dismiss {
			self.status = None;
		}
	}
}

impl eframe::App for SideloaderApp {
	fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
		let events: Vec<Event> = self.engine.poll_events().collect();
		for event in events {
			self.handle_event(ctx, event);
		}
	}

	fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
		egui::Panel::top("tabs").show(ui, |ui| {
			ui.horizontal(|ui| {
				for tab in Tab::ALL {
					ui.selectable_value(&mut self.tab, tab, tab.label());
				}
			});
		});

		egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

		let mut commands = Vec::new();
		egui::CentralPanel::default().show(ui, |ui| match self.tab {
			Tab::Devices => self.devices_view.show(ui, &mut commands),
			Tab::Library | Tab::Downloads => {
				ui.centered_and_justified(|ui| ui.weak("Not implemented yet"));
			}
			Tab::Settings => {
				let default_downloads = self.paths.default_downloads_dir();
				self.settings_view
					.show(ui, &default_downloads, &mut commands);
			}
		});
		for command in commands {
			self.engine.send(command);
		}
	}
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
	ctx.set_theme(match theme {
		Theme::System => egui::ThemePreference::System,
		Theme::Dark => egui::ThemePreference::Dark,
		Theme::Light => egui::ThemePreference::Light,
	});
}
