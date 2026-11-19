use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use egui_extras::{Column, TableBuilder};
use quest_sideloader_core::catalog::{Catalog, Game};
use quest_sideloader_core::{CatalogStatus, Command, format_bytes};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Filter {
	All,
	Installed,
	Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Sort {
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

/// The library's sort and filter, kept between runs.
#[derive(Serialize, Deserialize)]
pub struct LibraryPrefs {
	sort: Sort,
	descending: bool,
	filter: Filter,
}

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
	status: CatalogStatus,
	search: String,
	filter: Filter,
	sort: Sort,
	descending: bool,
	selected: Option<usize>,
	rows: Vec<usize>,
	rows_key: Option<RowsKey>,
	contribute: super::contribute::ContributeView,
	names: HashMap<String, String>,
}

impl LibraryView {
	pub fn new() -> Self {
		Self {
			catalog: None,
			lowercase_names: Vec::new(),
			status: CatalogStatus::Idle,
			search: String::new(),
			filter: Filter::All,
			sort: Sort::Name,
			descending: false,
			selected: None,
			rows: Vec::new(),
			rows_key: None,
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
		}
	}

	pub fn restore(&mut self, prefs: LibraryPrefs) {
		self.sort = prefs.sort;
		self.descending = prefs.descending;
		self.filter = prefs.filter;
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

		if let Some(index) = self.selected
			&& let Some(game) = catalog.games.get(index)
		{
			let mut close = false;
			egui::Panel::right("game_details")
				.resizable(true)
				.default_size(320.0)
				.show(ui, |ui| {
					close = details(ui, &catalog, game, installed, commands)
				});
			if close {
				self.selected = None;
			}
		}

		egui::CentralPanel::default().show(ui, |ui| self.table(ui, &catalog, installed));
	}

	fn toolbar(
		&mut self,
		ui: &mut egui::Ui,
		mirror_name: Option<&str>,
		installed: Option<&InstalledApps<'_>>,
		commands: &mut Vec<Command>,
	) {
		ui.horizontal(|ui| {
			ui.add(
				egui::TextEdit::singleline(&mut self.search)
					.hint_text(t!("lib.search"))
					.desired_width(240.0),
			);
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
	) {
		if self.rows.is_empty() {
			ui.centered_and_justified(|ui| ui.weak(t!("lib.no_match")));
			return;
		}
		let row_height = 44.0;
		let mut clicked = None;
		let mut sort_click = None;
		let (_, current, descending) = self.effective(installed.is_some());
		let mut table = TableBuilder::new(ui)
			.striped(true)
			.sense(egui::Sense::click())
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(Column::exact(80.0))
			.column(Column::remainder().at_least(200.0).clip(true))
			.column(Column::auto().at_least(80.0));
		// The installed version only means something with a headset.
		if installed.is_some() {
			table = table.column(Column::auto().at_least(80.0));
		}
		table
			.column(Column::auto().at_least(70.0))
			.column(Column::auto().at_least(120.0))
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
						if let Some(apps) = installed {
							status_label(ui, apps.status(game));
						}
					});
					if row.response().clicked() {
						clicked = Some(index);
					}
				});
			});
		if let Some(sort) = sort_click {
			self.set_sort(sort);
		}
		if let Some(index) = clicked {
			self.selected = if self.selected == Some(index) {
				None
			} else {
				Some(index)
			};
		}
	}
}

/// `names` are the games' lowercase names; `rank` gives each game's
/// `status_rank` on the compared headset.
fn sort_rows(
	rows: &mut [usize],
	games: &[Game],
	names: &[String],
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
			Sort::Popularity => ga
				.downloads
				.unwrap_or(0.0)
				.total_cmp(&gb.downloads.unwrap_or(0.0)),
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
		let label = match status {
			Some(GameStatus::UpdateAvailable) => t!("lib.download_update"),
			Some(GameStatus::Installed) => t!("lib.download_reinstall"),
			_ => t!("lib.download_install"),
		};
		let install = ui.add_enabled(target.is_some(), egui::Button::new(label));
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
					(t!("lib.col_updated"), game.last_updated.clone()),
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

/// `2024-11-02 10:15 UTC` -> `2024-11-02`.
fn short_date(value: &str) -> &str {
	value.split_whitespace().next().unwrap_or(value)
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
		let order = |sort, descending, rank: &dyn Fn(usize) -> u8| {
			let mut rows = vec![0, 1, 2];
			sort_rows(&mut rows, &games, &names, sort, descending, rank);
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
		let rank = |i| [1, 2, 0][i];
		assert_eq!(order(Sort::Status, false, &rank), [2, 0, 1]);
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
		});
		assert_eq!(view.effective(false), (Filter::All, Sort::Name, false));
		assert_eq!(view.effective(true), (Filter::Updates, Sort::Status, true));
		let prefs = view.prefs();
		assert_eq!((prefs.sort, prefs.filter), (Sort::Status, Filter::Updates));
	}

	#[test]
	fn dates() {
		assert_eq!(short_date("2024-11-02 10:15 UTC"), "2024-11-02");
		assert_eq!(short_date(""), "");
	}
}
