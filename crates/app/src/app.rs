use rust_i18n::t;
use std::sync::Arc;

use quest_sideloader_core::settings::{Language, Theme};
use quest_sideloader_core::{AppPaths, Command, Engine, Event, Settings};

use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
	Devices,
	Headset,
	Library,
	Downloads,
	Tasks,
	Console,
	Settings,
}

impl Tab {
	const ALL: [Tab; 7] = [
		Tab::Devices,
		Tab::Headset,
		Tab::Library,
		Tab::Downloads,
		Tab::Tasks,
		Tab::Console,
		Tab::Settings,
	];

	fn label(self) -> std::borrow::Cow<'static, str> {
		match self {
			Tab::Devices => t!("tab.devices"),
			Tab::Headset => t!("tab.headset"),
			Tab::Library => t!("tab.library"),
			Tab::Downloads => t!("tab.downloads"),
			Tab::Tasks => t!("tab.tasks"),
			Tab::Console => t!("tab.console"),
			Tab::Settings => t!("tab.settings"),
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
	tasks_view: ui::tasks::TasksView,
	console_view: ui::console::ConsoleView,
	downloads_view: ui::downloads::DownloadsView,
	headset_view: ui::headset::HeadsetView,
	logs_view: ui::logs::LogsView,
	adb_setup: ui::tool_setup::AdbSetup,
	rclone_setup: ui::tool_setup::RcloneSetup,
	review_dialog: ui::review::ReviewDialog,
	library_view: ui::library::LibraryView,
	settings_view: ui::settings::SettingsView,
	welcome: ui::welcome::Welcome,
	/// Last title sent to the window, so it is only sent on changes.
	title: String,
}

pub fn base_title(portable: bool) -> &'static str {
	if portable {
		"Quest Sideloader (portable)"
	} else {
		"Quest Sideloader"
	}
}

impl SideloaderApp {
	pub fn new(
		cc: &eframe::CreationContext<'_>,
		paths: AppPaths,
		settings: Settings,
		load_error: Option<String>,
		first_run: bool,
		log_buffer: crate::logbuf::LogBuffer,
	) -> Result<Self, quest_sideloader_core::Error> {
		let ctx = cc.egui_ctx.clone();
		let engine = Engine::start(
			paths.clone(),
			settings.clone(),
			Arc::new(move || ctx.request_repaint()),
		)?;
		apply_theme(&cc.egui_ctx, settings.theme);
		apply_language(settings.language);
		egui_extras::install_image_loaders(&cc.egui_ctx);

		let logs_dir = paths.logs_dir();
		let mut library_view = ui::library::LibraryView::new();
		if let Some(prefs) = cc.storage.and_then(|s| eframe::get_value(s, LIBRARY_KEY)) {
			library_view.restore(prefs);
		}
		let mut devices_view = ui::devices::DevicesView::new();
		if let Some(address) = cc
			.storage
			.and_then(|s| eframe::get_value::<String>(s, WIRELESS_KEY))
		{
			devices_view.restore_wireless(address);
		}
		Ok(Self {
			title: base_title(paths.portable_root().is_some()).to_owned(),
			engine,
			paths,
			tab: Tab::Devices,
			status: load_error
				.map(|e| Status::Error(t!("app.settings_load_error", error = e).into_owned())),
			devices_view,
			tasks_view: ui::tasks::TasksView::new(),
			console_view: ui::console::ConsoleView::new(),
			downloads_view: ui::downloads::DownloadsView::new(),
			headset_view: ui::headset::HeadsetView::new(),
			logs_view: ui::logs::LogsView::new(log_buffer, logs_dir),
			adb_setup: ui::tool_setup::AdbSetup::default(),
			rclone_setup: ui::tool_setup::RcloneSetup::default(),
			review_dialog: ui::review::ReviewDialog::new(),
			library_view,
			welcome: ui::welcome::Welcome::new(first_run, &settings),
			settings_view: ui::settings::SettingsView::new(settings),
		})
	}

	fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
		match event {
			Event::SettingsSaved(settings) => {
				apply_theme(ctx, settings.theme);
				apply_language(settings.language);
				self.downloads_view.invalidate();
				self.settings_view.saved(settings, &self.paths.rclone_dir());
				self.status = Some(Status::Info(t!("app.settings_saved").into_owned()));
			}
			Event::SettingsSaveFailed(message) => {
				self.settings_view.save_failed();
				self.status = Some(Status::Error(message));
			}
			Event::AdbStatus(status) => {
				if matches!(status, quest_sideloader_core::AdbStatus::Missing(_)) {
					self.adb_setup.on_missing();
				}
				if let Some(address) = self.devices_view.on_adb_status(status) {
					self.engine.send(Command::Reconnect(address));
				}
			}
			Event::DevicesChanged(devices) => self.devices_view.on_devices_changed(devices),
			Event::DeviceInfo { serial, info } => self.devices_view.on_info(serial, info),
			Event::CatalogLoaded(catalog) => self.library_view.on_catalog(catalog),
			Event::CatalogStatus(status) => self.library_view.on_status(status),
			Event::HeadsetDetails { serial, details } => {
				self.headset_view.on_details(serial, details)
			}
			Event::Backups(backups) => self.headset_view.on_backups(backups),
			Event::BackupsChanged => self.engine.send(Command::ListBackups),
			Event::Downloads(releases) => self.downloads_view.on_list(releases),
			Event::RcloneMissing => self.rclone_setup.on_missing(),
			Event::TaskUpdated(snapshot) => {
				if snapshot.state == quest_sideloader_core::TaskState::Done {
					self.headset_view.invalidate();
					// Installs can remove the release folder afterwards.
					self.downloads_view.invalidate();
				}
				if snapshot.state.is_finished() {
					self.review_dialog.forget(snapshot.id);
				}
				let in_background = !ctx.input(|i| i.viewport().focused.unwrap_or(true));
				let notify = self.settings_view.notifications() && in_background;
				let summary = match &snapshot.state {
					quest_sideloader_core::TaskState::Done => {
						Some((snapshot.title.clone(), snapshot.stage.clone()))
					}
					quest_sideloader_core::TaskState::Failed(e) => Some((
						t!("app.task_failed", title = snapshot.title).into_owned(),
						e.chars().take(200).collect(),
					)),
					_ => None,
				};
				let finished = self.tasks_view.on_update(snapshot);
				if let Some((title, body)) = summary.filter(|_| finished && notify) {
					crate::notify::send(title, body);
				}
			}
			Event::ScriptReview {
				task,
				title,
				analysis,
			} => self.review_dialog.push(task, title, analysis),
			Event::Shell { id, output } => self.console_view.on_output(id, output),
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

	fn handle_file_drop(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
		let (hovering, dropped) = ctx.input(|i| {
			(
				!i.raw.hovered_files.is_empty(),
				i.raw
					.dropped_files
					.iter()
					.map(|f| f.path().to_path_buf())
					.collect::<Vec<_>>(),
			)
		});
		let target = self
			.devices_view
			.install_target()
			.map(|d| (d.serial.clone(), d.display_name()));

		if hovering {
			let text = match &target {
				Some((_, name)) => t!("app.drop_to_install", name = name).into_owned(),
				None => t!("app.drop_no_headset").into_owned(),
			};
			drop_overlay(ctx, &text);
		}
		if dropped.is_empty() {
			return;
		}
		let Some((serial, _)) = target else {
			self.status = Some(Status::Error(t!("app.no_headset").into_owned()));
			return;
		};
		for path in dropped {
			commands.push(Command::Install {
				serial: serial.clone(),
				path,
			});
		}
	}

	fn update_title(&mut self, ctx: &egui::Context) {
		let base = base_title(self.paths.portable_root().is_some());
		let title = match self.tasks_view.title_prefix() {
			Some(prefix) => format!("{prefix} - {base}"),
			None => base.to_owned(),
		};
		if title != self.title {
			ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
			self.title = title;
		}
	}

	fn status_bar(&mut self, ui: &mut egui::Ui) {
		let mut dismiss = false;
		ui.horizontal(|ui| {
			match &self.status {
				None => {
					ui.weak(t!("app.ready"));
				}
				Some(status) => {
					match status {
						Status::Info(text) => ui.label(text),
						Status::Error(text) => ui.colored_label(ui.visuals().error_fg_color, text),
					};
					dismiss = ui
						.small_button("✕")
						.on_hover_text(t!("common.close"))
						.clicked();
				}
			}
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				self.logs_view.button(ui);
			});
		});
		if dismiss {
			self.status = None;
		}
	}
}

const LIBRARY_KEY: &str = "library";
const WIRELESS_KEY: &str = "wireless_address";

