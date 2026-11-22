use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, Theme, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::catalog::{Catalog, Game};
use quest_sideloader_core::contribute::{Candidate, Reason, find_candidates};
use quest_sideloader_core::{CatalogStatus, Command, Engine, format_bytes};
use serde::{Deserialize, Serialize};

use crate::theme::Mono;
use crate::widgets::{
	centered, column_header, content_width, fit_columns, format_date, soft_tag, table_row,
	update_tip, with_tip,
};

const ROW_HEIGHT: f32 = 52.;
const COVER: (f32, f32) = (80., 45.);
const CARD_WIDTH: f32 = 220.;
const CARD_GAP: f32 = 12.;
const DETAILS_WIDTH: f32 = 380.;
const VERSION_W: f32 = 110.;
const INSTALLED_W: f32 = 120.;
const SIZE_W: f32 = 100.;
const DATE_W: f32 = 130.;
const RANK_W: f32 = 140.;
const STATUS_W: f32 = 140.;
const NAME_MIN: f32 = 220.;

type MenuAction = Rc<dyn Fn(&mut CatalogView, &mut Context<CatalogView>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Filter {
	#[default]
	All,
	Installed,
	Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Sort {
	#[default]
	Name,
	Package,
	Version,
	Updated,
	Size,
	Popularity,
	Status,
}

impl Sort {
	const ALL: [Sort; 7] = [
		Sort::Name,
		Sort::Package,
		Sort::Version,
		Sort::Updated,
		Sort::Size,
		Sort::Popularity,
		Sort::Status,
	];

	fn label(self) -> String {
		match self {
			Sort::Name => t!("lib.sort_name"),
			Sort::Package => t!("lib.sort_package"),
			Sort::Version => t!("lib.sort_version"),
			Sort::Updated => t!("lib.sort_updated"),
			Sort::Size => t!("lib.sort_size"),
			Sort::Popularity => t!("lib.sort_popularity"),
			Sort::Status => t!("lib.sort_status"),
		}
		.into_owned()
	}

	/// Text sorts A to Z; numbers and dates start with the biggest or newest.
	fn starts_descending(self) -> bool {
		matches!(
			self,
			Sort::Version | Sort::Updated | Sort::Size | Sort::Popularity
		)
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Layout {
	#[default]
	List,
	Grid,
}

/// The catalog's sort, filter and layout, kept between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogPrefs {
	pub sort: Sort,
	pub descending: bool,
	pub filter: Filter,
	pub layout: Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
	NotInstalled,
	Installed,
	UpdateAvailable,
}

/// Available updates first, then installed, then the rest.
fn status_rank(status: GameStatus) -> u8 {
	match status {
		GameStatus::UpdateAvailable => 0,
		GameStatus::Installed => 1,
		GameStatus::NotInstalled => 2,
	}
}

/// The headset the catalog is compared against.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
	pub device_name: String,
	pub serial: String,
	pub versions: HashMap<String, Option<u64>>,
	/// Unix seconds each package was installed or last updated.
	pub installed_at: HashMap<String, u64>,
	pub free_bytes: Option<u64>,
}

impl Installed {
	fn status(&self, game: &Game) -> GameStatus {
		match self.versions.get(&game.package) {
			None => GameStatus::NotInstalled,
			Some(installed) => match (game.version_code, installed) {
				(Some(available), Some(installed)) if available > *installed => {
					GameStatus::UpdateAvailable
				}
				_ => GameStatus::Installed,
			},
		}
	}

	fn update_tip(&self, game: &Game) -> Option<String> {
		(self.status(game) == GameStatus::UpdateAvailable).then(|| {
			update_tip(
				self.versions.get(&game.package).copied().flatten(),
				game.version_code,
				self.installed_at.get(&game.package).copied(),
			)
		})
	}
}

pub struct CatalogView {
	engine: Rc<Engine>,
	catalog: Option<Arc<Catalog>>,
	lowercase_names: Vec<String>,
	/// Per game: the name with only letters and digits, and its initials.
	search_keys: Vec<(String, String)>,
	/// Popularity rank of each game, `#1` being the most downloaded.
	ranks: Vec<Option<u32>>,
	status: CatalogStatus,
	mirror: Option<String>,
	installed: Option<Installed>,
	/// Installed packages the catalog has a newer version of.
	updates: usize,
	/// Free space where downloads go.
	pc_free: Option<u64>,
	search: Entity<InputState>,
	filter: Filter,
	sort: Sort,
	descending: bool,
	layout: Layout,
	selected: Option<usize>,
	rows: Vec<usize>,
	rows_dirty: bool,
	list_scroll: UniformListScrollHandle,
	grid_columns: usize,
	focus: FocusHandle,
	/// Packages never offered for upload: ignored by the user, or renamed copies.
	no_upload: HashSet<String>,
	/// Uploads sent this session, so they aren't offered twice.
	requested: HashSet<String>,
	candidates: Vec<Candidate>,
	/// Which list columns fit: version, installed, size, updated, rank, status.
	cols: [bool; 6],
	this: WeakEntity<Self>,
	_search_changes: Subscription,
}

impl CatalogView {
	pub fn new(engine: Rc<Engine>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("lib.search_keys")));
		let changes = cx.subscribe_in(&search, window, |this, _, event, _, cx| match event {
			InputEvent::Change => {
				this.rows_dirty = true;
				cx.notify();
			}
			InputEvent::PressEnter { .. } => {
				this.refresh_rows(cx);
				if let Some(&first) = this.rows.first() {
					this.select(Some(first), cx);
				}
			}
			_ => {}
		});
		Self {
			engine,
			catalog: None,
			lowercase_names: Vec::new(),
			search_keys: Vec::new(),
			ranks: Vec::new(),
			status: CatalogStatus::Idle,
			mirror: None,
			installed: None,
			updates: 0,
			pc_free: None,
			search,
			filter: Filter::All,
			sort: Sort::Name,
			descending: false,
			layout: Layout::List,
			selected: None,
			rows: Vec::new(),
			rows_dirty: true,
			list_scroll: UniformListScrollHandle::new(),
			grid_columns: 1,
			focus: cx.focus_handle(),
			no_upload: HashSet::new(),
			requested: HashSet::new(),
			candidates: Vec::new(),
			cols: [true; 6],
			this: cx.entity().downgrade(),
			_search_changes: changes,
		}
	}

	pub fn set_prefs(&mut self, prefs: &CatalogPrefs) {
		self.sort = prefs.sort;
		self.descending = prefs.descending;
		self.filter = prefs.filter;
		self.layout = prefs.layout;
	}

	pub fn prefs(&self) -> CatalogPrefs {
		CatalogPrefs {
			sort: self.sort,
			descending: self.descending,
			filter: self.filter,
			layout: self.layout,
		}
	}

	pub fn set_catalog(&mut self, catalog: Option<Arc<Catalog>>, cx: &mut Context<Self>) {
		let games = catalog.as_ref().map_or(&[][..], |c| c.games.as_slice());
		self.search_keys = games.iter().map(|g| search_key(&g.name)).collect();
		self.lowercase_names = games.iter().map(|g| g.name.to_lowercase()).collect();
		self.ranks = popularity_ranks(games);
		self.catalog = catalog;
		self.selected = None;
		self.rows_dirty = true;
		self.refresh_candidates();
		self.count_updates();
		cx.notify();
	}

	pub fn set_status(&mut self, status: CatalogStatus, cx: &mut Context<Self>) {
		self.status = status;
		cx.notify();
	}

	pub fn set_mirror(&mut self, mirror: Option<String>, cx: &mut Context<Self>) {
		self.mirror = mirror;
		cx.notify();
	}

	pub fn set_installed(&mut self, installed: Option<Installed>, cx: &mut Context<Self>) {
		if self.installed != installed {
			self.installed = installed;
			self.rows_dirty = true;
			self.refresh_candidates();
			self.count_updates();
			cx.notify();
		}
	}

	/// How many games the synced catalog has, and what the sync is doing.
	pub fn state(&self) -> (Option<usize>, &CatalogStatus) {
		(self.catalog.as_ref().map(|c| c.games.len()), &self.status)
	}

	pub fn set_pc_free(&mut self, free: Option<u64>) {
		self.pc_free = free;
	}

	pub fn set_no_upload(&mut self, packages: HashSet<String>, cx: &mut Context<Self>) {
		if self.no_upload != packages {
			self.no_upload = packages;
			self.refresh_candidates();
			cx.notify();
		}
	}

	pub fn updates(&self) -> usize {
		self.updates
	}

	fn count_updates(&mut self) {
		self.updates = match (&self.catalog, &self.installed) {
			(Some(catalog), Some(apps)) => catalog
				.games
				.iter()
				.filter(|g| apps.status(g) == GameStatus::UpdateAvailable)
				.map(|g| g.package.as_str())
				.collect::<HashSet<_>>()
				.len(),
			_ => 0,
		};
	}

	fn refresh_candidates(&mut self) {
		self.candidates = match (&self.catalog, &self.installed) {
			(Some(catalog), Some(installed)) if catalog.upload_target().is_some() => {
				let apps: Vec<(&str, Option<u64>)> = installed
					.versions
					.iter()
					.map(|(p, v)| (p.as_str(), *v))
					.collect();
				find_candidates(catalog, &apps, &self.no_upload)
			}
			_ => Vec::new(),
		};
	}

	/// Filter, sort and direction in effect. Without a headset to compare
	/// against, the headset filters show all games and the status sort falls
	/// back to names; the choice itself is kept for when one connects.
	fn effective(&self) -> (Filter, Sort, bool) {
		let connected = self.installed.is_some();
		let filter = if connected { self.filter } else { Filter::All };
		match self.sort {
			Sort::Status if !connected => (filter, Sort::Name, false),
			sort => (filter, sort, self.descending),
		}
	}

	/// Picking the current key again flips the order.
	fn set_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
		if self.sort == sort {
			self.descending = !self.descending;
		} else {
			self.sort = sort;
			self.descending = sort.starts_descending();
		}
		self.rows_dirty = true;
		cx.notify();
	}

	fn refresh_rows(&mut self, cx: &App) {
		if !self.rows_dirty {
			return;
		}
		self.rows_dirty = false;
		let Some(catalog) = &self.catalog else {
			self.rows.clear();
			return;
		};
		let (filter, sort, descending) = self.effective();
		let search = self.search.read(cx).value().trim().to_lowercase();
		let installed = self.installed.as_ref();
		let games = &catalog.games;
		let mut rows: Vec<usize> = (0..games.len())
			.filter(|&i| {
				let game = &games[i];
				let matches_search = search.is_empty()
					|| self.lowercase_names[i].contains(&search)
					|| game.package.to_lowercase().contains(&search)
					|| matches_loosely(&self.search_keys[i], &search);
				let status = installed.map_or(GameStatus::NotInstalled, |apps| apps.status(game));
				let matches_filter = match filter {
					Filter::All => true,
					Filter::Installed => status != GameStatus::NotInstalled,
					Filter::Updates => status == GameStatus::UpdateAvailable,
				};
				matches_search && matches_filter
			})
			.collect();
		let rank = |i: usize| installed.map_or(2, |apps| status_rank(apps.status(&games[i])));
		sort_rows(
			&mut rows,
			games,
			&self.lowercase_names,
			&self.ranks,
			sort,
			descending,
			rank,
		);
		self.rows = rows;
	}

	fn select(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
		self.selected = index;
		if let Some(position) = index.and_then(|i| self.rows.iter().position(|&r| r == i)) {
			let line = match self.layout {
				Layout::List => position,
				Layout::Grid => position / self.grid_columns.max(1),
			};
			self.list_scroll
				.scroll_to_item(line, ScrollStrategy::Nearest);
		}
		cx.notify();
	}

	/// A click toggles the details.
	fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
		let next = (self.selected != Some(index)).then_some(index);
		self.select(next, cx);
	}

	fn download(&self, index: usize, install: bool) {
		let Some(game) = self.catalog.as_ref().and_then(|c| c.games.get(index)) else {
			return;
		};
		let serial = if install {
			match &self.installed {
				Some(apps) => Some(apps.serial.clone()),
				None => return,
			}
		} else {
			None
		};
		self.engine.send(Command::Download {
			game: game.clone(),
			serial,
		});
	}

	/// Ctrl+F focuses the search, Escape closes the details or clears the
	/// search, Enter in the search picks the first result, and the arrow,
	/// Page and Home/End keys move the selection.
	fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
		let key = event.keystroke.key.as_str();
		let modifiers = event.keystroke.modifiers;
		let search_focused = self.search.focus_handle(cx).is_focused(window);
		if key == "f" && modifiers.secondary(cx) {
			self.search.update(cx, |s, cx| s.focus(window, cx));
			cx.stop_propagation();
			return;
		}
		if key == "escape" {
			if self.selected.is_some() && !search_focused {
				self.select(None, cx);
			} else {
				self.search.update(cx, |s, cx| s.set_value("", window, cx));
			}
			cx.stop_propagation();
			return;
		}
		if modifiers.modified() || self.rows.is_empty() {
			return;
		}
		let (line, page) = match self.layout {
			Layout::List => (1, 10),
			Layout::Grid => (self.grid_columns.max(1), self.grid_columns.max(1) * 3),
		};
		let last = self.rows.len() as isize - 1;
		let step = match key {
			"down" => line as isize,
			"up" => -(line as isize),
			"pagedown" => page as isize,
			"pageup" => -(page as isize),
			// The search box needs these for its own cursor.
			_ if search_focused => return,
			"right" if self.layout == Layout::Grid => 1,
			"left" if self.layout == Layout::Grid => -1,
			"home" => -last - 1,
			"end" => last + 1,
			_ => return,
		};
		let current = self
			.selected
			.and_then(|s| self.rows.iter().position(|&r| r == s));
		let next = match current {
			Some(pos) => (pos as isize + step).clamp(0, last) as usize,
			None if step < 0 => last as usize,
			None => 0,
		};
		self.select(Some(self.rows[next]), cx);
		cx.stop_propagation();
	}

	fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let muted = theme.muted_foreground;
		let syncing = matches!(self.status, CatalogStatus::Syncing { .. });
		let status = match &self.status {
			CatalogStatus::Syncing { stage, progress } => h_flex()
				.gap_2()
				.min_w_0()
				.children(
					progress.map(|p| Progress::new("sync-progress").value(p * 100.).w(px(120.))),
				)
				.child(div().truncate().text_color(muted).child(stage.clone()))
				.into_any_element(),
			CatalogStatus::Failed(e) => div()
				.id("sync-failed")
				.truncate()
				.text_color(theme.danger)
				.child(format!("{}: {e}", t!("lib.sync_failed")))
				.into_any_element(),
			CatalogStatus::Idle => div()
				.truncate()
				.text_color(muted)
				.children(self.catalog.as_ref().map(|c| {
					t!("lib.synced", n = c.games.len(), ago = ago(c.synced_at)).into_owned()
				}))
				.into_any_element(),
		};
		h_flex()
			.px_5()
			.pt_4()
			.pb_2()
			.gap_3()
			.child(
				div()
					.text_xl()
					.flex_shrink_0()
					.child(t!("tab.catalog").into_owned()),
			)
			.children(
				self.mirror
					.clone()
					.map(|name| div().flex_shrink_0().text_color(muted).child(name)),
			)
			.child(div().flex_1().min_w_0().text_sm().child(status))
			.when(self.mirror.is_some(), |this| {
				this.child(
					Button::new("sync")
						.label(t!("lib.sync"))
						.icon(Icon::from(crate::icons::AppIcon::Refresh))
						.small()
						.outline()
						.loading(syncing)
						.disabled(syncing)
						.on_click(
							cx.listener(|this, _, _, _| this.engine.send(Command::SyncCatalog)),
						),
				)
			})
	}

	fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let (filter, sort, descending) = self.effective();
		let connected = self.installed.is_some();
		let filter_button = |id: &'static str, value: Filter, label: String| {
			Button::new(id)
				.label(label)
				.ghost()
				.small()
				.selected(filter == value)
				.disabled(value != Filter::All && !connected)
				.when(value != Filter::All && !connected, |b| {
					b.tooltip(t!("lib.connect_compare"))
				})
				.on_click(cx.listener(move |this, _, _, cx| {
					this.filter = value;
					this.rows_dirty = true;
					cx.notify();
				}))
		};
		let layout_button = |id: &'static str, value: Layout, icon: IconName, tip: String| {
			Button::new(id)
				.icon(Icon::new(icon))
				.ghost()
				.small()
				.tooltip(tip)
				.selected(self.layout == value)
				.on_click(cx.listener(move |this, _, _, cx| {
					this.layout = value;
					cx.notify();
				}))
		};
		let view = cx.entity().downgrade();
		h_flex()
			.gap_3()
			.px_5()
			.py_2()
			.child(
				Input::new(&self.search)
					.prefix(Icon::new(IconName::Search).small())
					.cleanable(true)
					.small()
					.w(px(300.)),
			)
			.child(
				h_flex()
					.gap_1()
					.child(filter_button(
						"filter-all",
						Filter::All,
						t!("lib.all").into(),
					))
					.child(filter_button(
						"filter-installed",
						Filter::Installed,
						t!("lib.installed").into(),
					))
					.child(filter_button(
						"filter-updates",
						Filter::Updates,
						t!("lib.updates").into(),
					)),
			)
			// The list sorts by its column headers; the grid has none.
			.when(self.layout == Layout::Grid, |bar| {
				bar.child(
					h_flex()
						.gap_1()
						.child(
							Button::new("sort")
								.label(sort.label())
								.ghost()
								.small()
								.dropdown_caret(true)
								.dropdown_menu(move |mut menu: PopupMenu, _, _| {
									for choice in Sort::ALL {
										let view = view.clone();
										menu = menu.item(
											PopupMenuItem::new(choice.label())
												.checked(choice == sort)
												.disabled(choice == Sort::Status && !connected)
												.on_click(move |_, _, cx| {
													let _ = view.update(cx, |this, cx| {
														this.set_sort(choice, cx)
													});
												}),
										);
									}
									menu
								}),
						)
						.child(
							Button::new("reverse")
								.icon(Icon::new(if descending {
									IconName::SortDescending
								} else {
									IconName::SortAscending
								}))
								.ghost()
								.small()
								.tooltip(t!("lib.reverse"))
								.on_click(cx.listener(move |this, _, _, cx| {
									this.sort = sort;
									this.descending = !descending;
									this.rows_dirty = true;
									cx.notify();
								})),
						),
				)
			})
			.child(div().flex_1())
			.child(
				h_flex()
					.gap_1()
					.child(layout_button(
						"layout-list",
						Layout::List,
						IconName::Menu,
						t!("lib.view_list").into(),
					))
					.child(layout_button(
						"layout-grid",
						Layout::Grid,
						IconName::LayoutDashboard,
						t!("lib.view_grid").into(),
					)),
			)
	}

	fn contribute_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
		let installed = self.installed.as_ref()?;
		let count = self
			.candidates
			.iter()
			.filter(|c| !self.requested.contains(&c.package))
			.count();
		if count == 0 {
			return None;
		}
		let theme = cx.theme();
		Some(
			h_flex()
				.mx_5()
				.mb_2()
				.px_3()
				.py_2()
				.gap_3()
				.rounded(theme.radius)
				.bg(theme.info.opacity(0.12))
				.border_1()
				.border_color(theme.info.opacity(0.4))
				.text_sm()
				.child(div().flex_1().child(
					t!("con.banner", n = count, device = installed.device_name).into_owned(),
				))
				.child(
					Button::new("contribute")
						.label(t!("con.review"))
						.small()
						.outline()
						.on_click(
							cx.listener(|this, _, window, cx| this.open_contribute(window, cx)),
						),
				),
		)
	}

	fn open_contribute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let view = cx.entity().downgrade();
		window.open_dialog(cx, move |dialog, _, cx| {
			let Some(view) = view.upgrade() else {
				return dialog;
			};
			let this = view.read(cx);
			let theme = cx.theme();
			let device = this
				.installed
				.as_ref()
				.map(|i| i.device_name.clone())
				.unwrap_or_default();
			let rows = this.candidates.iter().enumerate().map(|(ix, candidate)| {
				let reason = match &candidate.reason {
					Reason::NotInLibrary => {
						t!("con.not_in_library", version = candidate.version_code)
					}
					Reason::Newer { library_version } => t!(
						"con.newer",
						version = candidate.version_code,
						library = library_version
					),
				};
				let actions = if this.requested.contains(&candidate.package) {
					div()
						.text_color(theme.muted_foreground)
						.child(t!("con.queued").into_owned())
						.into_any_element()
				} else {
					let (send, ignore) = (candidate.clone(), candidate.package.clone());
					let (send_view, ignore_view) = (view.clone(), view.clone());
					h_flex()
						.gap_1()
						.child(
							Button::new(("ignore", ix))
								.label(t!("con.ignore"))
								.outline()
								.xsmall()
								.on_click(move |_, _, cx| {
									ignore_view.update(cx, |this, cx| {
										this.engine
											.send(Command::IgnoreContribution(ignore.clone()));
										this.no_upload.insert(ignore.clone());
										this.refresh_candidates();
										cx.notify();
									});
								}),
						)
						.child(
							Button::new(("upload", ix))
								.label(t!("con.upload"))
								.primary()
								.xsmall()
								.on_click(move |_, _, cx| {
									send_view.update(cx, |this, cx| this.upload(&send, cx));
								}),
						)
						.into_any_element()
				};
				h_flex()
					.py_2()
					.gap_3()
					.border_b_1()
					.border_color(theme.border)
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(candidate.name.clone()))
							.child(
								div()
									.text_sm()
									.truncate()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(candidate.package.clone()),
							),
					)
					.child(div().text_sm().mono(cx).child(reason.into_owned()))
					.child(actions)
			});
			let body = if this.candidates.is_empty() {
				div()
					.text_color(theme.muted_foreground)
					.child(t!("con.nothing").into_owned())
					.into_any_element()
			} else {
				div()
					.id("candidates")
					.max_h(px(380.))
					.overflow_y_scroll()
					.children(rows)
					.into_any_element()
			};
			dialog
				.title(t!("con.title").into_owned())
				.w(px(680.))
				.child(
					v_flex()
						.gap_3()
						.child(div().child(t!("con.intro", device = device).into_owned()))
						.child(
							div()
								.text_sm()
								.text_color(theme.muted_foreground)
								.child(t!("con.privacy").into_owned()),
						)
						.child(body),
				)
		});
	}

	/// Each upload needs the user's click in the dialog.
	fn upload(&mut self, candidate: &Candidate, cx: &mut Context<Self>) {
		let Some(installed) = &self.installed else {
			return;
		};
		self.requested.insert(candidate.package.clone());
		self.engine.send(Command::Contribute {
			serial: installed.serial.clone(),
			package: candidate.package.clone(),
			name: candidate.name.clone(),
		});
		cx.notify();
	}

	fn list(&mut self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let (_, current, descending) = self.effective();
		let connected = self.installed.is_some();
		let column = |sort: Sort, label: String, width: Option<f32>, right: bool| {
			let arrow = (current == sort).then(|| {
				Icon::new(if descending {
					IconName::ChevronDown
				} else {
					IconName::ChevronUp
				})
				.xsmall()
			});
			h_flex()
				.id(SharedString::from(format!("col-{sort:?}")))
				.gap_1()
				.whitespace_nowrap()
				.when_some(width, |d, w| d.w(px(w)).flex_shrink_0())
				.when(width.is_none(), |d| d.flex_1().min_w_0())
				.when(right, |d| d.justify_end())
				.when(current == sort, |d| d.text_color(theme.foreground))
				.when(sort != Sort::Status || connected, |d| {
					d.cursor_pointer()
						.hover(|s| s.text_color(theme.foreground))
						.on_click(cx.listener(move |this, _, _, cx| this.set_sort(sort, cx)))
				})
				.child(div().min_w_0().truncate().child(label))
				.children(arrow)
		};
		let c = self.cols;
		let header = column_header(cx)
			.child(div().w(px(COVER.0)).flex_shrink_0())
			.child(column(Sort::Name, t!("lib.col_game").into(), None, false).min_w(px(NAME_MIN)))
			.when(c[0], |h| {
				h.child(column(
					Sort::Version,
					t!("lib.col_version").into(),
					Some(VERSION_W),
					true,
				))
			})
			.when(connected && c[1], |h| {
				h.child(
					div()
						.w(px(INSTALLED_W))
						.flex_shrink_0()
						.text_right()
						.whitespace_nowrap()
						.child(t!("lib.col_installed").into_owned()),
				)
			})
			.when(c[2], |h| {
				h.child(column(
					Sort::Size,
					t!("lib.col_size").into(),
					Some(SIZE_W),
					true,
				))
			})
			.when(c[3], |h| {
				h.child(column(
					Sort::Updated,
					t!("lib.col_updated").into(),
					Some(DATE_W),
					true,
				))
			})
			.when(c[4], |h| {
				h.child(column(
					Sort::Popularity,
					t!("lib.col_popularity").into(),
					Some(RANK_W),
					true,
				))
			})
			.when(c[5], |h| {
				h.child(column(
					Sort::Status,
					t!("lib.col_status").into(),
					Some(STATUS_W),
					true,
				))
			});
		v_flex()
			.size_full()
			.child(header)
			.child(
				uniform_list(
					"catalog-rows",
					self.rows.len(),
					cx.processor(Self::list_rows),
				)
				.track_scroll(&self.list_scroll)
				.flex_1(),
			)
			.into_any_element()
	}

	fn list_rows(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let Some(catalog) = self.catalog.clone() else {
			return Vec::new();
		};
		let theme = cx.theme().clone();
		range
			.filter_map(|pos| self.rows.get(pos).map(|&index| (pos, index)))
			.map(|(pos, index)| {
				let game = &catalog.games[index];
				let status = self.installed.as_ref().map(|apps| apps.status(game));
				let installed_version = self
					.installed
					.as_ref()
					.and_then(|apps| apps.versions.get(&game.package).copied());
				let selected = self.selected == Some(index);
				let numbers = |text: String, width: f32| {
					div()
						.w(px(width))
						.flex_shrink_0()
						.text_right()
						.text_sm()
						.mono(cx)
						.child(text)
				};
				let c = self.cols;
				table_row(("game", pos), ROW_HEIGHT, &theme)
					.when(selected, |r| r.bg(theme.list_active))
					.on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
					.child(cover(&catalog, game, COVER, &theme))
					.child(
						v_flex()
							.flex_1()
							.min_w(px(NAME_MIN))
							.child(div().truncate().child(game.name.clone()))
							.child(
								div()
									.truncate()
									.text_sm()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(game.release_name.clone()),
							),
					)
					.when(c[0], |r| {
						r.child(numbers(
							game.version_code.map(|v| v.to_string()).unwrap_or_default(),
							VERSION_W,
						))
					})
					.when(self.installed.is_some() && c[1], |r| {
						r.child(
							numbers(
								installed_version
									.flatten()
									.map(|v| v.to_string())
									.unwrap_or_default(),
								INSTALLED_W,
							)
							.text_color(theme.muted_foreground),
						)
					})
					.when(c[2], |r| r.child(numbers(size_text(game), SIZE_W)))
					.when(c[3], |r| {
						r.child(numbers(short_date(&game.last_updated), DATE_W))
					})
					.when(c[4], |r| {
						r.child(numbers(rank_text(self.ranks[index]), RANK_W))
					})
					.when(c[5], |r| {
						r.child(
							h_flex()
								.w(px(STATUS_W))
								.flex_shrink_0()
								.justify_end()
								.children(status.and_then(|s| {
									let tip =
										self.installed.as_ref().and_then(|a| a.update_tip(game));
									status_badge(("status-row", index), s, &theme, tip)
								})),
						)
					})
					.context_menu(self.menu(index))
					.into_any_element()
			})
			.collect()
	}

	fn grid(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
		let details = if self.selected.is_some() {
			DETAILS_WIDTH
		} else {
			0.
		};
		// gpui has no element width before layout; the window width minus the
		// sidebar and the details panel is close enough for the column count.
		let available = content_width(window, details) - 40.;
		self.grid_columns = ((available + CARD_GAP) / (CARD_WIDTH + CARD_GAP)).max(1.) as usize;
		let lines = self.rows.len().div_ceil(self.grid_columns);
		uniform_list("catalog-grid", lines, cx.processor(Self::grid_lines))
			.track_scroll(&self.list_scroll)
			.size_full()
			.into_any_element()
	}

	fn grid_lines(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let Some(catalog) = self.catalog.clone() else {
			return Vec::new();
		};
		let theme = cx.theme().clone();
		let columns = self.grid_columns;
		let thumb = (CARD_WIDTH - 16., (CARD_WIDTH - 16.) * 0.5625);
		range
			.map(|line| {
				let start = line * columns;
				let end = (start + columns).min(self.rows.len());
				let cards = self.rows[start..end].iter().map(|&index| {
					let game = &catalog.games[index];
					let selected = self.selected == Some(index);
					let status = self.installed.as_ref().map(|apps| apps.status(game));
					let mut meta = Vec::new();
					if let Some(version) = game.version_code {
						meta.push(format!("v{version}"));
					}
					let size = size_text(game);
					if !size.is_empty() {
						meta.push(size);
					}
					v_flex()
						.id(("card", index))
						.w(px(CARD_WIDTH))
						.p_2()
						.gap_1()
						.rounded(theme.radius_lg)
						.border_1()
						.border_color(if selected {
							theme.list_active_border
						} else {
							theme.border
						})
						.bg(if selected {
							theme.list_active
						} else {
							theme.muted
						})
						.hover(|s| s.bg(theme.list_hover))
						.on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
						.child(cover(&catalog, game, thumb, &theme))
						.child(div().truncate().child(game.name.clone()))
						.child(
							h_flex()
								.gap_2()
								.text_sm()
								.child(
									div()
										.flex_1()
										.truncate()
										.text_color(theme.muted_foreground)
										.mono(cx)
										.child(meta.join("  ")),
								)
								.children(status.and_then(|s| {
									let tip =
										self.installed.as_ref().and_then(|a| a.update_tip(game));
									status_badge(("status-card", index), s, &theme, tip)
								})),
						)
						.context_menu(self.menu(index))
				});
				h_flex()
					.px_5()
					.pb(px(CARD_GAP))
					.gap(px(CARD_GAP))
					.children(cards)
					.into_any_element()
			})
			.collect()
	}

	/// The right-click menu of a game.
	fn menu(
		&self,
		index: usize,
	) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
		let view = self.this.clone();
		move |menu, _, cx| {
			let Some(view) = view.upgrade() else {
				return menu;
			};
			let this = view.read(cx);
			let Some(game) = this
				.catalog
				.as_ref()
				.and_then(|c| c.games.get(index))
				.cloned()
			else {
				return menu;
			};
			let status = this.installed.as_ref().map(|apps| apps.status(&game));
			let item = |label: String, f: MenuAction| {
				let view = view.downgrade();
				PopupMenuItem::new(label).on_click(move |_, _, cx| {
					let f = f.clone();
					let _ = view.update(cx, move |this, cx| f(this, cx));
				})
			};
			let copy = |label: String, text: String| {
				PopupMenuItem::new(label).on_click(move |_, _, cx| {
					cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
				})
			};
			let mut menu = menu.item(item(
				t!("lib.menu_details").into(),
				Rc::new(move |this, cx| this.select(Some(index), cx)),
			));
			menu = menu.separator();
			if let Some(status) = status {
				menu = menu.item(item(
					install_label(Some(status)),
					Rc::new(move |this, _| this.download(index, true)),
				));
			}
			menu.item(item(
				t!("lib.download_only").into(),
				Rc::new(move |this, _| this.download(index, false)),
			))
			.separator()
			.item(copy(t!("lib.copy_name").into(), game.name.clone()))
			.item(copy(
				t!("lib.copy_release").into(),
				game.release_name.clone(),
			))
			.item(copy(t!("lib.copy_package").into(), game.package.clone()))
		}
	}

	fn details(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
		let catalog = self.catalog.clone()?;
		let game = catalog.games.get(index)?.clone();
		let theme = cx.theme().clone();
		let status = self.installed.as_ref().map(|apps| apps.status(&game));
		let rank = self.ranks.get(index).copied().flatten();

		let mut warnings = Vec::new();
		if let Some(apps) = &self.installed
			&& let Some((needed, free)) = short_of_space(&game, apps)
		{
			warnings.push(
				t!(
					"lib.low_space",
					needed = format_bytes(needed),
					free = format_bytes(free)
				)
				.into_owned(),
			);
		}
		// The archive and the unpacked release are on disk together for a while.
		if let (Some(free), Some(mb)) = (self.pc_free, game.size_mb) {
			let needed = (mb * 2.0 * 1024.0 * 1024.0) as u64;
			if needed > free {
				warnings.push(
					t!(
						"lib.low_pc_space",
						needed = format_bytes(needed),
						free = format_bytes(free)
					)
					.into_owned(),
				);
			}
		}

		let mut facts: Vec<(String, String, bool)> = vec![
			(t!("lib.release").into(), game.release_name.clone(), true),
			(t!("lib.package").into(), game.package.clone(), true),
			(
				t!("lib.col_version").into(),
				game.version_code.map(|v| v.to_string()).unwrap_or_default(),
				true,
			),
			(t!("lib.col_size").into(), size_text(&game), true),
			(
				t!("lib.col_updated").into(),
				mirror_time(&game.last_updated),
				true,
			),
			(t!("lib.col_popularity").into(), rank_text(rank), true),
		];
		if let Some(apps) = &self.installed
			&& let Some(Some(version)) = apps.versions.get(&game.package)
		{
			let mut text = version.to_string();
			if let Some(at) = apps.installed_at.get(&game.package) {
				text.push_str(&format!(", {}", format_date(*at)));
			}
			facts.push((t!("lib.col_installed").into(), text, true));
			if status == Some(GameStatus::UpdateAvailable)
				&& let Some(latest) = game.version_code
			{
				facts.push((
					t!("lib.update").into(),
					format!("{version} -> {latest}"),
					true,
				));
			}
		}

		let install_tip = match &self.installed {
			Some(apps) => t!("lib.installs_on", name = apps.device_name),
			None => t!("lib.connect_install"),
		};
		let note = catalog.note(&game.release_name);
		let thumb_w = DETAILS_WIDTH - 32.;
		Some(
			v_flex()
				.w(px(DETAILS_WIDTH))
				.flex_shrink_0()
				.h_full()
				.border_l_1()
				.border_color(theme.border)
				.bg(theme.sidebar)
				.child(
					h_flex()
						.px_4()
						.pt_4()
						.gap_2()
						.child(div().flex_1().min_w_0().text_lg().child(game.name.clone()))
						.child(
							Button::new("close-details")
								.icon(Icon::new(IconName::Close))
								.ghost()
								.xsmall()
								.on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
						),
				)
				.child(
					div().id("details").flex_1().overflow_y_scroll().child(
						v_flex()
							.p_4()
							.gap_3()
							.child(cover(&catalog, &game, (thumb_w, thumb_w * 0.5625), &theme))
							.child(
								h_flex()
									.gap_2()
									.flex_wrap()
									.child(
										Button::new("install")
											.label(install_label(status))
											.primary()
											.small()
											.disabled(self.installed.is_none())
											.tooltip(install_tip)
											.on_click(cx.listener(move |this, _, _, _| {
												this.download(index, true)
											})),
									)
									.child(
										Button::new("download-only")
											.label(t!("lib.download_only"))
											.outline()
											.small()
											.on_click(cx.listener(move |this, _, _, _| {
												this.download(index, false)
											})),
									),
							)
							.children(
								warnings.into_iter().map(|text| {
									div().text_sm().text_color(theme.warning).child(text)
								}),
							)
							.child(v_flex().gap_1().text_sm().children(facts.into_iter().map(
								|(label, value, mono)| {
									h_flex()
										.gap_3()
										.child(
											div()
												.w(px(120.))
												.flex_shrink_0()
												.text_color(theme.muted_foreground)
												.child(label),
										)
										.child(
											div().min_w_0().when(mono, |d| d.mono(cx)).child(value),
										)
								},
							)))
							.children(status.and_then(|s| {
								status_tag(s, &theme).map(|tag| {
									h_flex()
										.gap_3()
										.text_sm()
										.child(
											div()
												.w(px(120.))
												.text_color(theme.muted_foreground)
												.child(t!("lib.on_headset").into_owned()),
										)
										.child(tag)
								})
							}))
							.children(note.map(|note| {
								v_flex()
									.gap_1()
									.child(
										div()
											.text_color(theme.muted_foreground)
											.child(t!("lib.note").into_owned()),
									)
									.child(div().text_sm().child(note))
							})),
					),
				)
				.into_any_element(),
		)
	}
}

