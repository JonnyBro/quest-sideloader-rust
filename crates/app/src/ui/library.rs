use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use egui_extras::{Column, TableBuilder};
use quest_sideloader_core::catalog::{Catalog, Game};
use quest_sideloader_core::{CatalogStatus, Command, format_bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Filter {
	All,
	Installed,
	Updates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sort {
	Name,
	Updated,
	Size,
	Popularity,
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
		}
	}

	pub fn on_catalog(&mut self, catalog: Option<Arc<Catalog>>) {
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

	fn refresh_rows(&mut self, installed: Option<&InstalledApps<'_>>) {
		let Some(catalog) = &self.catalog else {
			self.rows.clear();
			return;
		};
		let key = RowsKey {
			catalog: Arc::as_ptr(catalog) as usize,
			search: self.search.trim().to_lowercase(),
			filter: self.filter,
			sort: self.sort,
			descending: self.descending,
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
				let matches_filter = match self.filter {
					Filter::All => true,
					Filter::Installed => status != GameStatus::NotInstalled,
					Filter::Updates => status == GameStatus::UpdateAvailable,
				};
				matches_search && matches_filter
			})
			.collect();

		let by_name =
			|a: &usize, b: &usize| self.lowercase_names[*a].cmp(&self.lowercase_names[*b]);
		match self.sort {
			Sort::Name => rows.sort_by(by_name),
			Sort::Updated => rows.sort_by(|a, b| {
				games[*a]
					.last_updated
					.cmp(&games[*b].last_updated)
					.then(by_name(a, b))
			}),
			Sort::Size => rows.sort_by(|a, b| {
				games[*a]
					.size_mb
					.unwrap_or(0.0)
					.total_cmp(&games[*b].size_mb.unwrap_or(0.0))
					.then(by_name(a, b))
			}),
			Sort::Popularity => rows.sort_by(|a, b| {
				games[*a]
					.downloads
					.unwrap_or(0.0)
					.total_cmp(&games[*b].downloads.unwrap_or(0.0))
					.then(by_name(a, b))
			}),
		}
		if self.descending {
			rows.reverse();
		}
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
					(None, _) => "No mirror yet. Add one in Settings → Mirrors.",
					(Some(_), CatalogStatus::Syncing { .. }) => "Downloading the game list…",
					(Some(_), CatalogStatus::Failed(_)) => {
						"Could not load the game list. Check the mirror settings and press Sync."
					}
					(Some(_), CatalogStatus::Idle) => "The game list is empty. Press Sync.",
				});
			});
			return;
		};
		self.refresh_rows(installed);

		if let Some(index) = self.selected
			&& let Some(game) = catalog.games.get(index)
		{
			let status = installed.map(|apps| apps.status(game));
			let target = installed.map(|apps| (apps.serial, apps.device_name.as_str()));
			let mut close = false;
			egui::Panel::right("game_details")
				.resizable(true)
				.default_size(320.0)
				.show(ui, |ui| {
					close = details(ui, &catalog, game, status, target, commands)
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
					.hint_text("Search games or packages")
					.desired_width(240.0),
			);
			ui.selectable_value(&mut self.filter, Filter::All, "All");
			let enabled = installed.is_some();
			ui.add_enabled_ui(enabled, |ui| {
				ui.selectable_value(&mut self.filter, Filter::Installed, "Installed");
				ui.selectable_value(&mut self.filter, Filter::Updates, "Updates");
			})
			.response
			.on_disabled_hover_text("Connect a headset to compare with its apps");
			if !enabled && self.filter != Filter::All {
				self.filter = Filter::All;
			}

			ui.separator();
			egui::ComboBox::from_id_salt("library_sort")
				.selected_text(match self.sort {
					Sort::Name => "Name",
					Sort::Updated => "Last updated",
					Sort::Size => "Size",
					Sort::Popularity => "Popularity",
				})
				.show_ui(ui, |ui| {
					for (sort, label, descending) in [
						(Sort::Name, "Name", false),
						(Sort::Updated, "Last updated", true),
						(Sort::Size, "Size", true),
						(Sort::Popularity, "Popularity", true),
					] {
						if ui.selectable_label(self.sort == sort, label).clicked() {
							self.sort = sort;
							self.descending = descending;
						}
					}
				});
			let arrow = if self.descending { "⬇" } else { "⬆" };
			if ui.button(arrow).on_hover_text("Reverse order").clicked() {
				self.descending = !self.descending;
			}

			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				let syncing = matches!(self.status, CatalogStatus::Syncing { .. });
				if mirror_name.is_some()
					&& ui
						.add_enabled(!syncing, egui::Button::new("Sync"))
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
						ui.label(stage);
					}
					CatalogStatus::Failed(e) => {
						ui.colored_label(ui.visuals().error_fg_color, "Sync failed")
							.on_hover_text(e);
					}
					CatalogStatus::Idle => {
						if let Some(catalog) = &self.catalog {
							ui.weak(format!(
								"{} games · synced {}",
								catalog.games.len(),
								ago(catalog.synced_at)
							));
						}
					}
				}
				if let Some(name) = mirror_name {
					ui.label(name);
				}
				if let Some(apps) = installed {
					ui.weak(format!("Comparing with {} ·", apps.device_name));
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
			ui.centered_and_justified(|ui| ui.weak("Nothing matches the filter."));
			return;
		}
		let row_height = 44.0;
		let mut clicked = None;
		TableBuilder::new(ui)
			.striped(true)
			.sense(egui::Sense::click())
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(Column::exact(80.0))
			.column(Column::remainder().at_least(200.0).clip(true))
			.column(Column::auto().at_least(80.0))
			.column(Column::auto().at_least(70.0))
			.column(Column::auto().at_least(120.0))
			.column(Column::auto().at_least(100.0))
			.header(22.0, |mut header| {
				for title in ["", "Game", "Version", "Size", "Updated", "Status"] {
					header.col(|ui| {
						ui.strong(title);
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
		if let Some(index) = clicked {
			self.selected = if self.selected == Some(index) {
				None
			} else {
				Some(index)
			};
		}
	}
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
	status: Option<GameStatus>,
	target: Option<(&str, &str)>,
	commands: &mut Vec<Command>,
) -> bool {
	let mut close = false;
	ui.horizontal(|ui| {
		ui.heading(&game.name);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			close = ui.small_button("✕").clicked();
		});
	});
	ui.horizontal_wrapped(|ui| {
		let label = match status {
			Some(GameStatus::UpdateAvailable) => "Download and update",
			Some(GameStatus::Installed) => "Download and reinstall",
			_ => "Download and install",
		};
		let install = ui.add_enabled(target.is_some(), egui::Button::new(label));
		let install = match target {
			Some((_, name)) => install.on_hover_text(format!("Installs on {name}")),
			None => install.on_disabled_hover_text("Connect a headset to install"),
		};
		if install.clicked()
			&& let Some((serial, _)) = target
		{
			commands.push(Command::Download {
				game: game.clone(),
				serial: Some(serial.to_owned()),
			});
		}
		if ui.button("Download only").clicked() {
			commands.push(Command::Download {
				game: game.clone(),
				serial: None,
			});
		}
	});
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
					("Release", game.release_name.clone()),
					("Package", game.package.clone()),
					(
						"Version",
						game.version_code.map(|v| v.to_string()).unwrap_or_default(),
					),
					("Size", size_text(game)),
					("Updated", game.last_updated.clone()),
				] {
					ui.label(label);
					ui.add(egui::Label::new(value).wrap());
					ui.end_row();
				}
				if let Some(status) = status {
					ui.label("On headset");
					status_label(ui, status);
					ui.end_row();
				}
			});
		if let Some(note) = catalog.note(&game.release_name) {
			ui.add_space(8.0);
			ui.strong("Note");
			ui.label(note);
		}
	});
	close
}

fn status_label(ui: &mut egui::Ui, status: GameStatus) {
	match status {
		GameStatus::NotInstalled => {}
		GameStatus::Installed => {
			ui.colored_label(egui::Color32::from_rgb(90, 180, 110), "Installed");
		}
		GameStatus::UpdateAvailable => {
			ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "Update available");
		}
	}
}

fn size_text(game: &Game) -> String {
	game.size_mb
		.map(|mb| format_bytes((mb * 1024.0 * 1024.0) as u64))
		.unwrap_or_default()
}

/// `2024-11-02 10:15 UTC` → `2024-11-02`.
fn short_date(value: &str) -> &str {
	value.split_whitespace().next().unwrap_or(value)
}

fn ago(time: SystemTime) -> String {
	let secs = SystemTime::now()
		.duration_since(time)
		.map_or(0, |d| d.as_secs());
	match secs {
		0..60 => "just now".into(),
		60..3600 => format!("{} min ago", secs / 60),
		3600..86_400 => format!("{} h ago", secs / 3600),
		_ => format!("{} days ago", secs / 86_400),
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
	fn dates() {
		assert_eq!(short_date("2024-11-02 10:15 UTC"), "2024-11-02");
		assert_eq!(short_date(""), "");
	}
}
