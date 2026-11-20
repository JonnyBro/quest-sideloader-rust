use rust_i18n::t;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use egui_extras::{Column, TableBuilder};
use quest_sideloader_core::catalog::{Catalog, Game};
use quest_sideloader_core::{CatalogStatus, Command, format_bytes};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
enum Filter {
	#[default]
	All,
	Installed,
	Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
enum Sort {
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

	fn label(self) -> std::borrow::Cow<'static, str> {
		match self {
			Sort::Name => t!("lib.sort_name"),
			Sort::Package => t!("lib.sort_package"),
			Sort::Version => t!("lib.sort_version"),
			Sort::Updated => t!("lib.sort_updated"),
			Sort::Size => t!("lib.sort_size"),
			Sort::Popularity => t!("lib.sort_popularity"),
			Sort::Status => t!("lib.sort_status"),
		}
	}

	/// Text sorts A to Z; numbers and dates start with the biggest or newest;
	/// status starts with available updates.
	fn starts_descending(self) -> bool {
		matches!(
			self,
			Sort::Version | Sort::Updated | Sort::Size | Sort::Popularity
		)
	}
}

/// Available updates first, then installed, then the rest.
fn status_rank(status: GameStatus) -> u8 {
	match status {
		GameStatus::UpdateAvailable => 0,
		GameStatus::Installed => 1,
		GameStatus::NotInstalled => 2,
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
	NotInstalled,
	Installed,
	UpdateAvailable,
}

/// Installed packages of the headset the library is compared against.
pub struct InstalledApps<'a> {
	pub device_name: String,
	pub serial: &'a str,
	pub versions: HashMap<&'a str, Option<u64>>,
	pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
enum View {
	#[default]
	List,
	Grid,
}

/// The library's sort, filter and view, kept between runs.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryPrefs {
	sort: Sort,
	descending: bool,
	filter: Filter,
	view: View,
}

const SEARCH_ID: &str = "library_search";
const CARD_WIDTH: f32 = 200.0;

impl InstalledApps<'_> {
	fn status(&self, game: &Game) -> GameStatus {
		match self.versions.get(game.package.as_str()) {
			None => GameStatus::NotInstalled,
			Some(installed) => match (game.version_code, installed) {
				(Some(available), Some(installed)) if available > *installed => {
					GameStatus::UpdateAvailable
				}
				_ => GameStatus::Installed,
			},
		}
	}
}

#[derive(PartialEq)]
struct RowsKey {
	catalog: usize,
	search: String,
	filter: Filter,
	sort: Sort,
	descending: bool,
	device: Option<(String, usize)>,
}

pub struct LibraryView {
	catalog: Option<Arc<Catalog>>,
	lowercase_names: Vec<String>,
	/// Popularity rank of each game, `#1` being the most downloaded.
	ranks: Vec<Option<u32>>,
	status: CatalogStatus,
	search: String,
	filter: Filter,
	sort: Sort,
	descending: bool,
	view: View,
	selected: Option<usize>,
	rows: Vec<usize>,
	rows_key: Option<RowsKey>,
	/// Position in `rows` to bring into view, after a keyboard move.
	scroll_to: Option<usize>,
	/// Enter in the search box: select the first result once rows are ready.
	select_first: bool,
	grid_columns: usize,
	/// Scroll offset and visible height of the grid in the last frame.
	grid_view: (f32, f32),
	contribute: super::contribute::ContributeView,
	names: HashMap<String, String>,
}

impl LibraryView {
	pub fn new() -> Self {
		Self {
			catalog: None,
			lowercase_names: Vec::new(),
			ranks: Vec::new(),
			status: CatalogStatus::Idle,
			search: String::new(),
			filter: Filter::All,
			sort: Sort::Name,
			descending: false,
			view: View::List,
			selected: None,
			rows: Vec::new(),
			rows_key: None,
			scroll_to: None,
			select_first: false,
			grid_columns: 1,
			grid_view: (0.0, 0.0),
			contribute: super::contribute::ContributeView::new(),
			names: HashMap::new(),
		}
	}