impl Focusable for CatalogView {
	fn focus_handle(&self, _: &App) -> FocusHandle {
		self.focus.clone()
	}
}

impl Render for CatalogView {
	fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		if self.catalog.is_none() {
			let text = match (&self.mirror, &self.status) {
				(None, _) => t!("lib.no_mirror"),
				(Some(_), CatalogStatus::Syncing { .. }) => t!("lib.downloading_list"),
				(Some(_), CatalogStatus::Failed(_)) => t!("lib.load_failed"),
				(Some(_), CatalogStatus::Idle) => t!("lib.empty"),
			};
			return v_flex()
				.size_full()
				.child(self.header(cx))
				.child(centered(text.into_owned(), cx))
				.into_any_element();
		}
		self.refresh_rows(cx);
		let beside = if self.selected.is_some() {
			DETAILS_WIDTH
		} else {
			0.
		};
		let installed_w = if self.installed.is_some() {
			INSTALLED_W
		} else {
			0.
		};
		// Status stays longest, popularity goes first.
		let fit = fit_columns(
			content_width(window, beside),
			40. + COVER.0 + 16. + NAME_MIN,
			&[
				(VERSION_W, 4),
				(installed_w, 1),
				(SIZE_W, 3),
				(DATE_W, 2),
				(RANK_W, 0),
				(STATUS_W, 5),
			],
		);
		self.cols = [fit[0], fit[1], fit[2], fit[3], fit[4], fit[5]];
		let body = if self.rows.is_empty() {
			v_flex()
				.size_full()
				.items_center()
				.justify_center()
				.gap_1()
				.child(div().text_lg().child(t!("lib.no_match_title").into_owned()))
				.child(
					div()
						.text_color(cx.theme().muted_foreground)
						.child(t!("lib.no_match").into_owned()),
				)
				.into_any_element()
		} else {
			match self.layout {
				Layout::List => self.list(cx),
				Layout::Grid => self.grid(window, cx),
			}
		};
		let details = match self.selected {
			Some(index) => self.details(index, cx),
			None => None,
		};
		v_flex()
			.id("catalog")
			.size_full()
			.track_focus(&self.focus)
			.on_key_down(cx.listener(Self::on_key))
			.child(self.header(cx))
			.child(self.toolbar(cx))
			.children(self.contribute_banner(cx))
			.child(
				h_flex()
					.flex_1()
					.min_h_0()
					.border_t_1()
					.border_color(cx.theme().border)
					.child(div().flex_1().min_w_0().h_full().child(body))
					.children(details),
			)
			.into_any_element()
	}
}

