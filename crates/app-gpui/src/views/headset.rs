use rust_i18n::t;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::{AppUsage, DeviceDetails, DeviceInfo, RebootMode, Tweak};
use quest_sideloader_core::catalog::Catalog;
use quest_sideloader_core::saves::Backup;
use quest_sideloader_core::{AppInfo, Command, Engine, format_bytes};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::ui_state::HeadsetPrefs;
use crate::widgets::{
	Confirm, card, centered, column_header, confirm, format_date, open_folder, send_item, soft_tag,
	table_row,
};

type Details = Result<(DeviceDetails, HashMap<String, AppUsage>), String>;

const ROW_HEIGHT: f32 = 46.;
const COVER: (f32, f32) = (64., 36.);
const VERSION_W: f32 = 120.;
const SIZE_W: f32 = 96.;
const TAG_W: f32 = 120.;
const DATE_W: f32 = 150.;
const MENU_W: f32 = 28.;
const BACKUP_ACTIONS_W: f32 = 300.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
	Size,
	Name,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
	Apps,
	Device,
	Backups,
}

struct Row {
	app: AppInfo,
	/// `None` until the headset reports per-app storage.
	size: Option<u64>,
	cover: Option<PathBuf>,
	/// The newest version the catalog has.
	latest: Option<u64>,
}

pub struct HeadsetView {
	engine: Rc<Engine>,
	serial: Option<String>,
	info: Option<DeviceInfo>,
	details: Option<Details>,
	catalog: Option<Arc<Catalog>>,
	names: HashMap<String, String>,
	backups: Vec<Backup>,
	section: Section,
	filter: Entity<InputState>,
	sort: Sort,
	show_system: bool,
	rows: Vec<Row>,
	/// Rows hidden because they are system apps.
	hidden: usize,
	/// Renamed copies made here.
	copies: HashSet<String>,
	_filter_changes: Subscription,
}