	/// Game names by package, for showing installed apps by name.
	pub fn names(&self) -> &HashMap<String, String> {
		&self.names
	}

	pub fn on_catalog(&mut self, catalog: Option<Arc<Catalog>>) {
		self.names = catalog
			.as_ref()
			.map(|c| {
				c.games
					.iter()
					.map(|g| (g.package.clone(), g.name.clone()))
					.collect()
			})
			.unwrap_or_default();
		self.lowercase_names = catalog
			.as_ref()
			.map(|c| c.games.iter().map(|g| g.name.to_lowercase()).collect())
			.unwrap_or_default();
		self.ranks = catalog
			.as_ref()
			.map(|c| popularity_ranks(&c.games))
			.unwrap_or_default();
		self.catalog = catalog;
		self.selected = None;
		self.rows_key = None;
	}

	pub fn on_status(&mut self, status: CatalogStatus) {
		self.status = status;
	}

	pub fn prefs(&self) -> LibraryPrefs {
		LibraryPrefs {
			sort: self.sort,
			descending: self.descending,
			filter: self.filter,
			view: self.view,
		}
	}

	pub fn restore(&mut self, prefs: LibraryPrefs) {
		self.sort = prefs.sort;
		self.descending = prefs.descending;
		self.filter = prefs.filter;
		self.view = prefs.view;
	}

	/// Filter, sort and direction in effect. Without a headset to compare
	/// against, the headset filters show all games and the status sort falls
	/// back to names; the choice itself is kept for when one connects.
	fn effective(&self, connected: bool) -> (Filter, Sort, bool) {
		let filter = if connected { self.filter } else { Filter::All };
		match self.sort {
			Sort::Status if !connected => (filter, Sort::Name, false),
			sort => (filter, sort, self.descending),
		}
	}

	/// Picking the current key again flips the order.
	fn set_sort(&mut self, sort: Sort) {
		if self.sort == sort {
			self.descending = !self.descending;
		} else {
			self.sort = sort;
			self.descending = sort.starts_descending();
		}
	}