fn cover(catalog: &Catalog, game: &Game, size: (f32, f32), theme: &Theme) -> AnyElement {
	match catalog.thumbnail(&game.package) {
		Some(path) => img(path)
			.w(px(size.0))
			.h(px(size.1))
			.flex_shrink_0()
			.rounded(px(4.))
			.object_fit(ObjectFit::Cover)
			.into_any_element(),
		None => div()
			.w(px(size.0))
			.h(px(size.1))
			.flex_shrink_0()
			.rounded(px(4.))
			.bg(theme.muted)
			.border_1()
			.border_color(theme.border)
			.flex()
			.items_center()
			.justify_center()
			.text_color(theme.muted_foreground)
			.child(
				game.name
					.chars()
					.find(|c| c.is_alphanumeric())
					.map(|c| c.to_uppercase().to_string())
					.unwrap_or_default(),
			)
			.into_any_element(),
	}
}

/// The status tag, saying which versions an update is about on hover.
fn status_badge(
	id: impl Into<ElementId>,
	status: GameStatus,
	theme: &Theme,
	tip: Option<String>,
) -> Option<AnyElement> {
	let tag = status_tag(status, theme)?;
	Some(match tip {
		Some(tip) => with_tip(id, tag, tip),
		None => tag.into_any_element(),
	})
}

