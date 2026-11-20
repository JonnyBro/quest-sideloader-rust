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
	Rename,
	Inspector,
	Tasks,
	Console,
	Settings,
}

impl Tab {
	const ALL: [Tab; 9] = [
		Tab::Headset,
		Tab::Library,
		Tab::Downloads,
		Tab::Tasks,
		Tab::Devices,
		Tab::Rename,
		Tab::Inspector,
		Tab::Console,
		Tab::Settings,
	];

	/// Tabs for people who know adb; hidden until advanced mode is on.
	fn advanced(self) -> bool {
		matches!(
			self,
			Tab::Devices | Tab::Rename | Tab::Inspector | Tab::Console
		)
	}

	fn icon(self) -> crate::icons::Icon {
		use crate::icons::Icon;
		match self {
			Tab::Headset => Icon::Headset,
			Tab::Library => Icon::Library,
			Tab::Downloads => Icon::Downloads,
			Tab::Tasks => Icon::Tasks,
			Tab::Settings => Icon::Settings,
			Tab::Devices => Icon::Devices,
			Tab::Rename => Icon::Rename,
			Tab::Inspector => Icon::Inspector,
			Tab::Console => Icon::Console,
		}
	}

	fn label(self) -> std::borrow::Cow<'static, str> {
		match self {
			Tab::Devices => t!("tab.devices"),
			Tab::Headset => t!("tab.headset"),
			Tab::Library => t!("tab.library"),
			Tab::Downloads => t!("tab.downloads"),
			Tab::Rename => t!("tab.rename"),
			Tab::Inspector => t!("tab.inspector"),
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

/// How long a notice stays in the corner.
const TOAST_TIME: std::time::Duration = std::time::Duration::from_secs(5);

struct Toast {
	status: Status,
	shown: std::time::Instant,
}

impl Toast {
	fn new(status: Status) -> Self {
		Self {
			status,
			shown: std::time::Instant::now(),
		}
	}
}

pub struct SideloaderApp {
	engine: Engine,
	paths: AppPaths,
	tab: Tab,
	/// Messages in the corner: notices fade, errors stay until closed.
	toasts: Vec<Toast>,
	devices_view: ui::devices::DevicesView,
	tasks_view: ui::tasks::TasksView,
	console_view: ui::console::ConsoleView,
	rename_view: ui::rename::RenameView,
	inspector_view: ui::inspector::InspectorView,
	keys_view: ui::keys::KeysView,
	/// Shows the tabs for advanced users.
	advanced: bool,
	appearance: crate::theme::Appearance,
	checkup: ui::checkup::Checkup,
	/// When free space in the downloads folder was last read.
	pc_free_checked: Option<std::time::Instant>,
	/// Packages of renamed copies made here, never offered for upload.
	copies: std::collections::HashSet<String>,
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
		crate::theme::install_font(&cc.egui_ctx);
		let appearance: crate::theme::Appearance = cc
			.storage
			.and_then(|s| eframe::get_value(s, APPEARANCE_KEY))
			.unwrap_or_default();
		crate::theme::apply(&cc.egui_ctx, &appearance);
		apply_language(settings.language);
		egui_extras::install_image_loaders(&cc.egui_ctx);

		let logs_dir = paths.logs_dir();
		let mut library_view = ui::library::LibraryView::new();
		if let Some(prefs) = cc.storage.and_then(|s| eframe::get_value(s, LIBRARY_KEY)) {
			library_view.restore(prefs);
		}
		let keys_view = ui::keys::KeysView::new(paths.signing_dir());
		let mut rename_view = ui::rename::RenameView::new();
		if let Some(path) = cc
			.storage
			.and_then(|s| eframe::get_value::<std::path::PathBuf>(s, LOADER_KEY))
		{
			rename_view.restore_loader(path);
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
			tab: Tab::Headset,
			toasts: load_error
				.map(|e| {
					Toast::new(Status::Error(
						t!("app.settings_load_error", error = e).into_owned(),
					))
				})
				.into_iter()
				.collect(),
			devices_view,
			tasks_view: ui::tasks::TasksView::new(),
			console_view: ui::console::ConsoleView::new(),
			rename_view,
			inspector_view: ui::inspector::InspectorView::new(),
			copies: keys_view.packages().into_iter().collect(),
			keys_view,
			pc_free_checked: None,
			appearance,
			checkup: ui::checkup::Checkup::default(),
			advanced: cc
				.storage
				.and_then(|s| eframe::get_value(s, ADVANCED_KEY))
				.unwrap_or(false),
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
				self.notify(Status::Info(t!("app.settings_saved").into_owned()));
			}
			Event::SettingsSaveFailed(message) => {
				self.settings_view.save_failed();
				self.notify(Status::Error(message));
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
					if snapshot.kind == quest_sideloader_core::TaskKind::Rename {
						self.keys_view.invalidate();
						self.copies = self.keys_view.packages().into_iter().collect();
					}
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
			Event::RenameAnalysis { id, result } => self.rename_view.on_analysis(id, result),
			Event::Inspection { id, result } => self.inspector_view.on_result(id, result),
			Event::Notice(message) => {
				tracing::info!("{message}");
				self.notify(Status::Info(message));
			}
			Event::Error(message) => {
				tracing::error!("{message}");
				self.notify(Status::Error(message));
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
			self.notify(Status::Error(t!("app.no_headset").into_owned()));
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

	/// Everything that has to be right to install games, for "Check all".
	fn checks(&self) -> Vec<ui::checkup::Check> {
		use quest_sideloader_core::AdbStatus;
		use ui::checkup::{Check, Level};
		use ui::devices::Readiness;
		let mut checks = Vec::new();
		checks.push(match self.devices_view.adb_status() {
			AdbStatus::Running { .. } => Check::new(Level::Ok, t!("chk.adb_ok"), ""),
			AdbStatus::Starting => Check::new(Level::Warning, t!("chk.adb_starting"), ""),
			AdbStatus::Missing(e) | AdbStatus::Unavailable(e) => {
				Check::new(Level::Problem, t!("chk.adb_missing"), e.clone())
			}
		});
		checks.push(match self.devices_view.readiness() {
			Readiness::Ready => {
				let name = self
					.devices_view
					.install_target()
					.map(|d| d.display_name())
					.unwrap_or_default();
				Check::new(Level::Ok, t!("chk.headset_ok", name = name), "")
			}
			Readiness::Waiting(state) => Check::new(
				Level::Warning,
				t!("chk.headset_waiting"),
				ui::devices::state_hint(&state).unwrap_or_default(),
			),
			Readiness::NoHeadset | Readiness::NoAdb => Check::new(
				Level::Problem,
				t!("chk.headset_none"),
				t!("chk.headset_none_detail"),
			),
		});
		let info = self
			.devices_view
			.install_target()
			.and_then(|d| self.devices_view.info(&d.serial));
		if let Some(storage) = info.and_then(|i| i.storage) {
			const GB: u64 = 1024 * 1024 * 1024;
			let level = match storage.free_bytes {
				f if f < 2 * GB => Level::Problem,
				f if f < 10 * GB => Level::Warning,
				_ => Level::Ok,
			};
			checks.push(Check::new(
				level,
				t!(
					"chk.headset_space",
					free = ui::format_bytes(storage.free_bytes)
				),
				if level == Level::Ok {
					"".into()
				} else {
					t!("chk.space_detail")
				},
			));
		}
		if let Some(battery) = info.and_then(|i| i.battery) {
			let level = if battery.level < 15 && !battery.charging {
				Level::Warning
			} else {
				Level::Ok
			};
			checks.push(Check::new(
				level,
				t!("chk.battery", level = battery.level),
				"",
			));
		}
		match quest_sideloader_core::downloaded::free_space(self.settings_view.downloads_dir()) {
			Some(free) => {
				const GB: u64 = 1024 * 1024 * 1024;
				let level = match free {
					f if f < 3 * GB => Level::Problem,
					f if f < 10 * GB => Level::Warning,
					_ => Level::Ok,
				};
				checks.push(Check::new(
					level,
					t!("chk.pc_space", free = ui::format_bytes(free)),
					if level == Level::Ok {
						"".into()
					} else {
						t!("chk.pc_space_detail")
					},
				));
			}
			None => checks.push(Check::new(
				Level::Warning,
				t!("chk.pc_space_unknown"),
				self.settings_view.downloads_dir().display().to_string(),
			)),
		}
		let (games, status) = self.library_view.catalog_state();
		checks.push(
			match (self.settings_view.active_mirror_name(), status, games) {
				(None, _, _) => Check::new(
					Level::Skipped,
					t!("chk.mirror_none"),
					t!("chk.mirror_none_detail"),
				),
				(Some(_), quest_sideloader_core::CatalogStatus::Failed(e), _) => {
					Check::new(Level::Problem, t!("chk.mirror_failed"), e.clone())
				}
				(Some(_), quest_sideloader_core::CatalogStatus::Syncing { .. }, _) => {
					Check::new(Level::Warning, t!("chk.mirror_syncing"), "")
				}
				(Some(name), _, Some(n)) => {
					Check::new(Level::Ok, t!("chk.mirror_ok", name = name, n = n), "")
				}
				(Some(name), _, None) => {
					Check::new(Level::Warning, t!("chk.mirror_empty", name = name), "")
				}
			},
		);
		checks
	}

	fn notify(&mut self, status: Status) {
		// Only the latest few; a burst of errors shouldn't cover the window.
		if self.toasts.len() >= 4 {
			self.toasts.remove(0);
		}
		self.toasts.push(Toast::new(status));
	}

	/// Navigation on top; running tasks, the headset and the logs at the bottom.
	fn sidebar(&mut self, ui: &mut egui::Ui) {
		ui.add_space(6.0);
		ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
			for tab in Tab::ALL.into_iter().filter(|t| !t.advanced()) {
				if crate::icons::nav_item(ui, tab.icon(), &tab.label(), self.tab == tab, 18.0) {
					self.tab = tab;
				}
			}
			if self.advanced {
				ui.add_space(8.0);
				ui.weak(egui::RichText::new(t!("app.advanced_tabs")).small());
				for tab in Tab::ALL.into_iter().filter(|t| t.advanced()) {
					if crate::icons::nav_item(ui, tab.icon(), &tab.label(), self.tab == tab, 15.0) {
						self.tab = tab;
					}
				}
			}
		});
		ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
			ui.add_space(6.0);
			self.logs_view.button(ui);
			ui.add_space(4.0);
			let (open, check) = self.devices_view.sidebar_card(ui);
			if open {
				self.tab = Tab::Headset;
			}
			if check {
				self.checkup.open();
			}
			ui.add_space(8.0);
			ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
				if self.tasks_view.sidebar(ui) {
					self.tab = Tab::Tasks;
				}
			});
		});
	}

	fn show_toasts(&mut self, ctx: &egui::Context) {
		self.toasts
			.retain(|t| matches!(t.status, Status::Error(_)) || t.shown.elapsed() < TOAST_TIME);
		if self.toasts.is_empty() {
			return;
		}
		if self
			.toasts
			.iter()
			.any(|t| matches!(t.status, Status::Info(_)))
		{
			ctx.request_repaint_after(std::time::Duration::from_millis(250));
		}
		let mut close = None;
		egui::Area::new(egui::Id::unique("toasts"))
			.anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
			.order(egui::Order::Foreground)
			.show(ctx, |ui| {
				for (i, toast) in self.toasts.iter().enumerate() {
					let (text, stripe) = match &toast.status {
						Status::Info(text) => (text, ui.visuals().selection.bg_fill),
						Status::Error(text) => (text, crate::theme::BAD),
					};
					egui::Frame::popup(ui.style())
						.corner_radius(crate::theme::radius(8.0))
						.show(ui, |ui| {
							ui.set_max_width(380.0);
							ui.horizontal(|ui| {
								let (rect, _) = ui.allocate_exact_size(
									egui::vec2(4.0, 20.0),
									egui::Sense::hover(),
								);
								ui.painter()
									.rect_filled(rect, crate::theme::radius(2.0), stripe);
								ui.add(egui::Label::new(text.as_str()).wrap());
								if ui
									.small_button("✕")
									.on_hover_text(t!("common.close"))
									.clicked()
								{
									close = Some(i);
								}
							});
						});
					ui.add_space(6.0);
				}
			});
		if let Some(i) = close {
			self.toasts.remove(i);
		}
	}
}