	fn refresh_rows(&mut self, installed: Option<&InstalledApps<'_>>) {
		let Some(catalog) = &self.catalog else {
			self.rows.clear();
			return;
		};
		let (filter, sort, descending) = self.effective(installed.is_some());
		let key = RowsKey {
			catalog: Arc::as_ptr(catalog) as usize,
			search: self.search.trim().to_lowercase(),
			filter,
			sort,
			descending,
			device: installed.map(|i| (i.serial.to_owned(), i.versions.len())),
		};
		if self.rows_key.as_ref() == Some(&key) {
			return;
		}

		let games = &catalog.games;
		let mut rows: Vec<usize> = (0..games.len())
			.filter(|&i| {
				let game = &games[i];
				let matches_search = key.search.is_empty()
					|| self.lowercase_names[i].contains(&key.search)
					|| game.package.to_lowercase().contains(&key.search);
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
		self.rows_key = Some(key);
	}

	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		mirror_name: Option<&str>,
		installed: Option<&InstalledApps<'_>>,
		contribute_ignored: &std::collections::HashSet<String>,
		commands: &mut Vec<Command>,
	) {
		self.toolbar(ui, mirror_name, installed, commands);
		if let (Some(catalog), Some(apps)) = (&self.catalog, installed) {
			let catalog = catalog.clone();
			self.contribute
				.banner(ui, &catalog, apps, contribute_ignored);
			let ctx = ui.ctx().clone();
			self.contribute
				.window(&ctx, &catalog, apps, contribute_ignored, commands);
		}
		ui.separator();

		let Some(catalog) = self.catalog.clone() else {
			ui.centered_and_justified(|ui| {
				ui.weak(match (mirror_name, &self.status) {
					(None, _) => t!("lib.no_mirror"),
					(Some(_), CatalogStatus::Syncing { .. }) => t!("lib.downloading_list"),
					(Some(_), CatalogStatus::Failed(_)) => t!("lib.load_failed"),
					(Some(_), CatalogStatus::Idle) => t!("lib.empty"),
				});
			});
			return;
		};
		self.refresh_rows(installed);
		self.keyboard(ui.ctx());

		if let Some(index) = self.selected
			&& let Some(game) = catalog.games.get(index)
		{
			let mut close = false;
			let rank = self.ranks.get(index).copied().flatten();
			egui::Panel::right("game_details")
				.resizable(true)
				.default_size(320.0)
				.show(ui, |ui| {
					close = details(ui, &catalog, game, rank, installed, commands)
				});
			if close {
				self.selected = None;
			}
		}

		egui::CentralPanel::default().show(ui, |ui| match self.view {
			View::List => self.table(ui, &catalog, installed, commands),
			View::Grid => self.grid(ui, &catalog, installed, commands),
		});
	}

	/// Ctrl+F focuses the search, Escape closes the details or clears the
	/// search, Enter in the search picks the first result, and the arrow,
	/// Page and Home/End keys move the selection.
	fn keyboard(&mut self, ctx: &egui::Context) {
		use egui::{Key, Modifiers};
		let search = egui::Id::unique(SEARCH_ID);
		let search_focused = ctx.memory(|m| m.has_focus(search));
		if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
			ctx.memory_mut(|m| m.request_focus(search));
		}
		if ctx.input(|i| i.key_pressed(Key::Escape)) {
			if self.selected.is_some() && !search_focused {
				self.selected = None;
			} else {
				self.search.clear();
			}
		}
		if std::mem::take(&mut self.select_first) && !self.rows.is_empty() {
			self.selected = Some(self.rows[0]);
			self.scroll_to = Some(0);
		}
		// Other text fields keep their keys; the search box gives up only
		// the keys it has no use for.
		if ctx.egui_wants_keyboard_input() && !search_focused {
			return;
		}
		let (line, page) = match self.view {
			View::List => (1, 10),
			View::Grid => (self.grid_columns.max(1), self.grid_columns.max(1) * 3),
		};
		let last = self.rows.len().saturating_sub(1) as isize;
		let step = ctx.input_mut(|i| {
			let mut take = |key| i.consume_key(Modifiers::NONE, key);
			if take(Key::ArrowDown) {
				Some(line as isize)
			} else if take(Key::ArrowUp) {
				Some(-(line as isize))
			} else if take(Key::PageDown) {
				Some(page as isize)
			} else if take(Key::PageUp) {
				Some(-(page as isize))
			} else if search_focused {
				None
			} else if self.view == View::Grid && take(Key::ArrowRight) {
				Some(1)
			} else if self.view == View::Grid && take(Key::ArrowLeft) {
				Some(-1)
			} else if take(Key::Home) {
				Some(-last - 1)
			} else if take(Key::End) {
				Some(last + 1)
			} else {
				None
			}
		});
		let Some(step) = step else { return };
		if self.rows.is_empty() {
			return;
		}
		let current = self
			.selected
			.and_then(|s| self.rows.iter().position(|&r| r == s));
		let next = match current {
			Some(pos) => (pos as isize + step).clamp(0, last) as usize,
			None if step < 0 => last as usize,
			None => 0,
		};
		self.selected = Some(self.rows[next]);
		self.scroll_to = Some(next);
	}

