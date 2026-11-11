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
		if catalog.upload_config().is_none() {
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
			ui.label(format!(
				"{count} app(s) on {} are missing from the library or newer than its version.",
				installed.device_name
			));
			if ui.button("Review and share…").clicked() {
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

		egui::Window::new("Share app versions with the library")
			.open(&mut open)
			.collapsible(false)
			.default_width(560.0)
			.show(ctx, |ui| {
				ui.label(format!(
					"These apps on {device_name} are not in the library yet, or are newer than its copy. \
					 Uploading one helps everyone using this mirror."
				));
				ui.add_space(4.0);
				ui.weak(
					"An upload sends the app's APK and OBB files, the headset model, and an anonymous \
					 headset id (a SHA-256 hash of its serial number) to the mirror. Nothing is uploaded \
					 until you press Upload, and the headset itself is not changed.",
				);
				ui.separator();
				if candidates.is_empty() {
					ui.weak("Nothing left to share.");
					return;
				}
				egui::ScrollArea::vertical()
					.max_height(360.0)
					.show(ui, |ui| {
						egui::Grid::new("contribute_grid")
							.num_columns(3)
							.striped(true)
							.spacing([12.0, 6.0])
							.show(ui, |ui| {
								for candidate in &candidates {
									ui.vertical(|ui| {
										ui.label(&candidate.name);
										ui.small(&candidate.package);
									});
									ui.label(match &candidate.reason {
										Reason::NotInLibrary => format!(
											"v{} · not in the library",
											candidate.version_code
										),
										Reason::Newer { library_version } => {
											format!(
												"v{} · library has v{library_version}",
												candidate.version_code
											)
										}
									});
									ui.horizontal(|ui| {
										if self.requested.contains(&candidate.package) {
											ui.weak("sent to the queue");
										} else {
											if ui.button("Upload").clicked() {
												send = Some(candidate.clone());
											}
											if ui.small_button("Don't ask again").clicked() {
												ignore = Some(candidate.package.clone());
											}
										}
									});
									ui.end_row();
								}
							});
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