impl eframe::App for SideloaderApp {
	fn save(&mut self, storage: &mut dyn eframe::Storage) {
		eframe::set_value(storage, LIBRARY_KEY, &self.library_view.prefs());
		if let Some(address) = self.devices_view.last_wireless() {
			eframe::set_value(storage, WIRELESS_KEY, &address);
		}
	}

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
		if self.tab != Tab::Tasks {
			let mut open = false;
			egui::Panel::bottom("tasks_summary")
				.resizable(false)
				.show(ui, |ui| open = self.tasks_view.summary(ui));
			if open {
				self.tab = Tab::Tasks;
			}
		}
		egui::CentralPanel::default().show(ui, |ui| match self.tab {
			Tab::Devices => {
				if self.devices_view.show(ui, &mut commands) {
					self.adb_setup.reopen();
				}
			}
			Tab::Headset => {
				let target = self
					.devices_view
					.install_target()
					.map(|d| ui::headset::Target {
						serial: &d.serial,
						name: d.display_name(),
						info: self.devices_view.info(&d.serial),
						names: self.library_view.names(),
					});
				self.headset_view.show(ui, target, &mut commands);
			}
			Tab::Library => {
				let installed = self.devices_view.installed_apps();
				let ignored = self.settings_view.contribute_ignored();
				self.library_view.show(
					ui,
					self.settings_view.active_mirror_name(),
					installed.as_ref(),
					&ignored,
					&mut commands,
				);
			}
			Tab::Downloads => {
				let target = self
					.devices_view
					.install_target()
					.map(|d| (d.serial.clone(), d.display_name()));
				let target = target.as_ref().map(|(s, n)| (s.as_str(), n.as_str()));
				self.downloads_view.show(
					ui,
					self.settings_view.downloads_dir(),
					target,
					&mut commands,
				);
			}
			Tab::Tasks => {
				let target = self
					.devices_view
					.install_target()
					.map(|d| (d.serial.clone(), d.display_name()));
				let target = target.as_ref().map(|(s, n)| (s.as_str(), n.as_str()));
				self.tasks_view.show(ui, target, &mut commands);
			}
			Tab::Console => {
				let target = self
					.devices_view
					.install_target()
					.map(|d| (d.serial.clone(), d.display_name()));
				let target = target.as_ref().map(|(s, n)| (s.as_str(), n.as_str()));
				self.console_view.show(ui, target, &mut commands);
			}
			Tab::Settings => {
				let default_downloads = self.paths.default_downloads_dir();
				self.settings_view.show(
					ui,
					&default_downloads,
					&self.paths.rclone_dir(),
					&mut commands,
				);
			}
		});
		let ctx = ui.ctx().clone();
		self.update_title(&ctx);
		self.review_dialog.show(&ctx, &mut commands);
		self.logs_view.window(&ctx);
		let default_downloads = self.paths.default_downloads_dir();
		if self.welcome.show(&ctx, &default_downloads, &mut commands)
			== Some(ui::welcome::Outcome::OpenMirrors)
		{
			self.tab = Tab::Settings;
		}
		// Waits for the wizard, so two dialogs don't open over each other.
		if !self.welcome.is_open() {
			self.adb_setup.show(&ctx, &mut commands);
			self.rclone_setup.show(&ctx, &mut commands);
		}
		self.handle_file_drop(&ctx, &mut commands);

		for command in commands {
			self.engine.send(command);
		}
	}
}

fn drop_overlay(ctx: &egui::Context, text: &str) {
	let painter = ctx.layer_painter(egui::LayerId::new(
		egui::Order::Foreground,
		egui::Id::unique("drop_overlay"),
	));
	let rect = ctx.content_rect();
	painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(170));
	painter.text(
		rect.center(),
		egui::Align2::CENTER_CENTER,
		text,
		egui::FontId::proportional(24.0),
		egui::Color32::WHITE,
	);
}

pub(crate) fn apply_theme(ctx: &egui::Context, theme: Theme) {
	ctx.set_theme(match theme {
		Theme::System => egui::ThemePreference::System,
		Theme::Dark => egui::ThemePreference::Dark,
		Theme::Light => egui::ThemePreference::Light,
	});
}

pub(crate) fn apply_language(language: Language) {
	let russian = match language {
		Language::Russian => true,
		Language::English => false,
		Language::System => sys_locale::get_locale().is_some_and(|l| l.starts_with("ru")),
	};
	// Shared by the core crate too: rust-i18n keeps one global locale.
	rust_i18n::set_locale(if russian { "ru" } else { "en" });
}