	fn toolbar(
		&mut self,
		ui: &mut egui::Ui,
		mirror_name: Option<&str>,
		installed: Option<&InstalledApps<'_>>,
		commands: &mut Vec<Command>,
	) {
		ui.horizontal(|ui| {
			let search = ui.add(
				egui::TextEdit::singleline(&mut self.search)
					.id(egui::Id::unique(SEARCH_ID))
					.hint_text(t!("lib.search_keys"))
					.desired_width(240.0),
			);
			if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
				self.select_first = true;
			}
			let (filter, sort, descending) = self.effective(installed.is_some());
			if ui
				.selectable_label(filter == Filter::All, t!("lib.all"))
				.clicked()
			{
				self.filter = Filter::All;
			}
			let enabled = installed.is_some();
			ui.add_enabled_ui(enabled, |ui| {
				for (choice, label) in [
					(Filter::Installed, t!("lib.installed")),
					(Filter::Updates, t!("lib.updates")),
				] {
					if ui.selectable_label(filter == choice, label).clicked() {
						self.filter = choice;
					}
				}
			})
			.response
			.on_disabled_hover_text(t!("lib.connect_compare"));

			ui.separator();
			egui::ComboBox::from_id_salt("library_sort")
				.selected_text(sort.label())
				.show_ui(ui, |ui| {
					for choice in Sort::ALL {
						let enabled = choice != Sort::Status || installed.is_some();
						let item = ui
							.add_enabled(
								enabled,
								egui::Button::selectable(sort == choice, choice.label()),
							)
							.on_disabled_hover_text(t!("lib.connect_compare"));
						if item.clicked() {
							self.set_sort(choice);
						}
					}
				});
			let arrow = if descending { "⬇" } else { "⬆" };
			if ui.button(arrow).on_hover_text(t!("lib.reverse")).clicked() {
				self.sort = sort;
				self.descending = !descending;
			}

			ui.separator();
			ui.selectable_value(&mut self.view, View::List, t!("lib.view_list"));
			ui.selectable_value(&mut self.view, View::Grid, t!("lib.view_grid"));

			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				let syncing = matches!(self.status, CatalogStatus::Syncing { .. });
				if mirror_name.is_some()
					&& ui
						.add_enabled(!syncing, egui::Button::new(t!("lib.sync")))
						.clicked()
				{
					commands.push(Command::SyncCatalog);
				}
				match &self.status {
					CatalogStatus::Syncing { stage, progress } => {
						match progress {
							Some(p) => {
								ui.add(
									egui::ProgressBar::new(*p)
										.desired_width(120.0)
										.show_percentage(),
								);
							}
							None => {
								ui.spinner();
							}
						}
						fit(ui, stage.as_str());
					}
					CatalogStatus::Failed(e) => {
						ui.colored_label(ui.visuals().error_fg_color, t!("lib.sync_failed"))
							.on_hover_text(e);
					}
					CatalogStatus::Idle => {
						if let Some(catalog) = &self.catalog {
							let synced = t!(
								"lib.synced",
								n = catalog.games.len(),
								ago = ago(catalog.synced_at)
							);
							fit(ui, egui::RichText::new(synced).weak());
						}
					}
				}
				if let Some(name) = mirror_name {
					fit(ui, name);
				}
				if let Some(apps) = installed {
					let comparing = t!("lib.comparing", device = apps.device_name);
					fit(ui, egui::RichText::new(comparing).weak());
				}
			});
		});
		if let CatalogStatus::Failed(e) = &self.status {
			ui.colored_label(ui.visuals().error_fg_color, e);
		}
	}

	fn table(
		&mut self,
		ui: &mut egui::Ui,
		catalog: &Catalog,
		installed: Option<&InstalledApps<'_>>,
		commands: &mut Vec<Command>,
	) {
		if self.rows.is_empty() {
			ui.centered_and_justified(|ui| ui.weak(t!("lib.no_match")));
			return;
		}
		let row_height = 44.0;
		let mut clicked = None;
		let mut opened = None;
		let mut sort_click = None;
		let (_, current, descending) = self.effective(installed.is_some());
		let mut table = TableBuilder::new(ui)
			.striped(true)
			.sense(egui::Sense::click())
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(Column::exact(80.0))
			.column(Column::remainder().at_least(200.0).clip(true))
			.column(Column::auto().at_least(80.0));
		if let Some(position) = self.scroll_to.take() {
			table = table.scroll_to_row(position, None);
		}
		// The installed version only means something with a headset.
		if installed.is_some() {
			table = table.column(Column::auto().at_least(80.0));
		}
		table
			.column(Column::auto().at_least(70.0))
			.column(Column::auto().at_least(120.0))
			.column(Column::auto().at_least(60.0))
			.column(Column::auto().at_least(100.0))
			.header(22.0, |mut header| {
				header.col(|_| {});
				let mut columns = vec![
					(Some(Sort::Name), t!("lib.col_game")),
					(Some(Sort::Version), t!("lib.col_version")),
				];
				if installed.is_some() {
					columns.push((None, t!("lib.col_installed")));
				}
				columns.extend([
					(Some(Sort::Size), t!("lib.col_size")),
					(Some(Sort::Updated), t!("lib.col_updated")),
					(Some(Sort::Popularity), t!("lib.col_popularity")),
					(Some(Sort::Status), t!("lib.col_status")),
				]);
				for (sort, title) in columns {
					header.col(|ui| {
						let Some(sort) = sort else {
							ui.strong(title);
							return;
						};
						let text = if current == sort {
							let arrow = if descending { "⬇" } else { "⬆" };
							format!("{title} {arrow}")
						} else {
							title.into_owned()
						};
						let enabled = sort != Sort::Status || installed.is_some();
						let button =
							egui::Button::new(egui::RichText::new(text).strong()).frame(false);
						if ui
							.add_enabled(enabled, button)
							.on_hover_text(t!("lib.sort_by_column"))
							.clicked()
						{
							sort_click = Some(sort);
						}
					});
				}
			})
			.body(|body| {
				body.rows(row_height, self.rows.len(), |mut row| {
					let index = self.rows[row.index()];
					let game = &catalog.games[index];
					row.set_selected(self.selected == Some(index));
					row.col(|ui| thumbnail(ui, catalog, game, egui::vec2(72.0, 40.0)));
					row.col(|ui| {
						ui.vertical(|ui| {
							ui.label(&game.name);
							ui.small(&game.release_name);
						});
					});
					row.col(|ui| {
						ui.label(game.version_code.map(|v| v.to_string()).unwrap_or_default());
					});
					if let Some(apps) = installed {
						row.col(|ui| {
							if let Some(version) = apps.versions.get(game.package.as_str()) {
								ui.label(version.map(|v| v.to_string()).unwrap_or_default());
							}
						});
					}
					row.col(|ui| {
						ui.label(size_text(game));
					});
					row.col(|ui| {
						ui.label(short_date(&game.last_updated));
					});
					row.col(|ui| {
						ui.label(rank_text(self.ranks[index]));
					});
					row.col(|ui| {
						if let Some(apps) = installed {
							status_label(ui, apps.status(game));
						}
					});
					let response = row.response();
					if response.clicked() {
						clicked = Some(index);
					}
					response.context_menu(|ui| {
						if game_menu(ui, game, installed, commands) {
							opened = Some(index);
						}
					});
				});
			});
		if let Some(sort) = sort_click {
			self.set_sort(sort);
		}
		self.pick(clicked, opened);
	}

	/// A click toggles the details; "Details" in a menu always opens them.
	fn pick(&mut self, clicked: Option<usize>, opened: Option<usize>) {
		if let Some(index) = opened {
			self.selected = Some(index);
		} else if let Some(index) = clicked {
			self.selected = if self.selected == Some(index) {
				None
			} else {
				Some(index)
			};
		}
	}

	fn grid(
		&mut self,
		ui: &mut egui::Ui,
		catalog: &Catalog,
		installed: Option<&InstalledApps<'_>>,
		commands: &mut Vec<Command>,
	) {
		if self.rows.is_empty() {
			ui.centered_and_justified(|ui| ui.weak(t!("lib.no_match")));
			return;
		}
		let spacing = ui.spacing().item_spacing;
		let thumb = egui::vec2(CARD_WIDTH - 12.0, (CARD_WIDTH - 12.0) * 0.5625);
		let card = egui::vec2(CARD_WIDTH, thumb.y + 66.0);
		let columns = (((ui.available_width() + spacing.x) / (card.x + spacing.x)) as usize).max(1);
		self.grid_columns = columns;
		let lines = self.rows.len().div_ceil(columns);
		let line_height = card.y + spacing.y;

		let mut area = egui::ScrollArea::vertical().auto_shrink(false);
		if let Some(position) = self.scroll_to.take() {
			let top = (position / columns) as f32 * line_height;
			let (offset, height) = self.grid_view;
			if top < offset {
				area = area.vertical_scroll_offset(top);
			} else if top + line_height > offset + height {
				area = area.vertical_scroll_offset(top + line_height - height);
			}
		}
		let mut clicked = None;
		let mut opened = None;
		let output = area.show_rows(ui, card.y, lines, |ui, range| {
			for line in range {
				ui.horizontal(|ui| {
					let start = line * columns;
					let end = (start + columns).min(self.rows.len());
					for &index in &self.rows[start..end] {
						let game = &catalog.games[index];
						let selected = self.selected == Some(index);
						let response =
							game_card(ui, catalog, game, installed, selected, card, thumb);
						if response.clicked() {
							clicked = Some(index);
						}
						response.context_menu(|ui| {
							if game_menu(ui, game, installed, commands) {
								opened = Some(index);
							}
						});
					}
				});
			}
		});
		self.grid_view = (output.state.clamped_offset().y, output.inner_rect.height());
		self.pick(clicked, opened);
	}
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