const LIBRARY_KEY: &str = "library";
const WIRELESS_KEY: &str = "wireless_address";
const LOADER_KEY: &str = "rename_loader";
const ADVANCED_KEY: &str = "advanced_mode";
const APPEARANCE_KEY: &str = "appearance";

impl eframe::App for SideloaderApp {
	fn save(&mut self, storage: &mut dyn eframe::Storage) {
		eframe::set_value(storage, LIBRARY_KEY, &self.library_view.prefs());
		eframe::set_value(storage, ADVANCED_KEY, &self.advanced);
		eframe::set_value(storage, APPEARANCE_KEY, &self.appearance);
		if let Some(path) = self.rename_view.loader() {
			eframe::set_value(storage, LOADER_KEY, &path);
		}
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
		if !self.advanced && self.tab.advanced() {
			self.tab = Tab::Library;
		}
		let mut commands = Vec::new();
		egui::Panel::left("sidebar")
			.resizable(false)
			.default_size(190.0)
			.show(ui, |ui| self.sidebar(ui));
		egui::CentralPanel::default().show(ui, |ui| match self.tab {
			Tab::Devices => {
				if self.devices_view.show(ui, &mut commands) {
					self.adb_setup.reopen();
				}
			}
			Tab::Headset if self.devices_view.install_target().is_none() => {
				ui.add_space(16.0);
				if self.devices_view.help(ui, &mut commands) {
					self.adb_setup.reopen();
				}
			}
			Tab::Headset => {
				let library = &self.library_view;
				let cover = |p: &str| library.cover_for_package(p);
				let latest = |p: &str| library.catalog_version(p);
				let target = self
					.devices_view
					.install_target()
					.map(|d| ui::headset::Target {
						serial: &d.serial,
						name: d.display_name(),
						info: self.devices_view.info(&d.serial),
						names: self.library_view.names(),
						copies: &self.copies,
						cover: &cover,
						latest: &latest,
					});
				self.headset_view.show(ui, target, &mut commands);
			}
			Tab::Library => {
				// Reading free space is a system call; a few seconds old is fine.
				if self
					.pc_free_checked
					.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5))
				{
					self.pc_free_checked = Some(std::time::Instant::now());
					self.library_view
						.set_pc_free(quest_sideloader_core::downloaded::free_space(
							self.settings_view.downloads_dir(),
						));
				}
				let installed = self.devices_view.installed_apps();
				let mut ignored = self.settings_view.contribute_ignored();
				ignored.extend(self.copies.iter().cloned());
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
				if let Some(action) = self.downloads_view.show(
					ui,
					self.settings_view.downloads_dir(),
					target,
					self.advanced,
					&|release| self.library_view.cover_for_release(release),
					&mut commands,
				) {
					match action {
						ui::downloads::Action::Rename(path) => {
							self.rename_view.add(path, &mut commands);
							self.tab = Tab::Rename;
						}
						ui::downloads::Action::Inspect(path) => {
							self.inspector_view.open(path, &mut commands);
							self.tab = Tab::Inspector;
						}
					}
				}
			}
			Tab::Rename => {
				self.rename_view
					.show(ui, self.settings_view.downloads_dir(), &mut commands);
			}
			Tab::Inspector => {
				self.inspector_view
					.show(ui, self.settings_view.downloads_dir(), &mut commands);
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
				let before = self.appearance;
				self.settings_view.show(
					ui,
					&default_downloads,
					&self.paths.rclone_dir(),
					&mut commands,
					ui::settings::Prefs {
						keys: &mut self.keys_view,
						advanced: &mut self.advanced,
						appearance: &mut self.appearance,
					},
				);
				if self.keys_view.take_changed() {
					self.copies = self.keys_view.packages().into_iter().collect();
				}
				if self.appearance != before {
					crate::theme::apply(ui.ctx(), &self.appearance);
				}
			}
		});
		let ctx = ui.ctx().clone();
		self.update_title(&ctx);
		self.show_toasts(&ctx);
		if self.checkup.is_open() {
			let checks = self.checks();
			self.checkup.show(&ctx, &checks);
		}
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
