use rust_i18n::t;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::sidebar::{
	Sidebar, SidebarCollapsible, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu,
	SidebarMenuItem,
};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
	ActiveTheme, Icon, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::{
	Controller, ControllerSide, DeviceDetails, DeviceEntry, DeviceInfo, DeviceState,
};
use quest_sideloader_core::{
	AdbStatus, AppPaths, CatalogStatus, Command, Engine, Event, Settings, TaskId, TaskKind,
	TaskSnapshot, TaskState, format_bytes,
};
use serde::{Deserialize, Serialize};

use crate::dialogs::checkup::{self, Check, Level};
use crate::dialogs::review::PendingReview;
use crate::dialogs::{review, tools, welcome};
use crate::icons::AppIcon;
use crate::logbuf::highlight::{LevelColors, LogHighlighter, SharedColors};
use crate::logbuf::{Level as LogLevel, LogBuffer, level_of};
use crate::theme::Mono;
use crate::ui_state::{SavedBounds, UiState};
use crate::views::catalog::{CatalogView, Installed};
use crate::views::console::ConsoleView;
use crate::views::devices::{DevicesEvent, DevicesView};
use crate::views::downloads::{DownloadsEvent, DownloadsView};
use crate::views::headset::HeadsetView;
use crate::views::inspector::InspectorView;
use crate::views::rename::RenameView;
use crate::views::settings::{SettingsEvent, SettingsView};
use quest_sideloader_core::rename::KeyStore;

const LOGS_HEIGHT: f32 = 240.;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tab {
	Headset,
	Catalog,
	Downloads,
	Rename,
	Devices,
	Inspector,
	Console,
	Settings,
}

impl Tab {
	fn label(self) -> String {
		match self {
			Tab::Headset => t!("tab.headset"),
			Tab::Catalog => t!("tab.catalog"),
			Tab::Downloads => t!("tab.downloads"),
			Tab::Rename => t!("tab.rename"),
			Tab::Devices => t!("tab.devices"),
			Tab::Inspector => t!("tab.inspector"),
			Tab::Console => t!("tab.console"),
			Tab::Settings => t!("tab.settings"),
		}
		.into_owned()
	}

	fn icon(self) -> AppIcon {
		match self {
			Tab::Headset => AppIcon::Headset,
			Tab::Catalog => AppIcon::Catalog,
			Tab::Downloads => AppIcon::Downloads,
			Tab::Rename => AppIcon::Rename,
			Tab::Devices => AppIcon::Devices,
			Tab::Inspector => AppIcon::Inspector,
			Tab::Console => AppIcon::Console,
			Tab::Settings => AppIcon::Settings,
		}
	}
}

pub struct Shell {
	engine: Rc<Engine>,
	paths: AppPaths,
	settings: Settings,
	ui_path: PathBuf,
	/// The Wi-Fi headset to reconnect to once adb runs.
	last_wireless: Option<String>,
	reconnect: bool,
	/// The adb dialog opens by itself once per run; the status bar reopens it.
	adb_prompted: bool,
	reviews: VecDeque<PendingReview>,
	review_open: Option<TaskId>,
	/// Renamed copies made here; they get a badge and lead the restore list.
	copies: HashSet<String>,
	title: String,
	tab: Tab,
	adb: Option<AdbStatus>,
	devices: Vec<DeviceEntry>,
	infos: HashMap<String, DeviceInfo>,
	details: HashMap<String, DeviceDetails>,
	selected: Option<String>,
	tasks: Vec<TaskSnapshot>,
	headset: Entity<HeadsetView>,
	catalog: Entity<CatalogView>,
	settings_view: Entity<SettingsView>,
	downloads: Entity<DownloadsView>,
	rename: Entity<RenameView>,
	devices_view: Entity<DevicesView>,
	inspector: Entity<InspectorView>,
	console: Entity<ConsoleView>,
	/// When free space in the downloads folder was last read.
	pc_free_checked: Option<std::time::Instant>,
	logs: LogBuffer,
	logs_open: bool,
	logs_seen: u64,
	log_lines: usize,
	/// The lowest level the logs panel shows.
	log_level: LogLevel,
	/// Error lines already seen in the logs panel.
	errors_seen: u64,
	/// The Advanced group in the sidebar is folded.
	advanced_folded: bool,
	/// Read-only editor, so the log can be selected, searched and copied.
	log_view: Entity<EditorState>,
	log_colors: SharedColors,
	_tasks: Vec<Task<()>>,
	_appearance: Subscription,
	_settings_events: Subscription,
	_downloads_events: Subscription,
	_devices_events: Subscription,
}

/// What `main` read before the window opened.
pub struct Startup {
	pub paths: AppPaths,
	pub settings: Settings,
	/// Why the settings file couldn't be read, when it couldn't.
	pub load_error: Option<String>,
	pub ui: UiState,
	/// There was no settings file yet.
	pub first_run: bool,
	pub logs: LogBuffer,
}