fn install_label(status: Option<GameStatus>) -> std::borrow::Cow<'static, str> {
	match status {
		Some(GameStatus::UpdateAvailable) => t!("lib.download_update"),
		Some(GameStatus::Installed) => t!("lib.download_reinstall"),
		_ => t!("lib.download_install"),
	}
}

/// The right-click menu of a game. Returns true when "Details" was picked.
fn game_menu(
	ui: &mut egui::Ui,
	game: &Game,
	installed: Option<&InstalledApps<'_>>,
	commands: &mut Vec<Command>,
) -> bool {
	let mut details = false;
	if ui.button(t!("lib.menu_details")).clicked() {
		details = true;
		ui.close();
	}
	ui.separator();
	if let Some(apps) = installed
		&& ui.button(install_label(Some(apps.status(game)))).clicked()
	{
		commands.push(Command::Download {
			game: game.clone(),
			serial: Some(apps.serial.to_owned()),
		});
		ui.close();
	}
	if ui.button(t!("lib.download_only")).clicked() {
		commands.push(Command::Download {
			game: game.clone(),
			serial: None,
		});
		ui.close();
	}
	ui.separator();
	for (label, text) in [
		(t!("lib.copy_name"), &game.name),
		(t!("lib.copy_release"), &game.release_name),
		(t!("lib.copy_package"), &game.package),
	] {
		if ui.button(label).clicked() {
			ui.ctx().copy_text(text.clone());
			ui.close();
		}
	}
	details
}