fn status_tag(status: GameStatus, theme: &Theme) -> Option<impl IntoElement> {
	match status {
		GameStatus::NotInstalled => None,
		GameStatus::Installed => Some(soft_tag(
			theme.success,
			t!("lib.status_installed").into_owned(),
		)),
		GameStatus::UpdateAvailable => Some(soft_tag(
			theme.warning,
			t!("lib.status_update").into_owned(),
		)),
	}
}

fn install_label(status: Option<GameStatus>) -> String {
	match status {
		Some(GameStatus::UpdateAvailable) => t!("lib.download_update"),
		Some(GameStatus::Installed) => t!("lib.download_reinstall"),
		_ => t!("lib.download_install"),
	}
	.into_owned()
}

/// `names` are the games' lowercase names; `rank` gives each game's
/// `status_rank` on the compared headset.
fn sort_rows(
	rows: &mut [usize],
	games: &[Game],
	names: &[String],
	ranks: &[Option<u32>],
	sort: Sort,
	descending: bool,
	rank: impl Fn(usize) -> u8,
) {
	let by_name = |a: usize, b: usize| names[a].cmp(&names[b]);
	rows.sort_by(|&a, &b| {
		let (ga, gb) = (&games[a], &games[b]);
		let primary = match sort {
			Sort::Name => by_name(a, b),
			Sort::Package => ga.package.cmp(&gb.package),
			Sort::Version => ga.version_code.cmp(&gb.version_code),
			Sort::Updated => ga.last_updated.cmp(&gb.last_updated),
			Sort::Size => ga
				.size_mb
				.unwrap_or(0.0)
				.total_cmp(&gb.size_mb.unwrap_or(0.0)),
			// A better rank is a smaller number; unranked games are the lowest.
			Sort::Popularity => ranks[a].map(Reverse).cmp(&ranks[b].map(Reverse)),
			Sort::Status => rank(a).cmp(&rank(b)),
		};
		// Ties stay A to Z whichever way the main key goes.
		let primary = if descending {
			primary.reverse()
		} else {
			primary
		};
		primary
			.then_with(|| by_name(a, b))
			.then_with(|| ga.release_name.cmp(&gb.release_name))
	});
}