impl Shell {
	pub fn new(startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let Startup {
			paths,
			settings,
			load_error,
			ui,
			first_run,
			logs,
		} = startup;
		// The engine calls this from its own threads; it only wakes the UI task below.
		let (wake_tx, wake_rx) = smol::channel::unbounded::<()>();
		let engine = Engine::start(
			paths.clone(),
			settings.clone(),
			Arc::new(move || {
				let _ = wake_tx.try_send(());
			}),
		)
		.expect("start engine");
		let engine = Rc::new(engine);

		let events = cx.spawn_in(window, async move |this, cx| {
			while wake_rx.recv().await.is_ok() {
				while wake_rx.try_recv().is_ok() {}
				if this
					.update_in(cx, |this, window, cx| this.drain_events(window, cx))
					.is_err()
				{
					break;
				}
			}
		});
		// Log lines come from any thread; the panel picks them up a few times a second.
		let log_poll = cx.spawn_in(window, async move |this, cx| {
			loop {
				cx.background_executor()
					.timer(Duration::from_millis(400))
					.await;
				if this
					.update_in(cx, |this, window, cx| this.poll_logs(window, cx))
					.is_err()
				{
					break;
				}
			}
		});

		let appearance = cx.observe_window_appearance(window, |this, window, cx| {
			crate::theme::apply(&this.settings, window, cx);
		});
		window.on_window_should_close(cx, {
			let shell = cx.entity().downgrade();
			move |window, cx| {
				let bounds = window.window_bounds();
				let _ = shell.update(cx, |this, cx| this.save_ui(bounds, cx));
				true
			}
		});
		if let Some(error) = load_error {
			cx.defer_in(window, move |this, window, cx| {
				let text = t!("app.settings_load_error", error = error).into_owned();
				this.notify(Notification::error(text), window, cx);
			});
		}
		if first_run {
			cx.defer_in(window, |this, window, cx| this.open_welcome(window, cx));
		}

		let copies: HashSet<String> = KeyStore::new(paths.signing_dir())
			.packages()
			.into_iter()
			.collect();
		let catalog = cx.new(|cx| {
			let mut view = CatalogView::new(engine.clone(), window, cx);
			view.set_prefs(&ui.catalog);
			view.set_mirror(settings.active_mirror().map(|m| m.name.clone()), cx);
			view.set_no_upload(no_upload(&settings, &copies), cx);
			view
		});
		let downloads = cx.new(|cx| {
			DownloadsView::new(engine.clone(), settings.downloads_dir.clone(), window, cx)
		});
		let rename = cx.new(|_| {
			RenameView::new(
				engine.clone(),
				settings.downloads_dir.clone(),
				ui.rename_loader.clone(),
			)
		});
		let inspector = cx.new(|_| InspectorView::new(engine.clone()));
		let console = cx.new(|cx| ConsoleView::new(engine.clone(), window, cx));
		let devices_view =
			cx.new(|cx| DevicesView::new(engine.clone(), ui.last_wireless.clone(), window, cx));
		let devices_events = cx.subscribe_in(
			&devices_view,
			window,
			|this, _, event: &DevicesEvent, window, cx| match event {
				DevicesEvent::Select(serial) => this.select(Some(serial.clone()), cx),
				DevicesEvent::SetUpAdb => tools::open_adb(this.engine.clone(), window, cx),
			},
		);
		let downloads_events = cx.subscribe_in(
			&downloads,
			window,
			|this, _, event: &DownloadsEvent, window, cx| match event {
				DownloadsEvent::Rename(path) => {
					let path = path.clone();
					this.rename.update(cx, |r, cx| r.add(path, window, cx));
					this.set_tab(Tab::Rename, window, cx);
				}
				DownloadsEvent::Inspect(path) => {
					let path = path.clone();
					this.inspector.update(cx, |i, cx| i.open(path, cx));
					this.set_tab(Tab::Inspector, window, cx);
				}
			},
		);
		let settings_view =
			cx.new(|cx| SettingsView::new(engine.clone(), &paths, settings.clone(), window, cx));
		let settings_events = cx.subscribe_in(
			&settings_view,
			window,
			|this, _, event: &SettingsEvent, window, cx| match event {
				SettingsEvent::Checkup => checkup::open(this.checks(cx), window, cx),
			},
		);
		let headset = cx.new(|cx| {
			let mut view = HeadsetView::new(engine.clone(), window, cx);
			view.set_prefs(&ui.headset);
			view.set_copies(copies.clone());
			view
		});
		let log_colors = SharedColors::default();
		let log_view = cx.new(|cx| {
			let mut editor = EditorState::new(window, cx)
				.line_number(false)
				.soft_wrap(false);
			editor.set_readonly(true, cx);
			let colors = log_colors.clone();
			editor.set_highlighter_factory(
				Rc::new(move |_| Some(Box::new(LogHighlighter::new(colors.clone())))),
				cx,
			);
			editor.set_highlighter("log", cx);
			editor
		});
		engine.send(Command::ListBackups);
		Self {
			engine,
			ui_path: paths.ui_state_file(),
			paths,
			settings,
			reconnect: ui.last_wireless.is_some(),
			last_wireless: ui.last_wireless,
			adb_prompted: false,
			reviews: VecDeque::new(),
			review_open: None,
			copies,
			title: String::new(),
			tab: ui.tab.unwrap_or(Tab::Headset),
			adb: None,
			devices: Vec::new(),
			infos: HashMap::new(),
			details: HashMap::new(),
			selected: None,
			tasks: Vec::new(),
			headset,
			catalog,
			settings_view,
			downloads,
			rename,
			devices_view,
			inspector,
			console,
			pc_free_checked: None,
			logs,
			logs_open: ui.logs_open,
			logs_seen: 0,
			log_lines: 0,
			log_level: LogLevel::Trace,
			errors_seen: 0,
			advanced_folded: ui.advanced_folded,
			log_view,
			log_colors,
			_tasks: vec![events, log_poll],
			_appearance: appearance,
			_settings_events: settings_events,
			_downloads_events: downloads_events,
			_devices_events: devices_events,
		}
	}

