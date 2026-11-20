use rust_i18n::t;
use std::collections::HashMap;
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
use gpui_kit::component::{
	ActiveTheme, Icon, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::{DeviceDetails, DeviceEntry, DeviceInfo, DeviceState};
use quest_sideloader_core::{
	AdbStatus, AppPaths, Command, Engine, Event, Settings, TaskSnapshot, TaskState, format_bytes,
};

use crate::headset::HeadsetView;
use crate::icons::AppIcon;
use crate::logbuf::LogBuffer;
use crate::logbuf::highlight::{LevelColors, LogHighlighter, SharedColors};
use crate::theme::Mono;

const LOGS_HEIGHT: f32 = 240.;

#[derive(Clone, Copy, PartialEq, Eq)]
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
	tab: Tab,
	adb: Option<AdbStatus>,
	devices: Vec<DeviceEntry>,
	infos: HashMap<String, DeviceInfo>,
	details: HashMap<String, DeviceDetails>,
	selected: Option<String>,
	tasks: Vec<TaskSnapshot>,
	headset: Entity<HeadsetView>,
	logs: LogBuffer,
	logs_open: bool,
	logs_seen: u64,
	log_lines: usize,
	/// Read-only editor, so the log can be selected, searched and copied.
	log_view: Entity<EditorState>,
	log_colors: SharedColors,
	_tasks: Vec<Task<()>>,
	_appearance: Subscription,
}

impl Shell {
	pub fn new(
		paths: AppPaths,
		settings: Settings,
		logs: LogBuffer,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		// The engine calls this from its own threads; it only wakes the UI task below.
		let (wake_tx, wake_rx) = smol::channel::unbounded::<()>();
		let engine = Engine::start(
			paths,
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

		let theme = settings.theme;
		let appearance = cx.observe_window_appearance(window, move |_, window, cx| {
			crate::theme::apply(theme, window, cx);
		});

		let headset = cx.new(|cx| HeadsetView::new(engine.clone(), window, cx));
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
			tab: Tab::Headset,
			adb: None,
			devices: Vec::new(),
			infos: HashMap::new(),
			details: HashMap::new(),
			selected: None,
			tasks: Vec::new(),
			headset,
			logs,
			logs_open: false,
			logs_seen: 0,
			log_lines: 0,
			log_view,
			log_colors,
			_tasks: vec![events, log_poll],
			_appearance: appearance,
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
			Event::AdbStatus(status) => self.adb = Some(status),
			Event::DevicesChanged(devices) => {
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
			}
			Event::DeviceInfo { serial, info } => match info {
				Ok(info) => {
					if self.selected.as_deref() == Some(serial.as_str()) {
						let info = info.clone();
						self.headset
							.update(cx, |h, cx| h.set_info(&serial, Some(info), cx));
					}
					self.infos.insert(serial, info);
				}
				Err(e) => window.push_notification(Notification::error(e), cx),
			},
			Event::HeadsetDetails { serial, details } => {
				if let Ok((d, _)) = &details {
					self.details.insert(serial.clone(), d.clone());
				}
				self.headset
					.update(cx, |h, cx| h.set_details(&serial, details, cx));
			}
			Event::CatalogLoaded(catalog) => {
				self.headset.update(cx, |h, cx| h.set_catalog(catalog, cx));
			}
			Event::Backups(backups) => {
				self.headset.update(cx, |h, cx| h.set_backups(backups, cx));
			}
			Event::BackupsChanged => self.engine.send(Command::ListBackups),
			Event::TaskUpdated(task) => self.on_task(task),
			Event::Notice(text) => window.push_notification(Notification::info(text), cx),
			Event::Error(text) => window.push_notification(Notification::error(text), cx),
			_ => {}
		}
	}

	fn on_task(&mut self, task: TaskSnapshot) {
		let finished_here = task.state == TaskState::Done && task.serial.is_some();
		if finished_here && let Some(serial) = &task.serial {
			// Installs and uninstalls change the app list.
			self.engine.send(Command::RefreshDevice(serial.clone()));
			self.engine.send(Command::RefreshHeadset(serial.clone()));
		}
		match self.tasks.iter_mut().find(|t| t.id == task.id) {
			Some(slot) => *slot = task,
			None => self.tasks.push(task),
		}
	}

	fn poll_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let generation = self.logs.generation();
		if !self.logs_open || generation == self.logs_seen {
			return;
		}
		let shown = self.log_lines;
		let lines = self.logs.lines();
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
			.on_click(cx.listener(move |this, _, _, cx| {
				this.tab = tab;
				cx.notify();
			}));
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
		let advanced = SidebarMenu::new().children([
			self.nav_item(Tab::Devices, cx),
			self.nav_item(Tab::Inspector, cx),
			self.nav_item(Tab::Console, cx),
		]);
		Sidebar::new("sidebar")
			.collapsible(SidebarCollapsible::None)
			.w(px(240.))
			.header(self.device_switcher(cx))
			.child(SidebarGroup::new(t!("nav.main")).child(main))
			.child(SidebarGroup::new(t!("nav.advanced")).child(advanced))
			.footer(
				SidebarFooter::new().p_0().child(
					Button::new("nav-settings")
						.icon(Icon::from(AppIcon::Settings))
						.label(Tab::Settings.label())
						.ghost()
						.w_full()
						.justify_start()
						.selected(self.tab == Tab::Settings)
						.on_click(cx.listener(|this, _, _, cx| {
							this.tab = Tab::Settings;
							cx.notify();
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
			left = left
				.child(sep())
				.child(item(text).text_color(muted).mono(cx));
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
				.on_click(cx.listener(|this, _, _, cx| {
					this.tab = Tab::Downloads;
					cx.notify();
				}))
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
					.label(t!("tab.logs"))
					.ghost()
					.xsmall()
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
		div()
			.h(px(LOGS_HEIGHT))
			.flex_shrink_0()
			.border_t_1()
			.border_color(theme.border)
			.bg(theme.sidebar)
			.child(
				Editor::new(&self.log_view)
					.readonly(true)
					.bordered(false)
					.appearance(false)
					.h_full(),
			)
	}
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
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let content = match self.tab {
			Tab::Headset => self.headset.clone().into_any_element(),
			other => div()
				.size_full()
				.flex()
				.items_center()
				.justify_center()
				.text_color(cx.theme().muted_foreground)
				.child(format!("{}: {}", other.label(), t!("proto.not_ported")))
				.into_any_element(),
		};
		v_flex()
			.size_full()
			.bg(cx.theme().background)
			.text_color(cx.theme().foreground)
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
