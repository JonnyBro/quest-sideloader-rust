mod events;
mod sidebar;
mod status_bar;

use rust_i18n::t;
use sidebar::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
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
use quest_sideloader_core::app_update::AppRelease;
use quest_sideloader_core::{
	AdbStatus, AppPaths, CatalogStatus, Command, Engine, Event, Settings, TaskId, TaskKind,
	TaskSnapshot, TaskState, format_bytes,
};
use serde::{Deserialize, Serialize};

use crate::dialogs::checkup::{self, Check, Level};
use crate::dialogs::review::PendingReview;
use crate::dialogs::wifi::WifiDialog;
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
use crate::views::headset::{HeadsetEvent, HeadsetView};
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
	last_wireless: Option<String>,
	reconnect: bool,
	/// The adb dialog opens by itself once per run; the status bar reopens it.
	adb_prompted: bool,
	reviews: VecDeque<PendingReview>,
	review_open: Option<TaskId>,
	/// Renamed copies made here; they get a badge and lead the restore list.
	copies: HashSet<String>,
	app_update: Option<AppRelease>,
	title: String,
	tab: Tab,
	adb: Option<AdbStatus>,
	devices: Vec<DeviceEntry>,
	wifi: Option<WeakEntity<WifiDialog>>,
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
	pc_free_checked: Option<std::time::Instant>,
	logs: LogBuffer,
	logs_open: bool,
	logs_seen: u64,
	log_lines: usize,
	log_level: LogLevel,
	errors_seen: u64,
	advanced_folded: bool,
	/// Read-only editor, so the log can be selected, searched and copied.
	log_view: Entity<EditorState>,
	log_colors: SharedColors,
	_tasks: Vec<Task<()>>,
	_appearance: Subscription,
	_settings_events: Subscription,
	_downloads_events: Subscription,
	_headset_events: Subscription,
	_devices_events: Subscription,
}

pub struct Startup {
	pub paths: AppPaths,
	pub settings: Settings,
	pub load_error: Option<String>,
	pub ui: UiState,
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
			view.set_mirror(
				settings
					.active_mirror()
					.map(|m| (m.id.clone(), m.name.clone())),
				cx,
			);
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
				SettingsEvent::KeysChanged => this.reload_copies(cx),
			},
		);
		let headset = cx.new(|cx| {
			let mut view = HeadsetView::new(engine.clone(), window, cx);
			view.set_prefs(&ui.headset);
			view.set_copies(copies.clone());
			view.set_held(held_updates(&settings), cx);
			view
		});
		let headset_events = cx.subscribe_in(
			&headset,
			window,
			|this, _, event: &HeadsetEvent, window, cx| match event {
				HeadsetEvent::AppLog { package, name } => {
					// Package names never need quoting; anything else isn't one.
					if !package
						.chars()
						.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
					{
						return;
					}
					// Waits for the app to start, so a crash right at launch is caught too.
					let command = format!(
						"until pid=$(pidof -s {package}); do sleep 1; done; logcat -v time --pid=$pid"
					);
					let note = t!("console.app_log", name = name).into_owned();
					this.console
						.update(cx, |c, cx| c.run_command(command, Some(note), cx));
					this.set_tab(Tab::Console, window, cx);
				}
			},
		);
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
		if settings.check_app_updates {
			engine.send(Command::CheckAppUpdate);
		}
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
			app_update: None,
			title: String::new(),
			tab: ui.tab.unwrap_or(Tab::Headset),
			adb: None,
			devices: Vec::new(),
			wifi: None,
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
			_headset_events: headset_events,
			_devices_events: devices_events,
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
				streaming: true,
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
				held: held_updates(&self.settings),
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
		let mirror = self
			.settings
			.active_mirror()
			.map(|m| (m.id.clone(), m.name.clone()));
		let no_upload = no_upload(&self.settings, &self.copies);
		let held = held_updates(&self.settings);
		self.headset
			.update(cx, |h, cx| h.set_held(held.clone(), cx));
		self.catalog.update(cx, |c, cx| {
			c.set_mirror(mirror, cx);
			c.set_no_upload(no_upload, cx);
			c.set_held(held, cx);
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

	fn open_wifi_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let dialog = WifiDialog::open(self.engine.clone(), self.device_serials(), window, cx);
		self.wifi = Some(dialog.downgrade());
	}

	fn device_serials(&self) -> HashSet<String> {
		self.devices.iter().map(|d| d.serial.clone()).collect()
	}

	fn active_tasks(&self) -> impl Iterator<Item = &TaskSnapshot> {
		self.tasks.iter().filter(|t| !t.state.is_finished())
	}
}

/// A package can have several entries, such as a game and its save patch; the
/// list is sorted by name, so the first is usually the game.
pub fn game_names(catalog: &quest_sideloader_core::catalog::Catalog) -> HashMap<String, String> {
	let mut names = HashMap::new();
	for game in &catalog.games {
		names
			.entry(game.package.clone())
			.or_insert_with(|| game.name.clone());
	}
	names
}

fn held_updates(settings: &Settings) -> HashSet<String> {
	settings.update_ignored.iter().cloned().collect()
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
