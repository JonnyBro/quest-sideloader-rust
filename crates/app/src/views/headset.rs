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
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Sizable, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::{
	AppUsage, BatteryHealth, ControllerSide, DeviceDetails, DeviceInfo, PowerSource, RebootMode,
	Tweak,
};
use quest_sideloader_core::catalog::Catalog;
use quest_sideloader_core::saves::Backup;
use quest_sideloader_core::{AppInfo, Command, Engine, format_bytes};

use serde::{Deserialize, Serialize};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::ui_state::HeadsetPrefs;
use crate::widgets::{
	Confirm, card, centered, column_header, confirm, content_width, fit_columns, format_date,
	open_folder, send_item, soft_tag, table_row, update_tip, with_tip,
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
const NAME_MIN: f32 = 180.;
/// The package column is flexible; this is what it needs to be worth showing.
const PACKAGE_MIN: f32 = 200.;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Sort {
	#[default]
	Name,
	Package,
	Version,
	Size,
}

impl Sort {
	/// Text sorts A to Z; numbers start with the biggest.
	fn starts_descending(self) -> bool {
		matches!(self, Sort::Version | Sort::Size)
	}
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
	usage: Option<AppUsage>,
	cover: Option<PathBuf>,
	/// The newest version the catalog has.
	latest: Option<u64>,
	/// Unix seconds of the last install or update.
	installed: Option<u64>,
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
	descending: bool,
	show_system: bool,
	rows: Vec<Row>,
	/// Rows hidden because they are system apps.
	hidden: usize,
	/// Renamed copies made here.
	copies: HashSet<String>,
	/// Which app columns fit: package, version, size, badge.
	app_cols: [bool; 4],
	/// Which backup columns fit: package, date, size.
	backup_cols: [bool; 3],
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
			sort: Sort::Name,
			descending: false,
			show_system: false,
			rows: Vec::new(),
			hidden: 0,
			copies: HashSet::new(),
			app_cols: [true; 4],
			backup_cols: [true; 3],
			_filter_changes: subscription,
		}
	}

	pub fn set_prefs(&mut self, prefs: &HeadsetPrefs) {
		self.sort = prefs.sort;
		self.descending = prefs.descending;
		self.show_system = prefs.show_system;
	}

	pub fn prefs(&self) -> HeadsetPrefs {
		HeadsetPrefs {
			sort: self.sort,
			descending: self.descending,
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
				usage: usage.map(|u| u.get(&package.name).copied().unwrap_or_default()),
				cover: catalog.and_then(|c| c.thumbnail(&package.name)),
				latest: catalog.and_then(|c| {
					c.games
						.iter()
						.filter(|g| g.package == package.name)
						.filter_map(|g| g.version_code)
						.max()
				}),
				installed: package.installed,
				app: AppInfo {
					package: package.name.clone(),
					name,
					version_code: package.version_code,
				},
			});
		}
		let (sort, descending) = (self.sort, self.descending);
		self.rows.sort_by_cached_key(|r| r.app.name.to_lowercase());
		// Stable, so ties keep the A to Z order from above.
		self.rows.sort_by(|a, b| {
			let order = match sort {
				Sort::Name => a.app.name.to_lowercase().cmp(&b.app.name.to_lowercase()),
				Sort::Package => a.app.package.cmp(&b.app.package),
				Sort::Version => a.app.version_code.cmp(&b.app.version_code),
				Sort::Size => size_of(a).cmp(&size_of(b)),
			};
			if descending { order.reverse() } else { order }
		});
	}

	fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let info = self.info.as_ref();
		let model = info.map(|i| i.model.replace('_', " ")).unwrap_or_default();
		// (id, text, what it is); hovering a part says what it is.
		let mut subtitle: Vec<(&'static str, String, String)> = Vec::new();
		if let Some(i) = info {
			subtitle.push((
				"hs-android",
				format!("Android {}", i.android_version),
				t!("hs.tip_android").into_owned(),
			));
		}
		if let Some(v) = self
			.device_details()
			.and_then(|d| d.horizon_version.as_ref())
		{
			subtitle.push((
				"hs-horizon",
				format!("Horizon OS {v}"),
				t!("hs.tip_horizon").into_owned(),
			));
		}
		if let Some(serial) = &self.serial {
			subtitle.push((
				"hs-serial",
				serial.clone(),
				t!("hs.tip_serial").into_owned(),
			));
		}
		let separator = theme.muted_foreground.opacity(0.5);
		let parts = subtitle
			.into_iter()
			.enumerate()
			.map(|(ix, (id, text, tip))| {
				h_flex()
					.gap_2()
					.min_w_0()
					.when(ix > 0, |d| d.child(div().text_color(separator).child("/")))
					.child(
						div()
							.id(id)
							.truncate()
							.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
							.child(text),
					)
			});
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
								h_flex()
									.gap_2()
									.min_w_0()
									.text_sm()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.children(parts),
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

	/// Picking the current column again flips the order.
	fn set_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
		if self.sort == sort {
			self.descending = !self.descending;
		} else {
			self.sort = sort;
			self.descending = sort.starts_descending();
		}
		cx.notify();
	}

	fn apps_header_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let c = self.app_cols;
		let theme = cx.theme().clone();
		let (current, descending) = (self.sort, self.descending);
		let column = |sort: Sort, label: String, right: bool| {
			let arrow = (current == sort).then(|| {
				Icon::new(if descending {
					IconName::ChevronDown
				} else {
					IconName::ChevronUp
				})
				.xsmall()
			});
			h_flex()
				.id(SharedString::from(format!("hs-col-{sort:?}")))
				.gap_1()
				.whitespace_nowrap()
				.cursor_pointer()
				.when(right, |d| d.justify_end())
				.when(current == sort, |d| d.text_color(theme.foreground))
				.hover(|s| s.text_color(theme.foreground))
				.on_click(cx.listener(move |this, _, _, cx| this.set_sort(sort, cx)))
				.child(div().min_w_0().truncate().child(label))
				.children(arrow)
		};
		column_header(cx)
			.child(div().w(px(COVER.0)).flex_shrink_0())
			.child(
				column(Sort::Name, t!("hs.col_name").into(), false)
					.flex_1()
					.min_w(px(NAME_MIN)),
			)
			.when(c[0], |h| {
				h.child(
					column(Sort::Package, t!("hs.col_package").into(), false)
						.flex_1()
						.min_w_0(),
				)
			})
			.when(c[1], |h| {
				h.child(
					column(Sort::Version, t!("hs.col_version").into(), true)
						.w(px(VERSION_W))
						.flex_shrink_0(),
				)
			})
			.when(c[2], |h| {
				h.child(
					column(Sort::Size, t!("hs.col_size").into(), true)
						.w(px(SIZE_W))
						.flex_shrink_0(),
				)
			})
			.when(c[3], |h| h.child(div().w(px(TAG_W)).flex_shrink_0()))
			.child(div().w(px(MENU_W)).flex_shrink_0())
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
						Some(soft_tag(theme.info, t!("hs.copy").into_owned()).into_any_element())
					}
					(Some(have), Some(latest)) if have >= latest => Some(
						soft_tag(theme.success, t!("hs.latest").into_owned()).into_any_element(),
					),
					(have, Some(latest)) => Some(with_tip(
						("update-tip", ix),
						soft_tag(theme.warning, t!("hs.update").into_owned()),
						update_tip(have, Some(latest), row.installed),
					)),
					_ => None,
				};
				let version = row
					.app
					.version_code
					.map_or_else(String::new, |v| format!("v{v}"));
				let size = row
					.usage
					.map_or_else(|| "...".to_owned(), |u| format_bytes(u.total()));
				let breakdown = row.usage.map(|u| {
					t!(
						"hs.size_breakdown",
						apk = format_bytes(u.apk),
						obb = format_bytes(u.obb),
						data = format_bytes(u.data)
					)
					.into_owned()
				});
				let has_name = row.app.name != row.app.package;

				let c = self.app_cols;
				table_row(("app-row", ix), ROW_HEIGHT, &theme)
					.child(cover)
					.child(
						div()
							.flex_1()
							.min_w(px(NAME_MIN))
							.truncate()
							.child(row.app.name.clone()),
					)
					.when(c[0], |r| {
						r.child(
							div()
								.flex_1()
								.min_w_0()
								.truncate()
								.text_sm()
								.text_color(theme.muted_foreground)
								.mono(cx)
								.when(has_name, |d| d.child(row.app.package.clone())),
						)
					})
					.when(c[1], |r| {
						r.child(
							div()
								.w(px(VERSION_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.text_color(theme.muted_foreground)
								.mono(cx)
								.child(version),
						)
					})
					.when(c[2], |r| {
						r.child(
							div()
								.id(("app-size", ix))
								.w(px(SIZE_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.mono(cx)
								.when_some(breakdown, |d, text| {
									d.tooltip(move |window, cx| {
										Tooltip::new(text.clone()).build(window, cx)
									})
								})
								.child(size),
						)
					})
					.when(c[3], |r| {
						r.child(
							h_flex()
								.w(px(TAG_W))
								.flex_shrink_0()
								.justify_end()
								.children(tag),
						)
					})
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

		// (label, value, monospace)
		type Facts = Vec<(String, String, bool)>;
		let mut system: Facts = Vec::new();
		let mut power: Facts = Vec::new();
		let mut network: Facts = Vec::new();
		if let Some(i) = info {
			system.push((t!("dev.model").into(), i.model.replace('_', " "), false));
		}
		if let Some(d) = details
			&& !d.codename.is_empty()
		{
			system.push((t!("hs.codename").into(), d.codename.clone(), true));
		}
		if let Some(i) = info {
			system.push((t!("dev.android").into(), i.android_version.clone(), true));
		}
		if let Some(d) = details {
			if let Some(v) = &d.horizon_version {
				system.push((t!("hs.horizon").into(), v.clone(), true));
			}
			if let Some(n) = &d.build_number {
				system.push((t!("hs.build_number").into(), n.clone(), true));
			}
			if !d.build.is_empty() {
				system.push((t!("hs.build").into(), d.build.clone(), true));
			}
			if let Some(p) = &d.security_patch {
				system.push((t!("hs.security_patch").into(), p.clone(), true));
			}
			if let Some(size) = &d.display {
				system.push((t!("hs.display").into(), size.clone(), true));
			}
			if let (Some(total), Some(free)) = (d.memory_total, d.memory_available) {
				system.push((
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
		}
		if let Some(s) = info.and_then(|i| i.storage) {
			system.push((
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
		if let Some(uptime) = details.and_then(|d| d.uptime) {
			system.push((t!("hs.uptime").into(), format_uptime(uptime), false));
		}

		if let Some(b) = info.and_then(|i| i.battery) {
			let mut text = format!("{}%", b.level);
			if b.charging {
				text.push_str(&format!(", {}", t!("hs.charging")));
			}
			power.push((t!("hs.battery").into(), text, false));
		}
		if let Some(d) = details {
			if let Some(p) = d.power {
				let mut text = match p.source {
					None => t!("hs.source_battery"),
					Some(PowerSource::Ac) => t!("hs.source_ac"),
					Some(PowerSource::Usb) => t!("hs.source_usb"),
					Some(PowerSource::Wireless) => t!("hs.source_wireless"),
					Some(PowerSource::Dock) => t!("hs.source_dock"),
				}
				.into_owned();
				if p.weak_charger {
					text.push_str(&format!(", {}", t!("hs.weak_charger")));
				}
				power.push((t!("hs.power_source").into(), text, false));
				if let Some(health) = p.health {
					let text = match health {
						BatteryHealth::Good => t!("hs.health_good"),
						BatteryHealth::Overheat => t!("hs.health_overheat"),
						BatteryHealth::Dead => t!("hs.health_dead"),
						BatteryHealth::OverVoltage => t!("hs.health_overvoltage"),
						BatteryHealth::Cold => t!("hs.health_cold"),
						BatteryHealth::Other => t!("hs.health_other"),
					};
					power.push((t!("hs.battery_health").into(), text.into_owned(), false));
				}
				if let Some(mv) = p.voltage_mv {
					power.push((
						t!("hs.voltage").into(),
						format!("{:.2} V", f64::from(mv) / 1000.0),
						true,
					));
				}
			}
			if let Some(temp) = d.battery_temperature {
				power.push((t!("hs.battery_temp").into(), format!("{temp:.0}C"), true));
			}
			if let Some(temp) = d.soc_temperature {
				power.push((t!("hs.soc_temp").into(), format!("{temp:.0}C"), true));
			}

			match &d.wifi {
				Some(w) => {
					network.push((t!("hs.wifi").into(), w.ssid.clone(), false));
					network.push((
						t!("hs.signal").into(),
						format!("{} dBm, {}", w.rssi, signal_quality(w.rssi)),
						false,
					));
					let mut link = Vec::new();
					if let Some(mbps) = w.link_mbps {
						link.push(t!("hs.mbps", speed = mbps).into_owned());
					}
					if let Some(band) = w.frequency_mhz.and_then(wifi_band) {
						link.push(band.to_owned());
					}
					if let Some(standard) = w.standard {
						link.push(format!("Wi-Fi {standard}"));
					}
					if !link.is_empty() {
						network.push((t!("hs.link").into(), link.join(", "), false));
					}
				}
				None => network.push((
					t!("hs.wifi").into(),
					t!("hs.not_connected").into_owned(),
					false,
				)),
			}
			if let Some(ip) = &d.wifi_ip {
				network.push((t!("hs.ip").into(), ip.clone(), true));
			}
			if let Some(mtp) = d.mtp {
				network.push((
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
			network.push((
				t!("hs.wireless_adb").into(),
				d.adb_tcp_port.map_or_else(
					|| t!("hs.off").into_owned(),
					|port| t!("hs.on_port", port = port).into_owned(),
				),
				false,
			));
		}

		let fact_rows = |facts: Facts| {
			v_flex().children(facts.into_iter().map(|(label, value, mono)| {
				h_flex()
					.gap_4()
					.py_1()
					.child(
						div()
							.w(px(170.))
							.flex_shrink_0()
							.text_color(theme.muted_foreground)
							.child(label),
					)
					.child(div().min_w_0().when(mono, |d| d.mono(cx)).child(value))
			}))
		};
		let controllers = details.map(|d| d.controllers.clone()).unwrap_or_default();
		let controller_rows = if controllers.is_empty() {
			div()
				.text_color(theme.muted_foreground)
				.child(t!("hs.no_controllers").into_owned())
				.into_any_element()
		} else {
			let facts: Facts = controllers
				.iter()
				.map(|c| {
					let label = match c.side {
						ControllerSide::Left => t!("hs.left_controller"),
						ControllerSide::Right => t!("hs.right_controller"),
					};
					let mut value = c
						.battery
						.map_or_else(|| "?".to_owned(), |b| format!("{b}%"));
					if let Some(fw) = &c.firmware {
						value.push_str(&format!(", {}", t!("hs.firmware", version = fw)));
					}
					(label.into_owned(), value, false)
				})
				.collect();
			v_flex()
				.gap_2()
				.child(fact_rows(facts))
				.child(
					div()
						.text_sm()
						.text_color(theme.muted_foreground)
						.child(t!("hs.controllers_hint").into_owned()),
				)
				.into_any_element()
		};
		let info_cards = h_flex()
			.gap_4()
			.flex_wrap()
			.items_start()
			// Both columns are paddingless wrappers: flex grows from a zero basis plus
			// padding, so a padded card here would come out wider than the other column.
			.child(v_flex().flex_1().min_w(px(380.)).child(card(
				t!("hs.sec_system").into(),
				fact_rows(system),
				&theme,
			)))
			.child(
				v_flex()
					.gap_4()
					.flex_1()
					.min_w(px(380.))
					.when(!power.is_empty(), |d| {
						d.child(card(t!("hs.sec_power").into(), fact_rows(power), &theme))
					})
					.child(card(
						t!("hs.sec_controllers").into(),
						controller_rows,
						&theme,
					))
					.when(!network.is_empty(), |d| {
						d.child(card(
							t!("hs.sec_network").into(),
							fact_rows(network),
							&theme,
						))
					}),
			);

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

		v_flex()
			.size_full()
			.child(
				div().px_5().pt_5().pb_4().child(
					h_flex()
						.gap_4()
						.flex_wrap()
						.items_stretch()
						.child(
							card(t!("hs.tools").into(), tools, &theme)
								.flex_1()
								.min_w(px(380.)),
						)
						.child(
							card(t!("hs.performance").into(), tweaks, &theme)
								.flex_1()
								.min_w(px(380.)),
						),
				),
			)
			.child(
				div()
					.id("device-section")
					.flex_1()
					.min_h_0()
					.overflow_y_scroll()
					.child(v_flex().gap_4().px_5().pb_5().child(info_cards)),
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
			.outline()
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
		let c = self.backup_cols;
		let header = column_header(cx)
			.child(
				div()
					.flex_1()
					.min_w(px(NAME_MIN))
					.child(t!("hs.col_name").into_owned()),
			)
			.when(c[0], |h| {
				h.child(
					div()
						.flex_1()
						.min_w_0()
						.child(t!("hs.col_package").into_owned()),
				)
			})
			.when(c[1], |h| {
				h.child(
					div()
						.w(px(DATE_W))
						.flex_shrink_0()
						.child(t!("hs.col_date").into_owned()),
				)
			})
			.when(c[2], |h| {
				h.child(
					div()
						.w(px(SIZE_W))
						.flex_shrink_0()
						.text_right()
						.child(t!("hs.col_size").into_owned()),
				)
			})
			.child(div().w(px(BACKUP_ACTIONS_W)).flex_shrink_0());
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
				.child(
					div()
						.flex_1()
						.min_w(px(NAME_MIN))
						.truncate()
						.child(meta.name.clone()),
				)
				.when(c[0], |r| {
					r.child(
						div()
							.flex_1()
							.min_w_0()
							.truncate()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(meta.package.clone()),
					)
				})
				.when(c[1], |r| {
					r.child(
						div()
							.w(px(DATE_W))
							.flex_shrink_0()
							.text_sm()
							.mono(cx)
							.child(date),
					)
				})
				.when(c[2], |r| {
					r.child(
						div()
							.w(px(SIZE_W))
							.flex_shrink_0()
							.text_right()
							.text_sm()
							.mono(cx)
							.child(format_bytes(meta.bytes)),
					)
				})
				.child(
					h_flex()
						.w(px(BACKUP_ACTIONS_W))
						.flex_shrink_0()
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
	fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		if self.serial.is_none() {
			return centered(t!("dev.connect_hint").into_owned(), cx);
		}
		if self.info.is_none() {
			return centered(t!("hs.loading").into_owned(), cx);
		}
		let body = match self.section {
			Section::Apps => {
				// The badge stays longest, the package goes first.
				let fit = fit_columns(
					content_width(window, 0.),
					40. + COVER.0 + 16. + NAME_MIN + 16. + MENU_W,
					&[(PACKAGE_MIN, 0), (VERSION_W, 1), (SIZE_W, 2), (TAG_W, 3)],
				);
				self.app_cols = [fit[0], fit[1], fit[2], fit[3]];
				self.apps_section(cx)
			}
			Section::Device => self.device_section(cx),
			Section::Backups => {
				let fit = fit_columns(
					content_width(window, 0.),
					40. + NAME_MIN + 16. + BACKUP_ACTIONS_W,
					&[(PACKAGE_MIN, 0), (DATE_W, 1), (SIZE_W, 2)],
				);
				self.backup_cols = [fit[0], fit[1], fit[2]];
				self.backups_section(cx)
			}
		};
		v_flex()
			.size_full()
			.child(self.header(cx))
			.child(div().flex_1().min_h_0().child(body))
			.into_any_element()
	}
}

fn size_of(row: &Row) -> u64 {
	row.usage.map_or(0, |u| u.total())
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

fn signal_quality(rssi: i32) -> std::borrow::Cow<'static, str> {
	match rssi {
		-55.. => t!("hs.signal_excellent"),
		-67.. => t!("hs.signal_good"),
		-75.. => t!("hs.signal_fair"),
		_ => t!("hs.signal_weak"),
	}
}

fn wifi_band(mhz: u32) -> Option<&'static str> {
	match mhz {
		2400..=2500 => Some("2.4 GHz"),
		5000..=5900 => Some("5 GHz"),
		5925..=7125 => Some("6 GHz"),
		_ => None,
	}
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