impl HeadsetView {
	pub fn new(engine: Rc<Engine>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("hs.filter")));
		let subscription = cx.subscribe_in(&filter, window, |_, _, event, _, cx| {
			if matches!(event, InputEvent::Change) {
				cx.notify();
			}
		});
		Self {
			engine,
			serial: None,
			info: None,
			details: None,
			catalog: None,
			names: HashMap::new(),
			backups: Vec::new(),
			section: Section::Apps,
			filter,
			sort: Sort::Size,
			show_system: false,
			rows: Vec::new(),
			hidden: 0,
			copies: HashSet::new(),
			_filter_changes: subscription,
		}
	}

	pub fn set_prefs(&mut self, prefs: &HeadsetPrefs) {
		self.sort = if prefs.sort_by_name {
			Sort::Name
		} else {
			Sort::Size
		};
		self.show_system = prefs.show_system;
	}

	pub fn prefs(&self) -> HeadsetPrefs {
		HeadsetPrefs {
			sort_by_name: self.sort == Sort::Name,
			show_system: self.show_system,
		}
	}

	pub fn set_copies(&mut self, copies: HashSet<String>) {
		self.copies = copies;
	}

	pub fn set_copies_and_notify(&mut self, copies: HashSet<String>, cx: &mut Context<Self>) {
		self.copies = copies;
		cx.notify();
	}

	pub fn clear(&mut self, cx: &mut Context<Self>) {
		self.serial = None;
		self.info = None;
		self.details = None;
		cx.notify();
	}

	pub fn set_info(&mut self, serial: &str, info: Option<DeviceInfo>, cx: &mut Context<Self>) {
		if self.serial.as_deref() != Some(serial) {
			self.serial = Some(serial.to_owned());
			self.details = None;
		}
		self.info = info;
		cx.notify();
	}

	pub fn set_details(&mut self, serial: &str, details: Details, cx: &mut Context<Self>) {
		if self.serial.as_deref() == Some(serial) {
			self.details = Some(details);
			cx.notify();
		}
	}

	pub fn set_catalog(&mut self, catalog: Option<Arc<Catalog>>, cx: &mut Context<Self>) {
		self.names = catalog
			.iter()
			.flat_map(|c| c.games.iter())
			.map(|g| (g.package.clone(), g.name.clone()))
			.collect();
		self.catalog = catalog;
		cx.notify();
	}

	pub fn set_backups(&mut self, backups: Vec<Backup>, cx: &mut Context<Self>) {
		self.backups = backups;
		cx.notify();
	}

	fn device_details(&self) -> Option<&DeviceDetails> {
		match &self.details {
			Some(Ok((details, _))) => Some(details),
			_ => None,
		}
	}

	fn refresh(&mut self, cx: &mut Context<Self>) {
		if let Some(serial) = &self.serial {
			self.details = None;
			self.engine.send(Command::RefreshDevice(serial.clone()));
			self.engine.send(Command::RefreshHeadset(serial.clone()));
			cx.notify();
		}
	}

	fn send(&self, command: impl FnOnce(String) -> Command) {
		if let Some(serial) = &self.serial {
			self.engine.send(command(serial.clone()));
		}
	}

	fn pick_and_install(&mut self, directories: bool, cx: &mut Context<Self>) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: !directories,
			directories,
			multiple: false,
			prompt: None,
		});
		let engine = self.engine.clone();
		cx.spawn(async move |_, _| {
			if let Ok(Ok(Some(paths))) = paths.await {
				for path in paths {
					engine.send(Command::Install {
						serial: serial.clone(),
						path,
					});
				}
			}
		})
		.detach();
	}

	fn rebuild_rows(&mut self, cx: &App) {
		self.rows.clear();
		self.hidden = 0;
		let Some(info) = &self.info else { return };
		let needle = self.filter.read(cx).value().trim().to_lowercase();
		let usage = match &self.details {
			Some(Ok((_, usage))) => Some(usage),
			_ => None,
		};
		for package in &info.packages {
			if !self.show_system && is_system(&package.name) {
				self.hidden += 1;
				continue;
			}
			let name = self
				.names
				.get(&package.name)
				.cloned()
				.unwrap_or_else(|| package.name.clone());
			if !needle.is_empty()
				&& !name.to_lowercase().contains(&needle)
				&& !package.name.to_lowercase().contains(&needle)
			{
				continue;
			}
			let catalog = self.catalog.as_ref();
			self.rows.push(Row {
				size: usage.map(|u| u.get(&package.name).map_or(0, |u| u.obb + u.data)),
				cover: catalog.and_then(|c| c.thumbnail(&package.name)),
				latest: catalog.and_then(|c| {
					c.games
						.iter()
						.filter(|g| g.package == package.name)
						.filter_map(|g| g.version_code)
						.max()
				}),
				app: AppInfo {
					package: package.name.clone(),
					name,
					version_code: package.version_code,
				},
			});
		}
		match self.sort {
			Sort::Name => self.rows.sort_by_cached_key(|r| r.app.name.to_lowercase()),
			Sort::Size => self
				.rows
				.sort_by_key(|r| std::cmp::Reverse(r.size.unwrap_or(0))),
		}
	}

	fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let info = self.info.as_ref();
		let model = info.map(|i| i.model.replace('_', " ")).unwrap_or_default();
		let mut subtitle = Vec::new();
		if let Some(i) = info {
			subtitle.push(format!("Android {}", i.android_version));
		}
		if let Some(d) = self.device_details()
			&& !d.build.is_empty()
		{
			subtitle.push(d.build.clone());
		}
		if let Some(serial) = &self.serial {
			subtitle.push(serial.clone());
		}
		let apps = info.map_or(0, |i| i.packages.len());
		let section = self.section;
		v_flex()
			.child(
				h_flex()
					.px_5()
					.pt_4()
					.pb_2()
					.gap_3()
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().text_xl().child(model))
							.child(
								div()
									.text_sm()
									.truncate()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(subtitle.join("  /  ")),
							),
					)
					.child(
						Button::new("refresh")
							.icon(Icon::from(AppIcon::Refresh))
							.ghost()
							.tooltip(t!("hs.refresh"))
							.loading(self.details.is_none())
							.on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
					),
			)
			.child(
				TabBar::new("headset-sections")
					.underline()
					.px_4()
					.selected_index(match section {
						Section::Apps => 0,
						Section::Device => 1,
						Section::Backups => 2,
					})
					.child(Tab::new().label(format!("{} {apps}", t!("hs.tab_apps"))))
					.child(Tab::new().label(t!("hs.tab_device").into_owned()))
					.child(Tab::new().label(format!(
						"{} {}",
						t!("hs.tab_backups"),
						self.backups.len()
					)))
					.on_click(cx.listener(|this, ix: &usize, _, cx| {
						this.section = match ix {
							0 => Section::Apps,
							1 => Section::Device,
							_ => Section::Backups,
						};
						cx.notify();
					})),
			)
	}

	fn apps_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let sort_button = |id: &'static str, sort: Sort, label: String| {
			Button::new(id)
				.label(label)
				.ghost()
				.small()
				.selected(self.sort == sort)
				.on_click(cx.listener(move |this, _, _, cx| {
					this.sort = sort;
					cx.notify();
				}))
		};
		h_flex()
			.gap_3()
			.px_5()
			.py_3()
			.child(
				Input::new(&self.filter)
					.prefix(Icon::new(IconName::Search).small())
					.cleanable(true)
					.small()
					.w(px(280.)),
			)
			.child(
				h_flex()
					.gap_1()
					.child(sort_button(
						"sort-size",
						Sort::Size,
						t!("hs.largest").into(),
					))
					.child(sort_button(
						"sort-name",
						Sort::Name,
						t!("hs.by_name").into(),
					)),
			)
			.child(
				Switch::new("show-system")
					.small()
					.checked(self.show_system)
					.label(if self.show_system || self.hidden == 0 {
						t!("hs.system").into_owned()
					} else {
						format!("{} ({})", t!("hs.system"), self.hidden)
					})
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.show_system = *checked;
						cx.notify();
					})),
			)
			.child(div().flex_1())
			.child(
				Button::new("install-folder")
					.label(t!("hs.install_folder"))
					.outline()
					.small()
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_install(true, cx))),
			)
			.child(
				Button::new("install-file")
					.label(t!("hs.install_file"))
					.primary()
					.small()
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_install(false, cx))),
			)
	}

	fn apps_header_row(&self, cx: &App) -> impl IntoElement {
		column_header(cx)
			.child(div().w(px(COVER.0)))
			.child(
				div()
					.flex_1()
					.min_w_0()
					.child(t!("hs.col_name").into_owned()),
			)
			.child(
				div()
					.flex_1()
					.min_w_0()
					.child(t!("hs.col_package").into_owned()),
			)
			.child(
				div()
					.w(px(VERSION_W))
					.text_right()
					.child(t!("hs.col_version").into_owned()),
			)
			.child(
				div()
					.w(px(SIZE_W))
					.text_right()
					.child(t!("hs.col_size").into_owned()),
			)
			.child(div().w(px(TAG_W)))
			.child(div().w(px(MENU_W)))
	}

	fn render_app_rows(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let theme = cx.theme().clone();
		range
			.filter_map(|ix| self.rows.get(ix).map(|row| (ix, row)))
			.map(|(ix, row)| {
				let cover = match &row.cover {
					Some(path) => img(path.clone())
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.object_fit(ObjectFit::Cover)
						.into_any_element(),
					None => div()
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.bg(theme.muted)
						.flex()
						.items_center()
						.justify_center()
						.text_color(theme.muted_foreground)
						.child(initial(&row.app.name))
						.into_any_element(),
				};
				let tag = match (row.app.version_code, row.latest) {
					_ if self.copies.contains(&row.app.package) => {
						Some(soft_tag(theme.info, t!("hs.copy").into_owned()))
					}
					(Some(have), Some(latest)) if have >= latest => {
						Some(soft_tag(theme.success, t!("hs.latest").into_owned()))
					}
					(_, Some(_)) => Some(soft_tag(theme.warning, t!("hs.update").into_owned())),
					_ => None,
				};
				let version = row
					.app
					.version_code
					.map_or_else(String::new, |v| format!("v{v}"));
				let size = row.size.map_or_else(|| "...".to_owned(), format_bytes);
				let has_name = row.app.name != row.app.package;

				table_row(("app-row", ix), ROW_HEIGHT, &theme)
					.child(cover)
					.child(
						div()
							.flex_1()
							.min_w_0()
							.truncate()
							.child(row.app.name.clone()),
					)
					.child(
						div()
							.flex_1()
							.min_w_0()
							.truncate()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.when(has_name, |d| d.child(row.app.package.clone())),
					)
					.child(
						div()
							.w(px(VERSION_W))
							.text_right()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(version),
					)
					.child(
						div()
							.w(px(SIZE_W))
							.text_right()
							.text_sm()
							.mono(cx)
							.child(size),
					)
					.child(h_flex().w(px(TAG_W)).justify_end().children(tag))
					.child(self.app_menu(ix, &row.app))
					.into_any_element()
			})
			.collect()
	}

	fn app_menu(&self, ix: usize, app: &AppInfo) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let app = app.clone();
		Button::new(("app-menu", ix))
			.icon(Icon::from(AppIcon::Dots))
			.ghost()
			.small()
			.w(px(MENU_W))
			.dropdown_menu(move |menu, _, _| {
				let launch = Command::LaunchApp {
					serial: serial.clone(),
					package: app.package.clone(),
				};
				let stop = Command::StopApp {
					serial: serial.clone(),
					package: app.package.clone(),
				};
				let back_up = Command::BackUpSaves {
					serial: serial.clone(),
					app: app.clone(),
				};
				let (uninstall_serial, uninstall_app) = (serial.clone(), app.clone());
				let uninstall_engine = engine.clone();
				menu.item(send_item(&engine, t!("hs.launch").into(), launch))
					.item(send_item(&engine, t!("hs.stop").into(), stop))
					.separator()
					.item(send_item(&engine, t!("hs.back_up").into(), back_up))
					.separator()
					.item(PopupMenuItem::new(t!("hs.uninstall_button")).on_click(
						move |_, window, cx| {
							open_uninstall(
								uninstall_engine.clone(),
								uninstall_serial.clone(),
								uninstall_app.clone(),
								window,
								cx,
							)
						},
					))
			})
	}

	fn apps_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
		self.rebuild_rows(cx);
		let body = if self.rows.is_empty() {
			centered(t!("hs.nothing").into_owned(), cx)
		} else {
			uniform_list(
				"headset-apps",
				self.rows.len(),
				cx.processor(Self::render_app_rows),
			)
			.flex_1()
			.into_any_element()
		};
		v_flex()
			.size_full()
			.child(self.apps_toolbar(cx))
			.child(self.apps_header_row(cx))
			.child(body)
			.into_any_element()
	}

	fn device_section(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let details = self.device_details();
		let info = self.info.as_ref();

		let mut facts: Vec<(String, String, bool)> = Vec::new();
		if let Some(i) = info {
			facts.push((t!("dev.model").into(), i.model.replace('_', " "), false));
			facts.push((t!("dev.android").into(), i.android_version.clone(), true));
			if let Some(b) = i.battery {
				let mut text = format!("{}%", b.level);
				if let Some(temp) = details.and_then(|d| d.battery_temperature) {
					text.push_str(&format!(", {temp:.0}C"));
				}
				facts.push((t!("hs.battery").into(), text, false));
			}
			if let Some(s) = i.storage {
				facts.push((
					t!("hs.storage").into(),
					t!(
						"dev.free_of",
						free = format_bytes(s.free_bytes),
						total = format_bytes(s.total_bytes)
					)
					.into_owned(),
					false,
				));
			}
		}
		if let Some(d) = details {
			if !d.codename.is_empty() {
				facts.push((t!("hs.codename").into(), d.codename.clone(), true));
			}
			if !d.build.is_empty() {
				facts.push((t!("hs.build").into(), d.build.clone(), true));
			}
			if let (Some(total), Some(free)) = (d.memory_total, d.memory_available) {
				facts.push((
					t!("hs.memory").into(),
					t!(
						"dev.free_of",
						free = format_bytes(free),
						total = format_bytes(total)
					)
					.into_owned(),
					false,
				));
			}
			if let Some(uptime) = d.uptime {
				facts.push((t!("hs.uptime").into(), format_uptime(uptime), false));
			}
			facts.push((
				t!("hs.wifi").into(),
				d.wifi_ip
					.clone()
					.unwrap_or_else(|| t!("hs.not_connected").into_owned()),
				d.wifi_ip.is_some(),
			));
			if let Some(mtp) = d.mtp {
				facts.push((
					t!("hs.usb").into(),
					if mtp {
						t!("hs.usb_mtp")
					} else {
						t!("hs.usb_charging")
					}
					.into_owned(),
					false,
				));
			}
			facts.push((
				t!("hs.wireless_adb").into(),
				d.adb_tcp_port.map_or_else(
					|| t!("hs.off").into_owned(),
					|port| t!("hs.on_port", port = port).into_owned(),
				),
				false,
			));
		}

		let fact_rows = facts.into_iter().map(|(label, value, mono)| {
			h_flex()
				.gap_4()
				.py_1()
				.child(
					div()
						.w(px(200.))
						.flex_shrink_0()
						.text_color(theme.muted_foreground)
						.child(label),
				)
				.child(div().min_w_0().when(mono, |d| d.mono(cx)).child(value))
		});

		let has_wifi = details.is_some_and(|d| d.wifi_ip.is_some());
		let mtp = details.and_then(|d| d.mtp);
		let tools = h_flex()
			.gap_2()
			.flex_wrap()
			.child(
				Button::new("wireless")
					.label(t!("hs.enable_wireless"))
					.outline()
					.small()
					.disabled(!has_wifi)
					.tooltip(if has_wifi {
						t!("hs.enable_wireless_hint")
					} else {
						t!("hs.no_wifi")
					})
					.on_click(cx.listener(|this, _, _, _| this.send(Command::EnableWireless))),
			)
			.child(
				Button::new("mtp")
					.label(if mtp == Some(true) {
						t!("hs.mtp_off")
					} else {
						t!("hs.mtp_on")
					})
					.outline()
					.small()
					.disabled(mtp.is_none())
					.tooltip(t!("hs.mtp_hint"))
					.on_click(cx.listener(move |this, _, _, cx| {
						let enabled = mtp != Some(true);
						this.send(|serial| Command::SetMtp { serial, enabled });
						// The headset only reports the new mode after the USB reset.
						if let Some(Ok((d, _))) = &mut this.details {
							d.mtp = Some(enabled);
						}
						cx.notify();
					})),
			)
			.child(
				Button::new("screenshot")
					.label(t!("hs.screenshot"))
					.outline()
					.small()
					.tooltip(t!("hs.screenshot_hint"))
					.on_click(cx.listener(|this, _, _, _| this.send(Command::Screenshot))),
			)
			.child(self.reboot_menu());

		let current = |tweak: Tweak| -> Option<String> {
			let d = details?;
			match tweak {
				Tweak::RefreshRate => d.refresh_rate.clone(),
				Tweak::CpuLevel => d.cpu_level.clone(),
				Tweak::GpuLevel => d.gpu_level.clone(),
			}
		};
		let tweaks = h_flex().gap_4().flex_wrap().children(
			[
				(Tweak::RefreshRate, t!("hs.refresh_rate")),
				(Tweak::CpuLevel, t!("hs.cpu_level")),
				(Tweak::GpuLevel, t!("hs.gpu_level")),
			]
			.into_iter()
			.map(|(tweak, label)| {
				h_flex()
					.gap_2()
					.child(
						div()
							.text_color(theme.muted_foreground)
							.child(label.into_owned()),
					)
					.child(self.tweak_menu(tweak, current(tweak)))
			}),
		);

		div()
			.id("device-section")
			.size_full()
			.overflow_y_scroll()
			.child(
				v_flex()
					.gap_4()
					.p_5()
					.child(card(
						t!("hs.info").into(),
						v_flex().children(fact_rows),
						&theme,
					))
					.child(card(t!("hs.tools").into(), tools, &theme))
					.child(card(t!("hs.performance").into(), tweaks, &theme)),
			)
			.into_any_element()
	}

	fn reboot_menu(&self) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		Button::new("reboot")
			.label(t!("hs.restart_menu"))
			.outline()
			.small()
			.dropdown_caret(true)
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for (mode, label) in [
					(RebootMode::Normal, t!("hs.restart")),
					(RebootMode::PowerOff, t!("hs.power_off")),
					(RebootMode::Recovery, t!("hs.restart_recovery")),
					(RebootMode::Bootloader, t!("hs.restart_bootloader")),
				] {
					let engine = engine.clone();
					let serial = serial.clone();
					let label = label.into_owned();
					menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
						move |_, window, cx| {
							confirm(
								&engine,
								Confirm {
									title: t!("hs.reboot_title", action = label).into_owned(),
									text: t!("hs.reboot_text").into_owned(),
									ok: label.clone(),
									danger: mode != RebootMode::Normal,
								},
								Command::Reboot {
									serial: serial.clone(),
									mode,
								},
								window,
								cx,
							);
						},
					));
				}
				menu
			})
	}

	fn tweak_menu(&self, tweak: Tweak, current: Option<String>) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let default = t!("hs.default").into_owned();
		let shown = current
			.clone()
			.filter(|c| !c.is_empty())
			.unwrap_or_else(|| default.clone());
		Button::new(tweak.property())
			.label(shown)
			.outline()
			.small()
			.dropdown_caret(true)
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for choice in tweak.choices() {
					let text = if choice.is_empty() {
						default.clone()
					} else {
						(*choice).to_owned()
					};
					let checked = current.as_deref().unwrap_or("") == *choice;
					menu = menu.item(
						send_item(
							&engine,
							text,
							Command::SetTweak {
								serial: serial.clone(),
								tweak,
								value: (*choice).to_owned(),
							},
						)
						.checked(checked),
					);
				}
				menu
			})
	}

	/// Installed apps a backup can go into: renamed copies first, then by name.
	fn restore_targets(&self) -> Vec<(bool, String, String)> {
		let mut targets: Vec<(bool, String, String)> = self
			.info
			.iter()
			.flat_map(|i| i.packages.iter())
			.filter(|p| !is_system(&p.name))
			.map(|p| {
				let name = self
					.names
					.get(&p.name)
					.cloned()
					.unwrap_or_else(|| p.name.clone());
				(self.copies.contains(&p.name), name, p.name.clone())
			})
			.collect();
		targets.sort_by_key(|(copy, name, _)| (!*copy, name.to_lowercase()));
		targets
	}

	fn restore_into_menu(
		&self,
		ix: usize,
		backup: &Backup,
		targets: Rc<Vec<(bool, String, String)>>,
	) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let backup = backup.clone();
		let any = targets.iter().any(|(_, _, p)| p != &backup.meta.package);
		Button::new(("restore-into", ix))
			.label(t!("hs.restore_into"))
			.ghost()
			.xsmall()
			.dropdown_caret(true)
			.disabled(!any || self.serial.is_none())
			.tooltip(t!("hs.restore_into_hint"))
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				menu = menu.scrollable(true).max_h(px(320.));
				for (copy, name, package) in targets.iter() {
					if package == &backup.meta.package {
						continue;
					}
					let label = if *copy {
						t!("hs.copy_entry", name = name, package = package).into_owned()
					} else {
						format!("{name} ({package})")
					};
					let engine = engine.clone();
					let serial = serial.clone();
					let backup = backup.clone();
					let (name, package) = (name.clone(), package.clone());
					menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
						confirm(
							&engine,
							Confirm {
								title: t!("hs.restore_into_title").into_owned(),
								text: t!(
									"hs.restore_into_text",
									backup = backup.meta.name,
									date = format_date(backup.meta.created),
									name = name,
									package = package
								)
								.into_owned(),
								ok: t!("hs.restore").into_owned(),
								danger: false,
							},
							Command::RestoreBackup {
								serial: serial.clone(),
								backup: backup.clone(),
								package: package.clone(),
							},
							window,
							cx,
						);
					}));
				}
				menu
			})
	}

	fn backups_section(&self, cx: &mut Context<Self>) -> AnyElement {
		if self.backups.is_empty() {
			return centered(t!("hs.no_backups").into_owned(), cx);
		}
		let theme = cx.theme().clone();
		let header = column_header(cx)
			.child(
				div()
					.flex_1()
					.min_w_0()
					.child(t!("hs.col_name").into_owned()),
			)
			.child(
				div()
					.flex_1()
					.min_w_0()
					.child(t!("hs.col_package").into_owned()),
			)
			.child(div().w(px(DATE_W)).child(t!("hs.col_date").into_owned()))
			.child(
				div()
					.w(px(SIZE_W))
					.text_right()
					.child(t!("hs.col_size").into_owned()),
			)
			.child(div().w(px(BACKUP_ACTIONS_W)));
		let targets = Rc::new(self.restore_targets());
		let rows = self.backups.iter().enumerate().map(|(ix, backup)| {
			let meta = &backup.meta;
			let date = format_date(meta.created);
			let restore = {
				let engine = self.engine.clone();
				let serial = self.serial.clone();
				let backup = backup.clone();
				let title = t!("hs.restore_title", name = meta.name).into_owned();
				Button::new(("restore", ix))
					.label(t!("hs.restore"))
					.outline()
					.xsmall()
					.disabled(serial.is_none())
					.tooltip(t!("hs.restore_hint"))
					.on_click(move |_, window, cx| {
						if let Some(serial) = &serial {
							confirm(
								&engine,
								Confirm {
									title: title.clone(),
									text: t!("hs.restore_hint").into_owned(),
									ok: t!("hs.restore").into_owned(),
									danger: false,
								},
								Command::RestoreBackup {
									serial: serial.clone(),
									backup: backup.clone(),
									package: backup.meta.package.clone(),
								},
								window,
								cx,
							);
						}
					})
			};
			let delete = {
				let engine = self.engine.clone();
				let backup = backup.clone();
				let text = t!("hs.delete_backup_text", name = meta.name, date = date).into_owned();
				Button::new(("delete", ix))
					.icon(Icon::new(IconName::Delete))
					.ghost()
					.xsmall()
					.tooltip(t!("common.delete"))
					.on_click(move |_, window, cx| {
						confirm(
							&engine,
							Confirm {
								title: t!("hs.delete_backup_title").into_owned(),
								text: text.clone(),
								ok: t!("common.delete").into_owned(),
								danger: true,
							},
							Command::DeleteBackup(backup.clone()),
							window,
							cx,
						);
					})
			};
			table_row(("backup-row", ix), ROW_HEIGHT, &theme)
				.child(div().flex_1().min_w_0().truncate().child(meta.name.clone()))
				.child(
					div()
						.flex_1()
						.min_w_0()
						.truncate()
						.text_sm()
						.text_color(theme.muted_foreground)
						.mono(cx)
						.child(meta.package.clone()),
				)
				.child(div().w(px(DATE_W)).text_sm().mono(cx).child(date))
				.child(
					div()
						.w(px(SIZE_W))
						.text_right()
						.text_sm()
						.mono(cx)
						.child(format_bytes(meta.bytes)),
				)
				.child(
					h_flex()
						.w(px(BACKUP_ACTIONS_W))
						.gap_1()
						.justify_end()
						.child(restore)
						.child(self.restore_into_menu(ix, backup, targets.clone()))
						.child({
							let dir = backup.dir.clone();
							Button::new(("open", ix))
								.icon(Icon::new(IconName::FolderOpen))
								.ghost()
								.xsmall()
								.tooltip(t!("common.open"))
								.on_click(move |_, _, _| open_folder(&dir))
						})
						.child(delete),
				)
		});
		v_flex()
			.size_full()
			.pt_2()
			.child(header)
			.child(
				div()
					.id("backups")
					.flex_1()
					.overflow_y_scroll()
					.children(rows),
			)
			.into_any_element()
	}
}