	fn drain_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let events: Vec<Event> = self.engine.poll_events().collect();
		for event in events {
			self.handle_event(event, window, cx);
		}
		cx.notify();
	}

	fn handle_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
		match event {
			Event::SettingsSaved(settings) => {
				crate::apply_language(settings.language);
				crate::theme::apply(&settings, window, cx);
				let for_view = settings.clone();
				let announce = self
					.settings_view
					.update(cx, |s, cx| s.on_saved(for_view, window, cx));
				let folder = settings.downloads_dir.clone();
				let for_rename = folder.clone();
				self.downloads.update(cx, |d, cx| d.set_folder(folder, cx));
				self.rename
					.update(cx, |r, cx| r.set_downloads_dir(for_rename, cx));
				self.settings = settings;
				self.sync_catalog_settings(cx);
				window.refresh();
				if announce {
					self.notify(Notification::info(t!("app.settings_saved")), window, cx);
				}
			}
			Event::SettingsSaveFailed(text) => {
				self.settings_view.update(cx, |s, cx| s.save_failed(cx));
				self.notify(Notification::error(text), window, cx)
			}
			Event::AdbStatus(status) => {
				match &status {
					AdbStatus::Missing(_) if !self.adb_prompted => {
						self.adb_prompted = true;
						tools::open_adb(self.engine.clone(), window, cx);
					}
					AdbStatus::Running { .. } if std::mem::take(&mut self.reconnect) => {
						if let Some(address) = &self.last_wireless {
							self.engine.send(Command::Reconnect(address.clone()));
						}
					}
					_ => {}
				}
				self.adb = Some(status);
				self.update_installed(cx);
			}
			Event::RcloneMissing => tools::open_rclone(self.engine.clone(), window, cx),
			Event::ScriptReview {
				task,
				title,
				analysis,
			} => {
				self.reviews.push_back(PendingReview {
					task,
					title,
					analysis,
				});
				self.show_next_review(window, cx);
			}
			Event::DevicesChanged(devices) => {
				// mDNS serials (`adb-...`) can't be connected to by address.
				if let Some(device) = devices.iter().find(|d| {
					d.state == DeviceState::Online
						&& d.serial.contains(':')
						&& !d.serial.starts_with("adb-")
				}) {
					self.last_wireless = Some(device.serial.clone());
				}
				self.devices = devices;
				let online = |serial: &str| {
					self.devices
						.iter()
						.any(|d| d.serial == serial && d.state == DeviceState::Online)
				};
				if !self.selected.as_deref().is_some_and(online) {
					let first = self
						.devices
						.iter()
						.find(|d| d.state == DeviceState::Online)
						.map(|d| d.serial.clone());
					self.select(first, cx);
				}
				self.update_installed(cx);
			}
			Event::DeviceInfo { serial, info } => match info {
				Ok(info) => {
					if self.selected.as_deref() == Some(serial.as_str()) {
						let info = info.clone();
						self.headset
							.update(cx, |h, cx| h.set_info(&serial, Some(info), cx));
					}
					self.infos.insert(serial, info);
					self.update_installed(cx);
				}
				Err(e) => self.notify(Notification::error(e), window, cx),
			},
			Event::HeadsetDetails { serial, details } => {
				if let Ok((d, _)) = &details {
					self.details.insert(serial.clone(), d.clone());
				}
				self.headset
					.update(cx, |h, cx| h.set_details(&serial, details, cx));
			}
			Event::CatalogLoaded(catalog) => {
				let for_headset = catalog.clone();
				self.headset
					.update(cx, |h, cx| h.set_catalog(for_headset, cx));
				let for_downloads = catalog.clone();
				self.downloads
					.update(cx, |d, cx| d.set_catalog(for_downloads, cx));
				self.catalog.update(cx, |c, cx| c.set_catalog(catalog, cx));
			}
			Event::CatalogStatus(status) => {
				self.catalog.update(cx, |c, cx| c.set_status(status, cx));
			}
			Event::Backups(backups) => {
				self.headset.update(cx, |h, cx| h.set_backups(backups, cx));
			}
			Event::BackupsChanged => self.engine.send(Command::ListBackups),
			Event::RenameAnalysis { id, result } => {
				self.rename
					.update(cx, |r, cx| r.on_analysis(id, result, window, cx));
			}
			Event::Inspection { id, result } => {
				self.inspector
					.update(cx, |i, cx| i.on_result(id, result, cx));
			}
			Event::Shell { id, output } => {
				self.console.update(cx, |c, cx| c.on_output(id, output, cx));
			}
			Event::Downloads(releases) => {
				self.downloads
					.update(cx, |d, cx| d.set_releases(releases, cx));
			}
			Event::TaskUpdated(task) => self.on_task(task, window, cx),
			Event::Notice(text) => self.notify(Notification::info(text), window, cx),
			Event::Error(text) => self.notify(Notification::error(text), window, cx),
		}
	}

	fn on_task(&mut self, task: TaskSnapshot, window: &mut Window, cx: &mut Context<Self>) {
		let was_finished = self
			.tasks
			.iter()
			.find(|t| t.id == task.id)
			.is_some_and(|t| t.state.is_finished());
		let finished = task.state.is_finished() && !was_finished;
		if finished {
			self.forget_review(task.id, window, cx);
			self.notify_finished(&task, window);
		}
		let for_downloads = task.clone();
		self.downloads
			.update(cx, |d, cx| d.on_task(for_downloads, cx));
		if finished && (task.state == TaskState::Done || task.kind == TaskKind::Download) {
			// Downloads add release folders, or leave a part behind when they
			// stop; installs may remove folders.
			self.downloads.update(cx, |d, cx| d.invalidate(cx));
			if let Some(serial) = &task.serial {
				// Installs and uninstalls change the app list.
				self.engine.send(Command::RefreshDevice(serial.clone()));
				self.engine.send(Command::RefreshHeadset(serial.clone()));
			}
			if task.kind == TaskKind::Rename {
				self.copies = KeyStore::new(self.paths.signing_dir())
					.packages()
					.into_iter()
					.collect();
				let copies = self.copies.clone();
				self.headset
					.update(cx, |h, cx| h.set_copies_and_notify(copies, cx));
				self.sync_catalog_settings(cx);
				self.settings_view.update(cx, |s, cx| s.keys_changed(cx));
			}
		}
		match self.tasks.iter_mut().find(|t| t.id == task.id) {
			Some(slot) => *slot = task,
			None => self.tasks.push(task),
		}
		self.update_title(window);
	}

	/// A system notification when a task ends while the window is in the background.
	fn notify_finished(&self, task: &TaskSnapshot, window: &Window) {
		if !self.settings.notifications || window.is_window_active() {
			return;
		}
		match &task.state {
			TaskState::Done => crate::notify::send(task.title.clone(), task.stage.clone()),
			TaskState::Failed(e) => crate::notify::send(
				t!("app.task_failed", title = task.title).into_owned(),
				e.chars().take(200).collect(),
			),
			_ => {}
		}
	}

	/// The running task's progress goes in front of the title, for the taskbar.
	fn update_title(&mut self, window: &mut Window) {
		let base = crate::base_title(self.paths.portable_root().is_some());
		let title = match self.active_tasks().next() {
			Some(task) => match task.progress {
				Some(p) => format!("{:.0}% {} - {base}", p * 100., task.title),
				None => format!("{} - {base}", task.title),
			},
			None => base.to_owned(),
		};
		if title != self.title {
			window.set_window_title(&title);
			self.title = title;
		}
	}

	fn show_next_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		if self.review_open.is_some() {
			return;
		}
		let Some(next) = self.reviews.pop_front() else {
			return;
		};
		self.review_open = Some(next.task);
		let shell = cx.entity().downgrade();
		review::open(
			self.engine.clone(),
			next,
			move |window, cx| {
				let _ = shell.update(cx, |this, cx| {
					this.review_open = None;
					// The next one waits until this dialog is gone.
					cx.defer_in(window, |this, window, cx| this.show_next_review(window, cx));
				});
			},
			window,
			cx,
		);
	}

	/// The task was cancelled or failed elsewhere; its question is moot.
	fn forget_review(&mut self, task: TaskId, window: &mut Window, cx: &mut Context<Self>) {
		self.reviews.retain(|r| r.task != task);
		if self.review_open == Some(task) {
			self.review_open = None;
			window.close_dialog(cx);
			self.show_next_review(window, cx);
		}
	}

	fn open_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let shell = cx.entity().downgrade();
		welcome::Welcome::open(
			self.engine.clone(),
			self.settings.clone(),
			self.paths.default_downloads_dir(),
			move |outcome, _, cx| {
				if outcome == welcome::Outcome::OpenMirrors {
					let _ = shell.update(cx, |this, cx| {
						this.tab = Tab::Settings;
						this.settings_view.update(cx, |s, cx| s.open_mirrors(cx));
						cx.notify();
					});
				}
			},
			window,
			cx,
		);
	}

	/// Everything that has to be right to install games, for "Check everything".
	fn checks(&self, cx: &App) -> Vec<Check> {
		const GB: u64 = 1024 * 1024 * 1024;
		let mut checks = Vec::new();
		checks.push(match &self.adb {
			Some(AdbStatus::Running { .. }) => Check::new(Level::Ok, t!("chk.adb_ok"), ""),
			Some(AdbStatus::Starting) | None => {
				Check::new(Level::Warning, t!("chk.adb_starting"), "")
			}
			Some(AdbStatus::Missing(e) | AdbStatus::Unavailable(e)) => {
				Check::new(Level::Problem, t!("chk.adb_missing"), e.clone())
			}
		});
		checks.push(match (self.selected_device(), self.devices.first()) {
			(Some(device), _) => Check::new(
				Level::Ok,
				t!("chk.headset_ok", name = device.display_name()),
				"",
			),
			(None, Some(device)) => Check::new(
				Level::Warning,
				t!("chk.headset_waiting"),
				state_hint(&device.state),
			),
			(None, None) => Check::new(
				Level::Problem,
				t!("chk.headset_none"),
				t!("chk.headset_none_detail"),
			),
		});
		let info = self
			.selected_device()
			.and_then(|d| self.infos.get(&d.serial));
		if let Some(storage) = info.and_then(|i| i.storage) {
			let level = match storage.free_bytes {
				f if f < 2 * GB => Level::Problem,
				f if f < 10 * GB => Level::Warning,
				_ => Level::Ok,
			};
			let detail = if level == Level::Ok {
				String::new()
			} else {
				t!("chk.space_detail").into_owned()
			};
			checks.push(Check::new(
				level,
				t!("chk.headset_space", free = format_bytes(storage.free_bytes)),
				detail,
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
		match quest_sideloader_core::downloaded::free_space(&self.settings.downloads_dir) {
			Some(free) => {
				let level = match free {
					f if f < 3 * GB => Level::Problem,
					f if f < 10 * GB => Level::Warning,
					_ => Level::Ok,
				};
				let detail = if level == Level::Ok {
					String::new()
				} else {
					t!("chk.pc_space_detail").into_owned()
				};
				checks.push(Check::new(
					level,
					t!("chk.pc_space", free = format_bytes(free)),
					detail,
				));
			}
			None => checks.push(Check::new(
				Level::Warning,
				t!("chk.pc_space_unknown"),
				self.settings.downloads_dir.display().to_string(),
			)),
		}
		let catalog = self.catalog.read(cx);
		let (games, status) = catalog.state();
		let mirror = self.settings.active_mirror().map(|m| m.name.clone());
		checks.push(match (mirror, status, games) {
			(None, _, _) => Check::new(
				Level::Skipped,
				t!("chk.mirror_none"),
				t!("chk.mirror_none_detail"),
			),
			(Some(_), CatalogStatus::Failed(e), _) => {
				Check::new(Level::Problem, t!("chk.mirror_failed"), e.clone())
			}
			(Some(_), CatalogStatus::Syncing { .. }, _) => {
				Check::new(Level::Warning, t!("chk.mirror_syncing"), "")
			}
			(Some(name), _, Some(n)) => {
				Check::new(Level::Ok, t!("chk.mirror_ok", name = name, n = n), "")
			}
			(Some(name), _, None) => {
				Check::new(Level::Warning, t!("chk.mirror_empty", name = name), "")
			}
		});
		checks
	}

	fn save_ui(&self, bounds: WindowBounds, cx: &App) {
		let headset = self.headset.read(cx).prefs();
		let catalog = self.catalog.read(cx).prefs();
		UiState {
			window: Some(SavedBounds::from_window(bounds)),
			tab: Some(self.tab),
			logs_open: self.logs_open,
			scale: Some(crate::theme::scale(cx)),
			advanced_folded: self.advanced_folded,
			last_wireless: self.last_wireless.clone(),
			headset,
			catalog,
			rename_loader: self.rename.read(cx).loader(),
		}
		.save(&self.ui_path);
	}

	fn install_dropped(
		&mut self,
		paths: &ExternalPaths,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		let Some(device) = self.selected_device() else {
			self.notify(Notification::error(t!("app.no_headset")), window, cx);
			return;
		};
		let serial = device.serial.clone();
		for path in paths.paths() {
			self.engine.send(Command::Install {
				serial: serial.clone(),
				path: path.clone(),
			});
		}
	}

	/// Every notice opens the logs panel when clicked, where the details are.
	fn notify(&self, note: Notification, window: &mut Window, cx: &mut Context<Self>) {
		let shell = cx.entity().downgrade();
		let note = note.on_click(move |_, window, cx| {
			let _ = shell.update(cx, |this, cx| this.open_logs(window, cx));
		});
		window.push_notification(note, cx);
	}

	fn open_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		self.logs_open = true;
		self.logs_seen = 0;
		self.poll_logs(window, cx);
		cx.notify();
	}

	fn set_log_level(&mut self, level: LogLevel, window: &mut Window, cx: &mut Context<Self>) {
		self.log_level = level;
		self.logs_seen = 0;
		self.poll_logs(window, cx);
		cx.notify();
	}

	fn poll_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let generation = self.logs.generation();
		if !self.logs_open {
			// The badge on the logs button counts new errors.
			if self.logs.errors() != self.errors_seen {
				cx.notify();
			}
			return;
		}
		self.errors_seen = self.logs.errors();
		if generation == self.logs_seen {
			return;
		}
		let shown = self.log_lines;
		let min = self.log_level;
		// Lines without a level continue the line above; they stay.
		let lines: Vec<String> = self
			.logs
			.lines()
			.into_iter()
			.filter(|line| level_of(line).is_none_or(|level| level >= min))
			.collect();
		let updated = self.log_view.update(cx, |editor, cx| {
			// Replacing the text drops the selection; wait until the user is done with it.
			if !editor.selected_range().is_empty() {
				return false;
			}
			let at_bottom = editor
				.visible_row_range()
				.is_none_or(|rows| rows.end + 1 >= shown);
			editor.set_value(lines.join("\n"), window, cx);
			if at_bottom {
				// Clamped to the last line on the next layout.
				editor.set_scroll_offset(point(px(0.), px(-1e9)), cx);
			}
			true
		});
		if updated {
			self.logs_seen = generation;
			self.log_lines = lines.len();
		}
	}

	fn select(&mut self, serial: Option<String>, cx: &mut Context<Self>) {
		self.selected = serial.clone();
		let info = serial.as_ref().and_then(|s| self.infos.get(s)).cloned();
		self.headset.update(cx, |h, cx| match &serial {
			Some(serial) => h.set_info(serial, info, cx),
			None => h.clear(cx),
		});
		if let Some(serial) = serial {
			self.engine.send(Command::RefreshHeadset(serial));
		}
		self.update_installed(cx);
		cx.notify();
	}

	/// What the catalog compares its games against.
	fn update_installed(&mut self, cx: &mut Context<Self>) {
		let installed = self.selected_device().and_then(|device| {
			let info = self.infos.get(&device.serial)?;
			Some(Installed {
				device_name: device.display_name(),
				serial: device.serial.clone(),
				versions: info
					.packages
					.iter()
					.map(|p| (p.name.clone(), p.version_code))
					.collect(),
				installed_at: info
					.packages
					.iter()
					.filter_map(|p| Some((p.name.clone(), p.installed?)))
					.collect(),
				free_bytes: info.storage.map(|s| s.free_bytes),
			})
		});
		self.catalog
			.update(cx, |c, cx| c.set_installed(installed, cx));
		let target = self
			.selected_device()
			.map(|d| (d.serial.clone(), d.display_name()));
		let for_console = target.clone();
		self.downloads.update(cx, |d, cx| d.set_target(target, cx));
		self.console
			.update(cx, |c, cx| c.set_target(for_console, cx));
		let state = (
			self.adb.clone(),
			self.devices.clone(),
			self.infos.clone(),
			self.selected.clone(),
		);
		self.devices_view.update(cx, |d, cx| {
			let (adb, devices, infos, selected) = state;
			d.update_state(adb, devices, infos, selected, cx)
		});
	}

	fn sync_catalog_settings(&mut self, cx: &mut Context<Self>) {
		let mirror = self.settings.active_mirror().map(|m| m.name.clone());
		let no_upload = no_upload(&self.settings, &self.copies);
		self.catalog.update(cx, |c, cx| {
			c.set_mirror(mirror, cx);
			c.set_no_upload(no_upload, cx);
		});
	}

	fn set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
		self.tab = tab;
		if tab == Tab::Catalog {
			// So the catalog's keys work without clicking into it first.
			window.focus(&self.catalog.focus_handle(cx), cx);
		}
		cx.notify();
	}

	fn selected_device(&self) -> Option<&DeviceEntry> {
		let serial = self.selected.as_ref()?;
		self.devices.iter().find(|d| &d.serial == serial)
	}

	fn open_wifi_dialog(engine: Rc<Engine>, window: &mut Window, cx: &mut App) {
		let input = cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.50:5555"));
		window.open_dialog(cx, move |dialog, _, cx| {
			let connect = {
				let input = input.clone();
				let engine = engine.clone();
				move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
					let address = input.read(cx).value().trim().to_owned();
					if !address.is_empty() {
						engine.send(Command::ConnectWireless(address));
						window.close_dialog(cx);
					}
				}
			};
			dialog
				.title(t!("wifi.title").into_owned())
				.w(px(440.))
				.child(
					v_flex()
						.gap_3()
						.child(
							div()
								.text_sm()
								.text_color(cx.theme().muted_foreground)
								.child(t!("wifi.hint").into_owned()),
						)
						.child(Input::new(&input)),
				)
				.footer(
					h_flex()
						.w_full()
						.justify_end()
						.gap_2()
						.child(
							Button::new("wifi-cancel")
								.label(t!("common.cancel"))
								.outline()
								.on_click(|_, window, cx| window.close_dialog(cx)),
						)
						.child(
							Button::new("wifi-connect")
								.label(t!("wifi.connect"))
								.primary()
								.on_click(connect),
						),
				)
		});
	}

	fn device_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let (dot, title, subtitle) = match self.selected_device() {
			Some(device) => (
				theme.success,
				device.display_name(),
				transport_label(device),
			),
			None => match self.devices.first() {
				Some(device) => (
					theme.warning,
					device.display_name(),
					state_hint(&device.state),
				),
				None => (
					theme.muted_foreground,
					t!("dev.none").into_owned(),
					t!("dev.connect_hint").into_owned(),
				),
			},
		};

		let devices = self.devices.clone();
		let current = self.selected.clone();
		let engine = self.engine.clone();
		let shell = cx.entity().downgrade();
		let header = SidebarHeader::new()
			.w_full()
			.child(
				h_flex()
					.w_full()
					.gap_2()
					.child(div().size_2().flex_shrink_0().rounded_full().bg(dot))
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(title))
							.child(
								div()
									.text_sm()
									.text_color(theme.muted_foreground)
									.truncate()
									.child(subtitle),
							),
					)
					.child(
						Icon::new(IconName::ChevronsUpDown)
							.small()
							.text_color(theme.muted_foreground),
					),
			)
			.dropdown_menu(move |mut menu, _, _| {
				for device in &devices {
					let serial = device.serial.clone();
					let online = device.state == DeviceState::Online;
					let detail = if online {
						transport_label(device)
					} else {
						state_hint(&device.state)
					};
					let shell = shell.clone();
					menu = menu.item(
						PopupMenuItem::new(format!("{}  {detail}", device.display_name()))
							.checked(current.as_deref() == Some(serial.as_str()))
							.disabled(!online)
							.on_click(move |_, _, cx| {
								let serial = serial.clone();
								let _ = shell.update(cx, |s, cx| s.select(Some(serial), cx));
							}),
					);
				}
				if !devices.is_empty() {
					menu = menu.separator();
				}
				let engine = engine.clone();
				let manage = shell.clone();
				menu.item(
					PopupMenuItem::new(t!("wifi.menu"))
						.icon(Icon::new(IconName::Globe))
						.on_click(move |_, window, cx| {
							Self::open_wifi_dialog(engine.clone(), window, cx);
						}),
				)
				.item(
					PopupMenuItem::new(t!("dev.manage"))
						.icon(Icon::from(AppIcon::Devices))
						.on_click(move |_, _, cx| {
							let _ = manage.update(cx, |s, cx| {
								s.tab = Tab::Devices;
								cx.notify();
							});
						}),
				)
			});
		// The menu wraps the header in a container as wide as its content; a column
		// stretches that container to the sidebar's width.
		v_flex().flex_1().min_w_0().child(header)
	}

	fn nav_item(&self, tab: Tab, cx: &mut Context<Self>) -> SidebarMenuItem {
		let mut item = SidebarMenuItem::new(tab.label())
			.icon(Icon::from(tab.icon()))
			.active(self.tab == tab)
			.on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)));
		let active = self.active_tasks().count();
		if tab == Tab::Downloads && active > 0 {
			item = item.suffix(move |_, _| Tag::primary().small().child(active.to_string()));
		}
		item
	}

	fn active_tasks(&self) -> impl Iterator<Item = &TaskSnapshot> {
		self.tasks.iter().filter(|t| !t.state.is_finished())
	}

	fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let main = SidebarMenu::new().children([
			self.nav_item(Tab::Headset, cx),
			self.nav_item(Tab::Catalog, cx),
			self.nav_item(Tab::Downloads, cx),
			self.nav_item(Tab::Rename, cx),
		]);
		// Folded, the group still shows the tab that is open.
		let advanced = SidebarMenu::new().children(
			[Tab::Devices, Tab::Inspector, Tab::Console]
				.into_iter()
				.filter(|tab| !self.advanced_folded || self.tab == *tab)
				.map(|tab| self.nav_item(tab, cx))
				.collect::<Vec<_>>(),
		);
		Sidebar::new("sidebar")
			.collapsible(SidebarCollapsible::None)
			.w(px(240.))
			.header(self.device_switcher(cx))
			.child(SidebarGroup::new(t!("nav.main")).child(main))
			.child(
				SidebarGroup::new(t!("nav.advanced"))
					.child(advanced)
					.suffix({
						let shell = cx.entity().downgrade();
						let folded = self.advanced_folded;
						move |_, _| {
							let shell = shell.clone();
							Button::new("fold-advanced")
								.icon(Icon::new(if folded {
									IconName::ChevronRight
								} else {
									IconName::ChevronDown
								}))
								.ghost()
								.xsmall()
								.on_click(move |_, _, cx| {
									let _ = shell.update(cx, |this, cx| {
										this.advanced_folded = !this.advanced_folded;
										cx.notify();
									});
								})
						}
					}),
			)
			.footer(
				SidebarFooter::new().p_0().child(
					Button::new("nav-settings")
						.icon(Icon::from(AppIcon::Settings))
						.label(Tab::Settings.label())
						.ghost()
						.w_full()
						.justify_start()
						.selected(self.tab == Tab::Settings)
						.on_click(cx.listener(|this, _, window, cx| {
							this.set_tab(Tab::Settings, window, cx)
						})),
				),
			)
	}

	fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let muted = theme.muted_foreground;
		let sep = || div().w_px().h_4().bg(theme.border);
		let item = |text: String| div().child(text);

		let mut left = h_flex().gap_3().min_w_0().overflow_hidden();
		match self.selected_device() {
			Some(device) => {
				left = left
					.child(
						h_flex()
							.gap_2()
							.child(div().size_2().rounded_full().bg(theme.success))
							.child(item(device.display_name())),
					)
					.child(sep())
					.child(item(transport_label(device)).text_color(muted).mono(cx));
				if let Some(info) = self.infos.get(&device.serial) {
					if let Some(b) = info.battery {
						let charging = if b.charging { "+" } else { "" };
						let mut text = format!("{}{charging}%", b.level);
						if let Some(temp) = self
							.details
							.get(&device.serial)
							.and_then(|d| d.battery_temperature)
						{
							text.push_str(&format!("  {temp:.0}C"));
						}
						left = left.child(sep()).child(
							h_flex()
								.gap_1()
								.child(Icon::new(battery_icon(b.level, b.charging)).small())
								.child(item(text)),
						);
					}
					let pads = self
						.details
						.get(&device.serial)
						.map(|d| controllers_text(&d.controllers))
						.unwrap_or_default();
					if !pads.is_empty() {
						left = left.child(sep()).child(
							h_flex()
								.id("status-controllers")
								.gap_1()
								.child(Icon::from(AppIcon::Controller).small())
								.child(item(pads))
								.tooltip(|window, cx| {
									Tooltip::new(t!("hs.controllers_hint").into_owned())
										.build(window, cx)
								}),
						);
					}
					if let Some(s) = info.storage {
						left = left.child(sep()).child(
							h_flex()
								.gap_1()
								.child(Icon::new(IconName::HardDrive).small())
								.child(item(
									t!(
										"dev.free_of",
										free = format_bytes(s.free_bytes),
										total = format_bytes(s.total_bytes)
									)
									.into_owned(),
								)),
						);
					}
				}
			}
			None => {
				left = left.child(item(t!("dev.none").into_owned()).text_color(muted));
			}
		}
		if let Some(adb) = &self.adb {
			let text = match adb {
				AdbStatus::Running { version } => format!("adb {version}"),
				AdbStatus::Starting => t!("adb.starting").into_owned(),
				AdbStatus::Unavailable(_) | AdbStatus::Missing(_) => t!("adb.down").into_owned(),
			};
			let down = matches!(adb, AdbStatus::Unavailable(_) | AdbStatus::Missing(_));
			left = left.child(sep()).child(
				div()
					.id("adb-status")
					.text_color(if down { theme.danger } else { muted })
					.mono(cx)
					.when(down, |d| {
						d.cursor_pointer()
							.on_click(cx.listener(|this, _, window, cx| {
								tools::open_adb(this.engine.clone(), window, cx)
							}))
					})
					.child(text),
			);
		}

		let active: Vec<&TaskSnapshot> = self.active_tasks().collect();
		let task = active.first().map(|task| {
			let mut text = task.title.clone();
			if let Some(p) = task.progress {
				text.push_str(&format!("  {:.0}%", p * 100.));
			}
			if active.len() > 1 {
				text.push_str(&format!("  +{}", active.len() - 1));
			}
			h_flex()
				.id("status-task")
				.gap_2()
				.max_w(px(360.))
				.cursor_pointer()
				.child(Icon::from(AppIcon::Downloads).small())
				.child(div().truncate().child(text))
				.on_click(
					cx.listener(|this, _, window, cx| this.set_tab(Tab::Downloads, window, cx)),
				)
		});

		h_flex()
			.h(px(30.))
			.px_3()
			.gap_3()
			.text_sm()
			.bg(theme.status_bar)
			.border_t_1()
			.border_color(theme.status_bar_border)
			.child(left.flex_1())
			.children(task)
			.child(
				Button::new("toggle-logs")
					.icon(Icon::from(AppIcon::Logs))
					.label(match self.logs.errors().saturating_sub(self.errors_seen) {
						0 => t!("tab.logs").into_owned(),
						n => t!("logs.button_errors", n = n).into_owned(),
					})
					.ghost()
					.xsmall()
					.when(self.logs.errors() > self.errors_seen, |b| b.danger())
					.tooltip(t!("logs.button_hint"))
					.selected(self.logs_open)
					.on_click(cx.listener(|this, _, window, cx| {
						this.logs_open = !this.logs_open;
						this.logs_seen = 0;
						this.poll_logs(window, cx);
						cx.notify();
					})),
			)
	}

	fn logs_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		self.log_colors.set(LevelColors {
			error: theme.danger,
			warn: theme.warning,
			debug: theme.muted_foreground,
		});
		let level = self.log_level;
		let shell = cx.entity().downgrade();
		let folder = self.paths.logs_dir();
		let toolbar = h_flex()
			.px_3()
			.py_1()
			.gap_2()
			.text_sm()
			.border_b_1()
			.border_color(theme.border)
			.child(
				Button::new("log-level")
					.label(level_label(level))
					.ghost()
					.xsmall()
					.dropdown_caret(true)
					.dropdown_menu(move |mut menu, _, _| {
						for choice in [
							LogLevel::Trace,
							LogLevel::Debug,
							LogLevel::Info,
							LogLevel::Warn,
							LogLevel::Error,
						] {
							let shell = shell.clone();
							menu = menu.item(
								PopupMenuItem::new(level_label(choice))
									.checked(choice == level)
									.on_click(move |_, window, cx| {
										let _ = shell.update(cx, |this, cx| {
											this.set_log_level(choice, window, cx)
										});
									}),
							);
						}
						menu
					}),
			)
			.child(
				div()
					.text_color(theme.muted_foreground)
					.child(t!("logs.count", n = self.log_lines).into_owned()),
			)
			.child(div().flex_1())
			.child(
				Button::new("logs-folder")
					.label(t!("common.open_folder"))
					.icon(Icon::new(IconName::FolderOpen))
					.ghost()
					.xsmall()
					.on_click(move |_, _, _| crate::widgets::open_folder(&folder)),
			)
			.child(
				Button::new("logs-close")
					.icon(Icon::new(IconName::Close))
					.ghost()
					.xsmall()
					.on_click(cx.listener(|this, _, _, cx| {
						this.logs_open = false;
						cx.notify();
					})),
			);
		v_flex()
			.h(px(LOGS_HEIGHT))
			.flex_shrink_0()
			.border_t_1()
			.border_color(theme.border)
			.bg(theme.sidebar)
			.child(toolbar)
			.child(
				div().flex_1().min_h_0().child(
					Editor::new(&self.log_view)
						.readonly(true)
						.bordered(false)
						.appearance(false)
						.h_full(),
				),
			)
	}
}