/// Letters and digits only, lowercased: "Beat Saber!" -> "beatsaber".
fn squash(text: &str) -> String {
	text.chars()
		.filter(|c| c.is_alphanumeric())
		.flat_map(char::to_lowercase)
		.collect()
}

/// The squashed name and its initials, numbers kept whole:
/// "Resident Evil 4" -> ("residentevil4", "re4").
fn search_key(name: &str) -> (String, String) {
	let mut initials = String::new();
	for word in name
		.split(|c: char| !c.is_alphanumeric())
		.filter(|w| !w.is_empty())
	{
		if word.chars().all(|c| c.is_ascii_digit()) {
			initials.push_str(word);
		} else if let Some(first) = word.chars().next() {
			initials.extend(first.to_lowercase());
		}
	}
	(squash(name), initials)
}

/// Finds games despite spacing, punctuation and abbreviations: "beatsaber",
/// "beat-saber" and "bs" all find Beat Saber.
fn matches_loosely(key: &(String, String), search: &str) -> bool {
	let wanted = squash(search);
	!wanted.is_empty()
		&& (key.0.contains(&wanted) || (wanted.chars().count() >= 2 && key.1.starts_with(&wanted)))
}

/// Ranks packages by their best download score, like androidsideloader;
/// every release of a package gets the package's rank, and a score of 0 none.
fn popularity_ranks(games: &[Game]) -> Vec<Option<u32>> {
	let mut best: HashMap<&str, f64> = HashMap::new();
	for game in games {
		if let Some(score) = game.downloads.filter(|s| *s > 0.0) {
			let entry = best.entry(game.package.as_str()).or_insert(score);
			*entry = entry.max(score);
		}
	}
	let mut order: Vec<(&str, f64)> = best.into_iter().collect();
	order.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
	let ranks: HashMap<&str, u32> = order
		.iter()
		.zip(1..)
		.map(|((package, _), rank)| (*package, rank))
		.collect();
	games
		.iter()
		.map(|g| ranks.get(g.package.as_str()).copied())
		.collect()
}