impl Render for HeadsetView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		if self.serial.is_none() {
			return centered(t!("dev.connect_hint").into_owned(), cx);
		}
		if self.info.is_none() {
			return centered(t!("hs.loading").into_owned(), cx);
		}
		let body = match self.section {
			Section::Apps => self.apps_section(cx),
			Section::Device => self.device_section(cx),
			Section::Backups => self.backups_section(cx),
		};
		v_flex()
			.size_full()
			.child(self.header(cx))
			.child(div().flex_1().min_h_0().child(body))
			.into_any_element()
	}
}

/// System apps clutter the list; the switch in the toolbar shows them.
fn is_system(package: &str) -> bool {
	["com.meta.", "com.oculus.", "com.android.", "android."]
		.iter()
		.any(|prefix| package.starts_with(prefix))
}

fn initial(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map(|c| c.to_uppercase().to_string())
		.unwrap_or_default()
}

fn format_uptime(uptime: Duration) -> String {
	let mins = uptime.as_secs() / 60;
	match (mins / 1440, (mins / 60) % 24, mins % 60) {
		(0, 0, m) => t!("hs.uptime_m", m = m).into_owned(),
		(0, h, m) => t!("hs.uptime_hm", h = h, m = m).into_owned(),
		(d, h, _) => t!("hs.uptime_dh", d = d, h = h).into_owned(),
	}
}