/// One game in the grid view: cover, name, version and size, status.
fn game_card(
	ui: &mut egui::Ui,
	catalog: &Catalog,
	game: &Game,
	installed: Option<&InstalledApps<'_>>,
	selected: bool,
	size: egui::Vec2,
	thumb: egui::Vec2,
) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
	let visuals = ui.visuals();
	let fill = if selected {
		visuals.selection.bg_fill
	} else if response.hovered() {
		visuals.widgets.hovered.weak_bg_fill
	} else {
		visuals.faint_bg_color
	};
	ui.painter().rect_filled(rect, 4.0, fill);
	let mut content = ui.new_child(
		egui::UiBuilder::new()
			.max_rect(rect.shrink(6.0))
			.layout(egui::Layout::top_down(egui::Align::Min)),
	);
	// The card takes the clicks, so its text must not.
	content.style_mut().interaction.selectable_labels = false;
	thumbnail(&mut content, catalog, game, thumb);
	content.add(egui::Label::new(egui::RichText::new(&game.name).strong()).truncate());
	let mut line = Vec::new();
	if let Some(version) = game.version_code {
		line.push(format!("v{version}"));
	}
	let size_text = size_text(game);
	if !size_text.is_empty() {
		line.push(size_text);
	}
	content.add(egui::Label::new(egui::RichText::new(line.join(", ")).small()).truncate());
	if let Some(apps) = installed {
		status_label(&mut content, apps.status(game));
	}
	response
}

/// A label that shortens itself with an ellipsis instead of drawing over the
/// toolbar's left side when the window is narrow; hovering shows it all.
fn fit(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
	ui.add(egui::Label::new(text).truncate())
}