/// Packages never offered for upload: ignored by the user, or renamed copies.
fn no_upload(settings: &Settings, copies: &HashSet<String>) -> HashSet<String> {
	settings
		.contribute_ignored
		.iter()
		.cloned()
		.chain(copies.iter().cloned())
		.collect()
}

fn level_label(level: LogLevel) -> String {
	match level {
		LogLevel::Trace => t!("logs.everything"),
		LogLevel::Debug => t!("logs.debug"),
		LogLevel::Info => t!("logs.info"),
		LogLevel::Warn => t!("logs.warnings"),
		LogLevel::Error => t!("logs.errors"),
	}
	.into_owned()
}

fn transport_label(device: &DeviceEntry) -> String {
	if device.is_wireless() {
		format!("Wi-Fi {}", device.serial)
	} else {
		"USB".to_owned()
	}
}

fn state_hint(state: &DeviceState) -> String {
	match state {
		DeviceState::Unauthorized | DeviceState::Authorizing => t!("dev.unauthorized").into_owned(),
		_ => t!("dev.offline").into_owned(),
	}
}

/// `L 60%  R 70%`, or empty when no controller is paired.
fn controllers_text(controllers: &[Controller]) -> String {
	controllers
		.iter()
		.filter_map(|c| {
			let side = match c.side {
				ControllerSide::Left => t!("hs.left_short"),
				ControllerSide::Right => t!("hs.right_short"),
			};
			Some(format!("{side} {}%", c.battery?))
		})
		.collect::<Vec<_>>()
		.join("  ")
}