fn rank_text(rank: Option<u32>) -> String {
	rank.map(|r| format!("#{r}")).unwrap_or_default()
}

/// The game's size and the headset's free space, when the game won't fit.
/// The mirror lists the download size; the installed game takes about as much.
fn short_of_space(game: &Game, apps: &Installed) -> Option<(u64, u64)> {
	let needed = (game.size_mb? * 1024.0 * 1024.0) as u64;
	let free = apps.free_bytes?;
	(needed > free).then_some((needed, free))
}

fn size_text(game: &Game) -> String {
	game.size_mb
		.map(|mb| format_bytes((mb * 1024.0 * 1024.0) as u64))
		.unwrap_or_default()
}

/// The mirror writes `YYYY-MM-DD HH:MM UTC`; shown in the computer's time zone.
fn mirror_time(value: &str) -> String {
	let trimmed = value.trim().trim_end_matches("UTC").trim();
	chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M")
		.map(|t| {
			t.and_utc()
				.with_timezone(&chrono::Local)
				.format("%Y-%m-%d %H:%M")
				.to_string()
		})
		.unwrap_or_else(|_| value.to_owned())
}

/// `2024-11-02 10:15 UTC` -> `2024-11-02`, in the computer's time zone.
fn short_date(value: &str) -> String {
	let local = mirror_time(value);
	local.split_whitespace().next().unwrap_or(&local).to_owned()
}