fn thumbnail(ui: &mut egui::Ui, catalog: &Catalog, game: &Game, size: egui::Vec2) {
	match catalog.thumbnail(&game.package) {
		Some(path) => {
			ui.add(
				egui::Image::new(format!("file://{}", path.display()))
					.max_size(size)
					.maintain_aspect_ratio(true)
					.corner_radius(3.0),
			);
		}
		None => {
			let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
			ui.painter()
				.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
		}
	}
}

fn details(
	ui: &mut egui::Ui,
	catalog: &Catalog,
	game: &Game,
	rank: Option<u32>,
	installed: Option<&InstalledApps<'_>>,
	commands: &mut Vec<Command>,
) -> bool {
	let status = installed.map(|apps| apps.status(game));
	let target = installed.map(|apps| (apps.serial, apps.device_name.as_str()));
	let mut close = false;
	ui.horizontal(|ui| {
		ui.heading(&game.name);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			close = ui
				.small_button("✕")
				.on_hover_text(t!("common.close"))
				.clicked();
		});
	});
	ui.horizontal_wrapped(|ui| {
		let install = ui.add_enabled(target.is_some(), egui::Button::new(install_label(status)));
		let install = match target {
			Some((_, name)) => install.on_hover_text(t!("lib.installs_on", name = name)),
			None => install.on_disabled_hover_text(t!("lib.connect_install")),
		};
		if install.clicked()
			&& let Some((serial, _)) = target
		{
			commands.push(Command::Download {
				game: game.clone(),
				serial: Some(serial.to_owned()),
			});
		}
		if ui.button(t!("lib.download_only")).clicked() {
			commands.push(Command::Download {
				game: game.clone(),
				serial: None,
			});
		}
	});
	if let Some((needed, free)) = installed.and_then(|apps| short_of_space(game, apps)) {
		ui.colored_label(
			ui.visuals().warn_fg_color,
			t!(
				"lib.low_space",
				needed = format_bytes(needed),
				free = format_bytes(free)
			),
		);
	}
	ui.add_space(4.0);
	egui::ScrollArea::vertical().show(ui, |ui| {
		let width = ui.available_width();
		thumbnail(ui, catalog, game, egui::vec2(width, width * 0.56));
		ui.add_space(8.0);
		egui::Grid::new("game_details_grid")
			.num_columns(2)
			.spacing([12.0, 4.0])
			.show(ui, |ui| {
				for (label, value) in [
					(t!("lib.release"), game.release_name.clone()),
					(t!("lib.package"), game.package.clone()),
					(
						t!("lib.col_version"),
						game.version_code.map(|v| v.to_string()).unwrap_or_default(),
					),
					(t!("lib.col_size"), size_text(game)),
					(
						t!("lib.col_updated"),
						super::mirror_time(&game.last_updated),
					),
					(t!("lib.col_popularity"), rank_text(rank)),
				] {
					ui.label(label);
					ui.add(egui::Label::new(value).wrap());
					ui.end_row();
				}
				if let Some(status) = status {
					ui.label(t!("lib.on_headset"));
					status_label(ui, status);
					ui.end_row();
				}
				if let Some(Some(version)) =
					installed.and_then(|apps| apps.versions.get(game.package.as_str()))
				{
					ui.label(t!("lib.col_installed"));
					ui.label(version.to_string());
					ui.end_row();
				}
			});
		if let Some(note) = catalog.note(&game.release_name) {
			ui.add_space(8.0);
			ui.strong(t!("lib.note"));
			ui.label(note);
		}
	});
	close
}

fn status_label(ui: &mut egui::Ui, status: GameStatus) {
	match status {
		GameStatus::NotInstalled => {}
		GameStatus::Installed => {
			ui.colored_label(
				egui::Color32::from_rgb(90, 180, 110),
				t!("lib.status_installed"),
			);
		}
		GameStatus::UpdateAvailable => {
			ui.colored_label(
				egui::Color32::from_rgb(230, 170, 60),
				t!("lib.status_update"),
			);
		}
	}
}