fn battery_icon(level: u8, charging: bool) -> IconName {
	match level {
		_ if charging => IconName::BatteryCharging,
		0..=15 => IconName::BatteryWarning,
		16..=40 => IconName::BatteryLow,
		41..=80 => IconName::BatteryMedium,
		_ => IconName::BatteryFull,
	}
}

impl Render for Shell {
	fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		self.update_title(window);
		let content = match self.tab {
			Tab::Headset if self.selected_device().is_none() => {
				self.devices_view.update(cx, |d, cx| d.help(cx))
			}
			Tab::Headset => self.headset.clone().into_any_element(),
			Tab::Devices => self.devices_view.clone().into_any_element(),
			Tab::Inspector => self.inspector.clone().into_any_element(),
			Tab::Console => self.console.clone().into_any_element(),
			Tab::Settings => self.settings_view.clone().into_any_element(),
			Tab::Downloads => self.downloads.clone().into_any_element(),
			Tab::Rename => self.rename.clone().into_any_element(),
			Tab::Catalog => {
				// Reading free space is a system call; a few seconds old is fine.
				if self
					.pc_free_checked
					.is_none_or(|t| t.elapsed() > Duration::from_secs(5))
				{
					self.pc_free_checked = Some(std::time::Instant::now());
					let free =
						quest_sideloader_core::downloaded::free_space(&self.settings.downloads_dir);
					self.catalog.update(cx, |c, _| c.set_pc_free(free));
				}
				self.catalog.clone().into_any_element()
			}
		};
		let drop_tint = cx.theme().primary.opacity(0.08);
		let drop_border = cx.theme().primary;
		v_flex()
			.id("shell")
			.size_full()
			.bg(cx.theme().background)
			.text_color(cx.theme().foreground)
			.drag_over::<ExternalPaths>(move |style, _, _, _| {
				style.bg(drop_tint).border_2().border_color(drop_border)
			})
			.on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
				this.install_dropped(paths, window, cx)
			}))
			.child(
				h_flex()
					.flex_1()
					.min_h_0()
					.child(self.sidebar(cx))
					.child(div().flex_1().min_w_0().h_full().child(content)),
			)
			.when(self.logs_open, |this| this.child(self.logs_panel(cx)))
			.child(self.status_bar(cx))
	}
}
