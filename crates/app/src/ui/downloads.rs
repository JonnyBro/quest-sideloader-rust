use rust_i18n::t;
use std::path::Path;

use quest_sideloader_core::Command;
use quest_sideloader_core::downloaded::DownloadedRelease;

use super::format_bytes;

pub struct DownloadsView {
	releases: Option<Vec<DownloadedRelease>>,
	/// Ask the engine for the list next time the tab is shown.
	stale: bool,
	search: String,
	confirm_delete: Option<DownloadedRelease>,
}

impl DownloadsView {
	pub fn new() -> Self {
		Self {
			releases: None,
			stale: true,
			search: String::new(),
			confirm_delete: None,
		}
	}

	pub fn on_list(&mut self, releases: Vec<DownloadedRelease>) {
		self.releases = Some(releases);
	}

	pub fn invalidate(&mut self) {
		self.stale = true;
	}

	/// `install_target` is `(serial, name)` of the headset to install on.
	/// Returns a release the user wants to make a renamed copy of.
	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		folder: &Path,
		install_target: Option<(&str, &str)>,
		commands: &mut Vec<Command>,
	) -> Option<std::path::PathBuf> {
		let mut rename = None;
		if self.stale {
			self.stale = false;
			commands.push(Command::ListDownloads);
		}
		ui.horizontal(|ui| {
			ui.heading(t!("dl.heading"));
			if ui.button("⟳").on_hover_text(t!("common.refresh")).clicked() {
				commands.push(Command::ListDownloads);
			}
			if ui.button(t!("common.open_folder")).clicked() {
				super::open_folder(folder);
			}
			ui.add(
				egui::TextEdit::singleline(&mut self.search)
					.hint_text(t!("common.search"))
					.desired_width(220.0),
			);
			if let Some(releases) = &self.releases {
				let total: u64 = releases.iter().map(|r| r.size).sum();
				ui.weak(t!(
					"dl.summary",
					n = releases.len(),
					size = format_bytes(total)
				));
			}
		});
		ui.weak(folder.display().to_string());
		ui.separator();

		let Some(releases) = &self.releases else {
			ui.spinner();
			return None;
		};
		if releases.is_empty() {
			ui.centered_and_justified(|ui| {
				ui.weak(t!("dl.empty"));
			});
			return None;
		}
		let needle = self.search.trim().to_lowercase();
		let rows: Vec<&DownloadedRelease> = releases
			.iter()
			.filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
			.collect();

		egui_extras::TableBuilder::new(ui)
			.id_salt("downloaded_releases")
			.striped(true)
			.auto_shrink([false, false])
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(egui_extras::Column::remainder().at_least(240.0).clip(true))
			.column(egui_extras::Column::auto().at_least(80.0))
			.column(egui_extras::Column::auto().at_least(120.0))
			.column(egui_extras::Column::auto())
			.header(22.0, |mut header| {
				for title in [
					t!("dl.col_release"),
					t!("dl.col_size"),
					t!("dl.col_date"),
					"".into(),
				] {
					header.col(|ui| {
						ui.strong(title);
					});
				}
			})
			.body(|mut body| {
				for release in rows {
					body.row(28.0, |mut row| {
						row.col(|ui| {
							ui.add(egui::Label::new(&release.name).truncate());
						});
						row.col(|ui| {
							ui.label(format_bytes(release.size));
						});
						row.col(|ui| {
							ui.label(
								release
									.modified
									.map(super::format_unix_time)
									.unwrap_or_default(),
							);
						});
						row.col(|ui| {
							let install = ui.add_enabled(
								install_target.is_some(),
								egui::Button::new(t!("common.install")).small(),
							);
							let install = match install_target {
								Some((_, name)) => {
									install.on_hover_text(t!("common.install_on", name = name))
								}
								None => install.on_disabled_hover_text(t!("dl.connect_first")),
							};
							if install.clicked()
								&& let Some((serial, _)) = install_target
							{
								commands.push(Command::Install {
									serial: serial.to_owned(),
									path: release.path.clone(),
								});
							}
							if ui
								.small_button(t!("dl.copy"))
								.on_hover_text(t!("dl.copy_hint"))
								.clicked()
							{
								rename = Some(release.path.clone());
							}
							if ui.small_button(t!("common.open")).clicked() {
								super::open_folder(&release.path);
							}
							if ui.small_button(t!("common.delete")).clicked() {
								self.confirm_delete = Some(release.clone());
							}
						});
					});
				}
			});

		self.delete_dialog(ui.ctx(), commands);
		rename
	}

	fn delete_dialog(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
		let Some(release) = &self.confirm_delete else {
			return;
		};
		let mut close = false;
		let modal = egui::Modal::new(egui::Id::unique("delete_download")).show(ctx, |ui| {
			ui.set_max_width(440.0);
			ui.heading(t!("dl.delete_title"));
			ui.label(t!(
				"dl.delete_text",
				name = release.name,
				size = format_bytes(release.size)
			));
			ui.horizontal(|ui| {
				if ui.button(t!("common.delete")).clicked() {
					commands.push(Command::DeleteDownload(release.name.clone()));
					close = true;
				}
				close |= ui.button(t!("common.cancel")).clicked();
			});
		});
		if close || modal.should_close() {
			self.confirm_delete = None;
		}
	}
}