/// Uninstall with the two choices the old dialog had: back up saves first, delete OBB files.
fn open_uninstall(
	engine: Rc<Engine>,
	serial: String,
	app: AppInfo,
	window: &mut Window,
	cx: &mut App,
) {
	let back_up = Rc::new(Cell::new(true));
	let remove_obb = Rc::new(Cell::new(true));
	window.open_dialog(cx, move |dialog, _, _| {
		let toggle = |id: &'static str, label: String, value: &Rc<Cell<bool>>| {
			let value = value.clone();
			Checkbox::new(id)
				.label(label)
				.checked(value.get())
				.on_click(move |checked: &bool, window, _| {
					value.set(*checked);
					window.refresh();
				})
		};
		let uninstall = {
			let (engine, serial, app) = (engine.clone(), serial.clone(), app.clone());
			let (back_up, remove_obb) = (back_up.clone(), remove_obb.clone());
			move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
				engine.send(Command::Uninstall {
					serial: serial.clone(),
					app: app.clone(),
					back_up: back_up.get(),
					remove_obb: remove_obb.get(),
				});
				window.close_dialog(cx);
			}
		};
		dialog
			.title(t!("hs.uninstall_title", name = app.name).into_owned())
			.w(px(460.))
			.child(
				v_flex()
					.gap_3()
					.child(toggle("back-up", t!("hs.backup_first").into(), &back_up))
					.child(toggle(
						"remove-obb",
						t!("hs.delete_obb").into(),
						&remove_obb,
					)),
			)
			.footer(
				h_flex()
					.w_full()
					.justify_end()
					.gap_2()
					.child(
						Button::new("cancel")
							.label(t!("common.cancel"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("uninstall")
							.label(t!("hs.uninstall"))
							.danger()
							.on_click(uninstall),
					),
			)
	});
}