/// The game's size and the headset's free space, when the game won't fit.
/// The mirror lists the download size; the installed game takes about as much.
fn short_of_space(game: &Game, apps: &InstalledApps<'_>) -> Option<(u64, u64)> {
	let needed = (game.size_mb? * 1024.0 * 1024.0) as u64;
	let free = apps.free_bytes?;
	(needed > free).then_some((needed, free))
}

fn size_text(game: &Game) -> String {
	game.size_mb
		.map(|mb| format_bytes((mb * 1024.0 * 1024.0) as u64))
		.unwrap_or_default()
}

/// `2024-11-02 10:15 UTC` -> `2024-11-02`, in the computer's time zone.
fn short_date(value: &str) -> String {
	let local = super::mirror_time(value);
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
	fn status_against_device() {
		let apps = InstalledApps {
			device_name: "Quest 3".into(),
			serial: "S",
			versions: HashMap::from([("com.a", Some(10)), ("com.b", None)]),
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
		// Same name: the release name decides, still A to Z when reversed.
		assert_eq!(order(Sort::Name, false, &none), [2, 1, 0]);
		assert_eq!(order(Sort::Name, true, &none), [0, 2, 1]);
		assert_eq!(order(Sort::Package, false, &none), [1, 2, 0]);
		// Equal versions fall back to the name; a missing version is lowest.
		assert_eq!(order(Sort::Version, true, &none), [2, 0, 1]);
		assert_eq!(order(Sort::Size, true, &none), [0, 2, 1]);
		// Best rank first when descending, unranked last.
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
		let mut apps = InstalledApps {
			device_name: "Quest 3".into(),
			serial: "S",
			versions: HashMap::new(),
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
	fn saved_headset_choices_wait_for_a_headset() {
		let mut view = LibraryView::new();
		view.restore(LibraryPrefs {
			sort: Sort::Status,
			descending: true,
			filter: Filter::Updates,
			view: View::Grid,
		});
		assert_eq!(view.effective(false), (Filter::All, Sort::Name, false));
		assert_eq!(view.effective(true), (Filter::Updates, Sort::Status, true));
		let prefs = view.prefs();
		assert_eq!((prefs.sort, prefs.filter), (Sort::Status, Filter::Updates));
	}

	fn press(view: &mut LibraryView, ctx: &egui::Context, key: egui::Key) {
		let input = egui::RawInput {
			events: vec![egui::Event::Key {
				key,
				physical_key: None,
				pressed: true,
				repeat: false,
				modifiers: egui::Modifiers::NONE,
			}],
			..Default::default()
		};
		let mut output = ctx.run_ui(input, |ui| view.keyboard(ui.ctx()));
		// No renderer here to take the font texture.
		output.textures_delta.clear();
	}

	#[test]
	fn keys_move_the_selection() {
		let ctx = egui::Context::default();
		let mut view = LibraryView::new();
		view.rows = vec![5, 3, 8, 1, 7];
		press(&mut view, &ctx, egui::Key::ArrowDown);
		assert_eq!((view.selected, view.scroll_to), (Some(5), Some(0)));
		press(&mut view, &ctx, egui::Key::ArrowDown);
		assert_eq!(view.selected, Some(3));
		press(&mut view, &ctx, egui::Key::End);
		assert_eq!(view.selected, Some(7));
		press(&mut view, &ctx, egui::Key::ArrowDown);
		assert_eq!(view.selected, Some(7), "stops at the end");
		view.view = View::Grid;
		view.grid_columns = 2;
		press(&mut view, &ctx, egui::Key::ArrowUp);
		assert_eq!(view.selected, Some(8), "a grid line up");
		press(&mut view, &ctx, egui::Key::ArrowLeft);
		assert_eq!(view.selected, Some(3));
		press(&mut view, &ctx, egui::Key::Escape);
		assert_eq!(view.selected, None, "Escape closes the details first");
		view.search = "beat".into();
		press(&mut view, &ctx, egui::Key::Escape);
		assert!(view.search.is_empty(), "then clears the search");
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