fn ago(time: SystemTime) -> String {
	let secs = SystemTime::now()
		.duration_since(time)
		.map_or(0, |d| d.as_secs());
	match secs {
		0..60 => t!("lib.just_now").into_owned(),
		60..3600 => t!("lib.mins_ago", n = secs / 60).into_owned(),
		3600..86_400 => t!("lib.hours_ago", n = secs / 3600).into_owned(),
		_ => t!("lib.days_ago", n = secs / 86_400).into_owned(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	// The gpui glob import brings its own `test` attribute.
	use ::core::prelude::v1::test;

	fn game(package: &str, version: Option<u64>) -> Game {
		Game {
			name: "G".into(),
			release_name: "G v1".into(),
			package: package.into(),
			version_code: version,
			last_updated: String::new(),
			size_mb: None,
			downloads: None,
		}
	}

	#[test]
	fn loose_search() {
		let key = search_key("Resident Evil 4");
		assert_eq!(key, ("residentevil4".into(), "re4".into()));
		assert!(matches_loosely(&key, "re4"));
		assert!(matches_loosely(&key, "resident-evil 4"));
		assert!(matches_loosely(&key, "Evil4"));
		assert!(matches_loosely(&key, "r"));
		assert!(!matches_loosely(&key, "rex"));
		assert!(!matches_loosely(&key, "re5"));
		assert!(matches_loosely(&search_key("Beat Saber"), "beatsaber"));
		assert!(matches_loosely(&search_key("Beat Saber"), "bs"));
	}

	#[test]
	fn status_against_device() {
		let apps = Installed {
			device_name: "Quest 3".into(),
			serial: "S".into(),
			versions: HashMap::from([("com.a".into(), Some(10)), ("com.b".into(), None)]),
			installed_at: HashMap::new(),
			free_bytes: None,
		};
		assert_eq!(
			apps.status(&game("com.a", Some(11))),
			GameStatus::UpdateAvailable
		);
		assert_eq!(apps.status(&game("com.a", Some(10))), GameStatus::Installed);
		assert_eq!(apps.status(&game("com.a", Some(9))), GameStatus::Installed);
		assert_eq!(apps.status(&game("com.b", Some(5))), GameStatus::Installed);
		assert_eq!(
			apps.status(&game("com.c", Some(1))),
			GameStatus::NotInstalled
		);
	}

	#[test]
	fn sorts_by_each_key_with_name_ties() {
		let mut games = vec![
			game("com.c", Some(3)),
			game("com.a", None),
			game("com.b", Some(3)),
		];
		games[0].name = "Beta".into();
		games[1].name = "Alpha".into();
		games[2].name = "Alpha".into();
		games[2].release_name = "Alpha v2".into();
		games[0].size_mb = Some(500.0);
		games[2].size_mb = Some(10.0);
		let names: Vec<String> = games.iter().map(|g| g.name.to_lowercase()).collect();
		let ranks = [Some(2), None, Some(1)];
		let order = |sort, descending, rank: &dyn Fn(usize) -> u8| {
			let mut rows = vec![0, 1, 2];
			sort_rows(&mut rows, &games, &names, &ranks, sort, descending, rank);
			rows
		};
		let none = |_| 2;
		assert_eq!(order(Sort::Name, false, &none), [2, 1, 0]);
		assert_eq!(order(Sort::Name, true, &none), [0, 2, 1]);
		assert_eq!(order(Sort::Package, false, &none), [1, 2, 0]);
		assert_eq!(order(Sort::Version, true, &none), [2, 0, 1]);
		assert_eq!(order(Sort::Size, true, &none), [0, 2, 1]);
		assert_eq!(order(Sort::Popularity, true, &none), [2, 0, 1]);
		let rank = |i| [1, 2, 0][i];
		assert_eq!(order(Sort::Status, false, &rank), [2, 0, 1]);
	}

	#[test]
	fn ranks_packages_by_their_best_score() {
		let mut games = vec![
			game("com.a", None),
			game("com.b", None),
			game("com.a", None),
			game("com.c", None),
			game("com.d", None),
		];
		games[0].downloads = Some(5.0);
		games[1].downloads = Some(7.5);
		games[2].downloads = Some(9.0);
		games[3].downloads = Some(0.0);
		assert_eq!(
			popularity_ranks(&games),
			[Some(1), Some(2), Some(1), None, None]
		);
	}

	#[test]
	fn warns_only_when_the_game_does_not_fit() {
		let mut apps = Installed {
			device_name: "Quest 3".into(),
			serial: "S".into(),
			versions: HashMap::new(),
			installed_at: HashMap::new(),
			free_bytes: Some(100 * 1024 * 1024),
		};
		let mut big = game("com.a", Some(1));
		big.size_mb = Some(150.0);
		let mut small = game("com.b", Some(1));
		small.size_mb = Some(50.0);
		assert_eq!(
			short_of_space(&big, &apps),
			Some((150 * 1024 * 1024, 100 * 1024 * 1024))
		);
		assert_eq!(short_of_space(&small, &apps), None);
		assert_eq!(short_of_space(&game("com.c", None), &apps), None);
		apps.free_bytes = None;
		assert_eq!(short_of_space(&big, &apps), None);
	}

	#[test]
	fn dates() {
		// The day depends on the time zone the test runs in.
		let date = short_date("2024-11-02 10:15 UTC");
		assert!(["2024-11-01", "2024-11-02", "2024-11-03"].contains(&date.as_str()));
		assert_eq!(short_date(""), "");
		assert_eq!(short_date("someday"), "someday");
	}
}
