mod contribute;
mod details;
mod helpers;
mod list;

use helpers::*;
use list::*;
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
use quest_sideloader_core::catalog::{Catalog, Game, MirrorHealth};
use quest_sideloader_core::contribute::{Candidate, Reason, find_candidates};
use quest_sideloader_core::mirror_key::KeyStatus;
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
	Favorites,
	Installed,
	Updates,
}

impl Filter {
	fn needs_headset(self) -> bool {
		matches!(self, Self::Installed | Self::Updates)
	}
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogPrefs {
	pub sort: Sort,
	pub descending: bool,
	pub filter: Filter,
	pub layout: Layout,
	/// Games the mirror marks `(18+)`; installed ones are shown either way.
	pub show_adult: bool,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
	pub device_name: String,
	pub serial: String,
	pub versions: HashMap<String, Option<u64>>,
	/// Unix seconds each package was installed or last updated.
	pub installed_at: HashMap<String, u64>,
	pub free_bytes: Option<u64>,
	pub held: HashSet<String>,
}

impl Installed {
	fn status(&self, game: &Game) -> GameStatus {
		match self.versions.get(&game.package) {
			None => GameStatus::NotInstalled,
			Some(_) if self.held.contains(&game.package) => GameStatus::Installed,
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
	mirror_id: Option<String>,
	health: Option<MirrorHealth>,
	installed: Option<Installed>,
	updates: usize,
	pc_free: Option<u64>,
	search: Entity<InputState>,
	filter: Filter,
	sort: Sort,
	descending: bool,
	layout: Layout,
	show_adult: bool,
	favorites: HashSet<String>,
	hidden: HashSet<String>,
	show_hidden: bool,
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
			mirror_id: None,
			health: None,
			installed: None,
			updates: 0,
			pc_free: None,
			search,
			filter: Filter::All,
			sort: Sort::Name,
			descending: false,
			layout: Layout::List,
			show_adult: false,
			favorites: HashSet::new(),
			hidden: HashSet::new(),
			show_hidden: false,
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
		self.show_adult = prefs.show_adult;
	}

	pub fn prefs(&self) -> CatalogPrefs {
		CatalogPrefs {
			sort: self.sort,
			descending: self.descending,
			filter: self.filter,
			layout: self.layout,
			show_adult: self.show_adult,
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

	pub fn set_mirror(&mut self, mirror: Option<(String, String)>, cx: &mut Context<Self>) {
		let (id, name) = mirror.unzip();
		if self.mirror_id != id {
			self.health = None;
		}
		self.mirror_id = id;
		self.mirror = name;
		cx.notify();
	}

	pub fn set_health(&mut self, health: MirrorHealth, cx: &mut Context<Self>) {
		if self.mirror_id.as_ref() == Some(&health.mirror_id) {
			self.health = Some(health);
			cx.notify();
		}
	}

	fn health_dot(&self, cx: &App) -> Option<AnyElement> {
		self.mirror.as_ref()?;
		let theme = cx.theme();
		let health = self.health.as_ref();
		let key = health.map(|h| &h.key);
		let color = match health {
			Some(h) if h.failing() => theme.danger,
			_ if matches!(key, Some(KeyStatus::Failed(_) | KeyStatus::NotInBuild)) => theme.warning,
			Some(h) if h.last_ok.is_some() => theme.success,
			_ => theme.muted_foreground,
		};
		let mut lines = vec![
			match &self.catalog {
				Some(c) => t!("lib.health_synced", ago = ago(c.synced_at)),
				None => t!("lib.health_never_synced"),
			}
			.into_owned(),
		];
		lines.push(match health.and_then(|h| h.last_ok) {
			Some(at) => t!("lib.health_ok", ago = ago(at)).into_owned(),
			None => t!("lib.health_no_answer").into_owned(),
		});
		if let Some((at, error)) = health.and_then(|h| h.last_error.as_ref()) {
			lines.push(t!("lib.health_error", ago = ago(*at), error = error).into_owned());
		}
		lines.push(match key {
			None | Some(KeyStatus::NotUsed) => t!("lib.key_not_used").into_owned(),
			Some(KeyStatus::NotInBuild) => t!("lib.key_not_in_build").into_owned(),
			Some(KeyStatus::NotFetched) => t!("lib.key_not_fetched").into_owned(),
			Some(KeyStatus::Ready {
				expires_at: Some(at),
			}) => t!("lib.key_ready_until", date = format_date(*at)).into_owned(),
			Some(KeyStatus::Ready { expires_at: None }) => t!("lib.key_ready").into_owned(),
			Some(KeyStatus::Failed(e)) => t!("lib.key_failed", error = e).into_owned(),
		});
		Some(with_tip(
			"mirror-health",
			div().size_2().rounded_full().bg(color),
			lines.join("\n"),
		))
	}

	pub fn set_marks(
		&mut self,
		favorites: HashSet<String>,
		hidden: HashSet<String>,
		cx: &mut Context<Self>,
	) {
		if self.favorites != favorites || self.hidden != hidden {
			if hidden.is_empty() {
				self.show_hidden = false;
			}
			self.favorites = favorites;
			self.hidden = hidden;
			self.rows_dirty = true;
			cx.notify();
		}
	}

	pub fn set_held(&mut self, held: HashSet<String>, cx: &mut Context<Self>) {
		if let Some(apps) = &mut self.installed
			&& apps.held != held
		{
			apps.held = held;
			self.rows_dirty = true;
			self.count_updates();
			cx.notify();
		}
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

	/// Without a headset the headset filters show all games and the status sort
	/// falls back to names; the choice stays for when one connects.
	fn effective(&self) -> (Filter, Sort, bool) {
		let connected = self.installed.is_some();
		let filter = if connected || !self.filter.needs_headset() {
			self.filter
		} else {
			Filter::All
		};
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
					Filter::Favorites => self.favorites.contains(&game.package),
					Filter::Installed => status != GameStatus::NotInstalled,
					Filter::Updates => status == GameStatus::UpdateAvailable,
				};
				let shown =
					(self.show_adult || status != GameStatus::NotInstalled || !is_adult(game))
						&& (self.show_hidden || !self.hidden.contains(&game.package));
				matches_search && matches_filter && shown
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

	fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
		let next = (self.selected != Some(index)).then_some(index);
		self.select(next, cx);
	}

	fn set_favorite(&self, package: String, on: bool) {
		self.engine.send(Command::SetFavorite { package, on });
	}

	fn set_hidden(&mut self, package: String, on: bool, cx: &mut Context<Self>) {
		if on && !self.show_hidden {
			self.select(None, cx);
		}
		self.engine.send(Command::SetHidden { package, on });
	}

	/// The name with a star for favorites, faded for hidden games.
	fn name_cell(&self, game: &Game, cx: &App) -> Div {
		let theme = cx.theme();
		h_flex()
			.gap_1()
			.min_w_0()
			.when(self.favorites.contains(&game.package), |d| {
				d.child(
					Icon::new(IconName::StarFill)
						.xsmall()
						.text_color(theme.warning),
				)
			})
			.when(self.hidden.contains(&game.package), |d| {
				d.text_color(theme.muted_foreground)
			})
			.child(div().truncate().child(game.name.clone()))
	}

	fn download(&self, index: usize, install: bool, streaming: bool) {
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
			streaming,
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
			.children(self.health_dot(cx))
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
				.disabled(value.needs_headset() && !connected)
				.when(value.needs_headset() && !connected, |b| {
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
						"filter-favorites",
						Filter::Favorites,
						t!("lib.favorites").into(),
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
					))
					.child(
						Button::new("filter-adult")
							.label("18+")
							.ghost()
							.small()
							.selected(self.show_adult)
							.tooltip(if self.show_adult {
								t!("lib.adult_shown")
							} else {
								t!("lib.adult_hidden")
							})
							.on_click(cx.listener(|this, _, _, cx| {
								this.show_adult = !this.show_adult;
								this.rows_dirty = true;
								cx.notify();
							})),
					)
					.when(!self.hidden.is_empty(), |bar| {
						bar.child(
							Button::new("filter-hidden")
								.icon(Icon::new(IconName::EyeOff))
								.label(self.hidden.len().to_string())
								.ghost()
								.small()
								.selected(self.show_hidden)
								.tooltip(if self.show_hidden {
									t!("lib.hidden_shown")
								} else {
									t!("lib.hidden_hidden")
								})
								.on_click(cx.listener(|this, _, _, cx| {
									this.show_hidden = !this.show_hidden;
									this.rows_dirty = true;
									cx.notify();
								})),
						)
					}),
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
