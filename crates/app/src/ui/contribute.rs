use rust_i18n::t;
use std::collections::HashSet;
use std::sync::Arc;

use quest_sideloader_core::Command;
use quest_sideloader_core::catalog::Catalog;
use quest_sideloader_core::contribute::{Candidate, Reason, find_candidates};

use super::library::InstalledApps;

#[derive(PartialEq)]
struct Key {
	catalog: usize,
	serial: String,
	installed: usize,
	ignored: usize,
}

/// Offers app versions the library lacks; each upload needs the user's click.
pub struct ContributeView {
	open: bool,
	/// Packages already sent this session, so they aren't offered twice.
	requested: HashSet<String>,
	cache: Option<(Key, Vec<Candidate>)>,
}

impl ContributeView {
	pub fn new() -> Self {
		Self {
			open: false,
			requested: HashSet::new(),
			cache: None,
		}
	}

	fn candidates(
		&mut self,
		catalog: &Arc<Catalog>,
		installed: &InstalledApps<'_>,
		ignored: &HashSet<String>,
	) -> &[Candidate] {
		let key = Key {
			catalog: Arc::as_ptr(catalog) as usize,
			serial: installed.serial.to_owned(),
			installed: installed.versions.len(),
			ignored: ignored.len(),
		};
		if self.cache.as_ref().is_none_or(|(k, _)| *k != key) {
			let apps: Vec<(&str, Option<u64>)> =
				installed.versions.iter().map(|(p, v)| (*p, *v)).collect();
			self.cache = Some((key, find_candidates(catalog, &apps, ignored)));
		}
		self.cache.as_ref().map_or(&[], |(_, c)| c.as_slice())
	}

	/// A one-line hint above the library; opens the review window.
	pub fn banner(
		&mut self,
		ui: &mut egui::Ui,
		catalog: &Arc<Catalog>,
		installed: &InstalledApps<'_>,
		ignored: &HashSet<String>,
	) {
		if catalog.upload_target().is_none() {
			return;
		}
		let requested = self.requested.clone();
		let count = self
			.candidates(catalog, installed, ignored)
			.iter()
			.filter(|c| !requested.contains(&c.package))
			.count();
		if count == 0 {
			return;
		}
		ui.horizontal(|ui| {
			ui.label(t!("con.banner", n = count, device = installed.device_name));
			if ui.button(t!("con.review")).clicked() {
				self.open = true;
			}
		});
	}

	pub fn window(
		&mut self,
		ctx: &egui::Context,
		catalog: &Arc<Catalog>,
		installed: &InstalledApps<'_>,
		ignored: &HashSet<String>,
		commands: &mut Vec<Command>,
	) {
		if !self.open {
			return;
		}
		let serial = installed.serial.to_owned();
		let device_name = installed.device_name.clone();
		let candidates = self.candidates(catalog, installed, ignored).to_vec();
		let mut open = self.open;
		let mut send = None;
		let mut ignore = None;

		egui::Window::new(t!("con.title"))
			.id(egui::Id::unique("contribute_window"))
			.open(&mut open)
			.collapsible(false)
			.default_width(560.0)
			.show(ctx, |ui| {
				ui.label(t!("con.intro", device = device_name));
				ui.add_space(4.0);
				ui.weak(t!("con.privacy"));
				ui.separator();
				if candidates.is_empty() {
					ui.weak(t!("con.nothing"));
					return;
				}
				egui_extras::TableBuilder::new(ui)
					.id_salt("contribute_table")
					.striped(true)
					.max_scroll_height(360.0)
					.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
					.column(egui_extras::Column::remainder().at_least(180.0).clip(true))
					.column(egui_extras::Column::auto())
					.column(egui_extras::Column::auto())
					.body(|mut body| {
						for candidate in &candidates {
							body.row(40.0, |mut row| {
								row.col(|ui| {
									ui.vertical(|ui| {
										ui.add(egui::Label::new(&candidate.name).truncate());
										ui.add(
											egui::Label::new(
												egui::RichText::new(&candidate.package).small(),
											)
											.truncate(),
										);
									});
								});
								row.col(|ui| {
									ui.label(match &candidate.reason {
										Reason::NotInLibrary => t!(
											"con.not_in_library",
											version = candidate.version_code
										),
										Reason::Newer { library_version } => t!(
											"con.newer",
											version = candidate.version_code,
											library = library_version
										),
									});
								});
								row.col(|ui| {
									if self.requested.contains(&candidate.package) {
										ui.weak(t!("con.queued"));
									} else {
										if ui.button(t!("con.upload")).clicked() {
											send = Some(candidate.clone());
										}
										if ui.small_button(t!("con.ignore")).clicked() {
											ignore = Some(candidate.package.clone());
										}
									}
								});
							});
						}
					});
			});
		self.open = open;

		if let Some(candidate) = send {
			self.requested.insert(candidate.package.clone());
			commands.push(Command::Contribute {
				serial,
				package: candidate.package,
				name: candidate.name,
			});
		}
		if let Some(package) = ignore {
			commands.push(Command::IgnoreContribution(package));
		}
	}
}
