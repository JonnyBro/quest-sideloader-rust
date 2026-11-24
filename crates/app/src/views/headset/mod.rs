mod apps;
mod backups;
mod device;

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
	AppUsage, BatteryHealth, ControllerSide, DeviceDetails, DeviceInfo, Leftover, PowerSource,
	RebootMode, RemoteEntry, Tweak,
};
use quest_sideloader_core::catalog::Catalog;
use quest_sideloader_core::saves::Backup;
use quest_sideloader_core::scrcpy::Eye;
use quest_sideloader_core::{AppInfo, Command, Engine, format_bytes};

use serde::{Deserialize, Serialize};

use super::files::FilesView;
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
	Files,
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

pub enum HeadsetEvent {
	/// Show the log of an app in the Console.
	AppLog { package: String, name: String },
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
	/// Packages whose updates the user doesn't want offered.
	held: HashSet<String>,
	/// Which app columns fit: package, version, size, badge.
	app_cols: [bool; 4],
	/// Which backup columns fit: package, date, size.
	backup_cols: [bool; 3],
	/// Folders of uninstalled apps; `None` until searched.
	leftovers: Option<Result<Vec<Leftover>, String>>,
	searching_leftovers: bool,
	files: Entity<FilesView>,
	_filter_changes: Subscription,
}

impl EventEmitter<HeadsetEvent> for HeadsetView {}

impl HeadsetView {
	pub fn new(engine: Rc<Engine>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t!("hs.filter")));
		let subscription = cx.subscribe_in(&filter, window, |_, _, event, _, cx| {
			if matches!(event, InputEvent::Change) {
				cx.notify();
			}
		});
		let files = cx.new(|_| FilesView::new(engine.clone()));
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
			held: HashSet::new(),
			app_cols: [true; 4],
			backup_cols: [true; 3],
			leftovers: None,
			searching_leftovers: false,
			files,
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

	pub fn set_held(&mut self, held: HashSet<String>, cx: &mut Context<Self>) {
		if self.held != held {
			self.held = held;
			cx.notify();
		}
	}

	pub fn set_copies_and_notify(&mut self, copies: HashSet<String>, cx: &mut Context<Self>) {
		self.copies = copies;
		cx.notify();
	}

	pub fn clear(&mut self, cx: &mut Context<Self>) {
		self.serial = None;
		self.info = None;
		self.details = None;
		self.leftovers = None;
		self.files.update(cx, |f, cx| f.set_serial(None, cx));
		cx.notify();
	}

	pub fn set_info(&mut self, serial: &str, info: Option<DeviceInfo>, cx: &mut Context<Self>) {
		if self.serial.as_deref() != Some(serial) {
			self.serial = Some(serial.to_owned());
			self.details = None;
			self.leftovers = None;
			self.files
				.update(cx, |f, cx| f.set_serial(Some(serial.to_owned()), cx));
		}
		self.info = info;
		cx.notify();
	}

	pub fn set_listing(
		&mut self,
		serial: &str,
		path: &str,
		result: Result<Vec<RemoteEntry>, String>,
		cx: &mut Context<Self>,
	) {
		self.files
			.update(cx, |f, cx| f.set_listing(serial, path, result, cx));
	}

	pub fn set_leftovers(
		&mut self,
		serial: &str,
		leftovers: Result<Vec<Leftover>, String>,
		cx: &mut Context<Self>,
	) {
		if self.serial.as_deref() == Some(serial) {
			self.leftovers = Some(leftovers);
			self.searching_leftovers = false;
			cx.notify();
		}
	}

	pub fn set_details(&mut self, serial: &str, details: Details, cx: &mut Context<Self>) {
		if self.serial.as_deref() == Some(serial) {
			self.details = Some(details);
			cx.notify();
		}
	}

	pub fn set_catalog(&mut self, catalog: Option<Arc<Catalog>>, cx: &mut Context<Self>) {
		self.names = catalog
			.as_deref()
			.map(crate::shell::game_names)
			.unwrap_or_default();
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
						Section::Files => 3,
					})
					.child(Tab::new().label(format!("{} {apps}", t!("hs.tab_apps"))))
					.child(Tab::new().label(t!("hs.tab_device").into_owned()))
					.child(Tab::new().label(format!(
						"{} {}",
						t!("hs.tab_backups"),
						self.backups.len()
					)))
					.child(Tab::new().label(t!("hs.tab_files").into_owned()))
					.on_click(cx.listener(|this, ix: &usize, _, cx| {
						this.section = match ix {
							0 => Section::Apps,
							1 => Section::Device,
							2 => Section::Backups,
							_ => Section::Files,
						};
						cx.notify();
					})),
			)
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
			Section::Files => {
				self.files.update(cx, |f, _| f.ensure_loaded());
				self.files.clone().into_any_element()
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
