use rust_i18n::t;
use std::path::{Path, PathBuf};

use quest_sideloader_core::Command;
use quest_sideloader_core::rename::{
	Analysis, ChangeKind, check_package, obb_name, output_dir, rename_value, suggest_package,
};

enum State {
	Analyzing,
	Ready(Box<Analysis>),
	Failed(String),
}

struct Item {
	id: u64,
	path: PathBuf,
	state: State,
	new_package: String,
	use_loader: bool,
	debuggable: bool,
	/// A build was started; its progress is on the Tasks tab.
	started: bool,
}

enum Action {
	None,
	Reanalyze,
	Build { new_package: String, title: String },
}

impl Item {
	fn title(&self) -> String {
		match &self.state {
			State::Ready(a) => a.title.clone(),
			_ => self
				.path
				.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_else(|| self.path.display().to_string()),
		}
	}
}

pub struct RenameView {
	items: Vec<Item>,
	selected: Option<u64>,
	next_id: u64,
	/// The patched Platform SDK loader the user picked, kept between runs.
	loader: Option<PathBuf>,
}

impl RenameView {
	pub fn new() -> Self {
		Self {
			items: Vec::new(),
			selected: None,
			next_id: 0,
			loader: None,
		}
	}

	pub fn loader(&self) -> Option<&Path> {
		self.loader.as_deref()
	}

	pub fn restore_loader(&mut self, path: PathBuf) {
		self.loader = Some(path);
	}

	/// Adds a release to the list and selects it; one already there is only selected.
	pub fn add(&mut self, path: PathBuf, commands: &mut Vec<Command>) {
		if let Some(item) = self.items.iter().find(|i| i.path == path) {
			self.selected = Some(item.id);
			return;
		}
		self.next_id += 1;
		let id = self.next_id;
		commands.push(Command::AnalyzeRename {
			id,
			path: path.clone(),
		});
		self.items.push(Item {
			id,
			path,
			state: State::Analyzing,
			new_package: String::new(),
			use_loader: false,
			debuggable: false,
			started: false,
		});
		self.selected = Some(id);
	}

	pub fn on_analysis(&mut self, id: u64, result: Result<Analysis, String>) {
		let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
			return;
		};
		item.state = match result {
			Ok(analysis) => {
				if item.new_package.is_empty() {
					item.new_package = suggest_package(&analysis.package);
				}
				State::Ready(Box::new(analysis))
			}
			Err(e) => State::Failed(e),
		};
	}

	fn reanalyze(&mut self, id: u64, commands: &mut Vec<Command>) {
		if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
			item.state = State::Analyzing;
			commands.push(Command::AnalyzeRename {
				id,
				path: item.path.clone(),
			});
		}
	}

	pub fn show(&mut self, ui: &mut egui::Ui, downloads_dir: &Path, commands: &mut Vec<Command>) {
		egui::Panel::top("rename_toolbar").show(ui, |ui| {
			ui.horizontal(|ui| {
				ui.heading(t!("rn.heading"));
				if ui.button(t!("rn.add_folder")).clicked()
					&& let Some(dir) = rfd::FileDialog::new()
						.set_directory(downloads_dir)
						.pick_folder()
				{
					self.add(dir, commands);
				}
				if ui.button(t!("rn.add_apk")).clicked()
					&& let Some(file) = rfd::FileDialog::new()
						.set_directory(downloads_dir)
						.add_filter("APK", &["apk"])
						.pick_file()
				{
					self.add(file, commands);
				}
			});
			ui.add_space(2.0);
		});
		if self.items.is_empty() {
			egui::CentralPanel::default().show(ui, |ui| {
				crate::theme::empty_state(ui, t!("rn.empty_title"), t!("rn.empty"));
			});
			return;
		}
		egui::Panel::left("rename_list")
			.resizable(true)
			.default_size(260.0)
			.show(ui, |ui| self.list(ui));
		egui::CentralPanel::default().show(ui, |ui| {
			let Some(index) = self
				.selected
				.and_then(|id| self.items.iter().position(|i| i.id == id))
			else {
				ui.centered_and_justified(|ui| ui.weak(t!("rn.select")));
				return;
			};
			let mut action = Action::None;
			egui::ScrollArea::vertical()
				.auto_shrink(false)
				.show(ui, |ui| {
					action = details(ui, &mut self.items[index], &mut self.loader, downloads_dir);
				});
			match action {
				Action::None => {}
				Action::Reanalyze => self.reanalyze(self.items[index].id, commands),
				Action::Build { new_package, title } => {
					let item = &mut self.items[index];
					item.started = true;
					commands.push(Command::BuildCopy {
						source: item.path.clone(),
						new_package,
						loader: self.loader.clone().filter(|_| item.use_loader),
						debuggable: item.debuggable,
						title,
					});
				}
			}
		});
	}

	fn list(&mut self, ui: &mut egui::Ui) {
		let mut remove = None;
		egui::ScrollArea::vertical().show(ui, |ui| {
			for item in &self.items {
				ui.horizontal(|ui| {
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
						if ui
							.small_button("✕")
							.on_hover_text(t!("rn.remove"))
							.clicked()
						{
							remove = Some(item.id);
						}
						ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
							let selected = self.selected == Some(item.id);
							let label = egui::Button::selectable(selected, item.title()).truncate();
							if ui.add(label).clicked() {
								self.selected = Some(item.id);
							}
						});
					});
				});
				ui.horizontal(|ui| {
					ui.add_space(8.0);
					match &item.state {
						State::Analyzing => {
							ui.spinner();
							ui.small(t!("rn.analyzing"));
						}
						State::Ready(a) => {
							ui.add(
								egui::Label::new(egui::RichText::new(&a.package).small().weak())
									.truncate(),
							);
						}
						State::Failed(_) => {
							ui.colored_label(ui.visuals().error_fg_color, t!("rn.failed"));
						}
					}
				});
				ui.add_space(4.0);
			}
		});
		if let Some(id) = remove {
			self.items.retain(|i| i.id != id);
			if self.selected == Some(id) {
				self.selected = self.items.first().map(|i| i.id);
			}
		}
	}
}

fn details(
	ui: &mut egui::Ui,
	item: &mut Item,
	loader: &mut Option<PathBuf>,
	downloads_dir: &Path,
) -> Action {
	let mut reanalyze = false;
	ui.horizontal(|ui| {
		ui.heading(item.title());
		let analyzing = matches!(item.state, State::Analyzing);
		reanalyze = ui
			.add_enabled(
				!analyzing,
				egui::Button::image(
					crate::icons::Icon::Refresh.image(16.0, ui.visuals().text_color()),
				),
			)
			.on_hover_text(t!("rn.reanalyze"))
			.clicked();
	});
	ui.weak(item.path.display().to_string());
	ui.add_space(8.0);

	let analysis = match &item.state {
		State::Analyzing => {
			ui.horizontal(|ui| {
				ui.spinner();
				ui.label(t!("rn.analyzing_long"));
			});
			return Action::None;
		}
		State::Failed(e) => {
			ui.colored_label(ui.visuals().error_fg_color, e);
			return if reanalyze {
				Action::Reanalyze
			} else {
				Action::None
			};
		}
		State::Ready(a) => a,
	};
	let old = analysis.package.as_str();

	egui::Grid::new("rename_summary")
		.num_columns(2)
		.spacing([16.0, 6.0])
		.show(ui, |ui| {
			ui.label(t!("rn.package"));
			ui.monospace(old);
			ui.end_row();
			ui.label(t!("rn.version"));
			let version = match (&analysis.version_name, analysis.version_code) {
				(Some(name), Some(code)) => format!("{name} ({code})"),
				(Some(name), None) => name.clone(),
				(None, Some(code)) => code.to_string(),
				(None, None) => "-".into(),
			};
			ui.label(version);
			ui.end_row();
			ui.label(t!("rn.new_package"));
			ui.horizontal(|ui| {
				ui.add(
					egui::TextEdit::singleline(&mut item.new_package)
						.font(egui::TextStyle::Monospace)
						.desired_width(280.0),
				);
				if ui.button(t!("rn.suggest")).clicked() {
					item.new_package = suggest_package(old);
				}
			});
			ui.end_row();
		});
	let new = item.new_package.trim();
	let problem = check_package(new, old);
	if let Some(problem) = &problem {
		ui.colored_label(ui.visuals().error_fg_color, problem);
	}
	// Without a valid name the preview would show nonsense.
	let new = if problem.is_none() { new } else { old };

	warnings(ui, analysis);

	ui.add_space(8.0);
	section(ui, &t!("rn.manifest", n = analysis.manifest.len()), |ui| {
		changes_table(
			ui,
			"rename_manifest",
			analysis.manifest.iter().map(|c| {
				let place = format!("<{}> {}", c.element, c.attribute);
				let note =
					(c.kind == ChangeKind::ExpandClass).then(|| t!("rn.expanded").into_owned());
				(place, c.value.clone(), c.new_value(old, new), note)
			}),
		);
	});
	if !analysis.resources.is_empty() {
		section(
			ui,
			&t!("rn.resources", n = analysis.resources.len()),
			|ui| {
				changes_table(
					ui,
					"rename_resources",
					analysis.resources.iter().map(|s| {
						let renamed = rename_value(s, old, new).unwrap_or_else(|| s.clone());
						(String::new(), s.clone(), renamed, None)
					}),
				);
			},
		);
	}
	if !analysis.obb_files.is_empty() {
		section(ui, &t!("rn.obb", n = analysis.obb_files.len()), |ui| {
			ui.label(format!("Android/obb/{old}  ->  Android/obb/{new}"));
			changes_table(
				ui,
				"rename_obb",
				analysis
					.obb_files
					.iter()
					.map(|f| (String::new(), f.clone(), obb_name(f, old, new), None)),
			);
		});
	}
	if !analysis.script_lines.is_empty() {
		section(
			ui,
			&t!("rn.script", n = analysis.script_lines.len()),
			|ui| {
				changes_table(
					ui,
					"rename_script",
					analysis.script_lines.iter().map(|l| {
						let renamed = rename_value(l, old, new).unwrap_or_else(|| l.clone());
						(String::new(), l.clone(), renamed, None)
					}),
				);
			},
		);
	}

	ui.add_space(12.0);
	ui.separator();
	if analysis.platform_loader.is_some() {
		ui.checkbox(&mut item.use_loader, t!("rn.use_loader"));
		ui.horizontal(|ui| {
			ui.add_space(24.0);
			match loader.as_deref() {
				Some(path) => ui.add(egui::Label::new(path.display().to_string()).truncate()),
				None => ui.weak(t!("rn.no_loader_file")),
			};
			if ui.button(t!("rn.pick_loader")).clicked()
				&& let Some(file) = rfd::FileDialog::new()
					.add_filter("libovrplatformloader.so", &["so"])
					.pick_file()
			{
				*loader = Some(file);
			}
		});
		ui.horizontal_wrapped(|ui| {
			ui.add_space(24.0);
			ui.weak(t!("rn.loader_hint"));
		});
		ui.add_space(6.0);
	}
	if analysis.debuggable {
		ui.weak(t!("rn.already_debuggable"));
	} else {
		ui.checkbox(&mut item.debuggable, t!("rn.debuggable"))
			.on_hover_text(t!("rn.debuggable_hint"));
	}
	ui.add_space(6.0);
	let out = output_dir(&item.path, downloads_dir, new);
	ui.weak(t!("rn.output", path = out.display()));
	let loader_missing = item.use_loader && loader.is_none();
	let can_build = problem.is_none() && !loader_missing && !out.exists();
	let mut build = false;
	ui.horizontal(|ui| {
		let button = ui.add_enabled(can_build, super::toned(t!("rn.build"), super::Tone::Go));
		let button = if out.exists() {
			button.on_disabled_hover_text(t!("rn.output_exists"))
		} else if loader_missing {
			button.on_disabled_hover_text(t!("rn.no_loader_file"))
		} else {
			button
		};
		build = button.clicked();
		if item.started {
			ui.weak(t!("rn.started"));
		}
	});
	if build {
		Action::Build {
			new_package: new.to_owned(),
			title: analysis.title.clone(),
		}
	} else if reanalyze {
		Action::Reanalyze
	} else {
		Action::None
	}
}

fn warnings(ui: &mut egui::Ui, analysis: &Analysis) {
	let warn = ui.visuals().warn_fg_color;
	if analysis.dex_count > 0 {
		ui.add_space(6.0);
		ui.colored_label(warn, t!("rn.dex_warning", n = analysis.dex_count));
		egui::CollapsingHeader::new(t!("rn.dex_examples"))
			.id_salt("rename_dex")
			.show(ui, |ui| {
				for s in &analysis.dex_strings {
					ui.add(egui::Label::new(egui::RichText::new(s).monospace()).truncate());
				}
			});
	}
	if analysis.firebase {
		ui.add_space(6.0);
		ui.colored_label(warn, t!("rn.firebase"));
	}
	if analysis.other_apks > 0 {
		ui.add_space(6.0);
		ui.colored_label(warn, t!("rn.other_apks", n = analysis.other_apks));
	}
	if !analysis.jni_libraries.is_empty() {
		ui.add_space(6.0);
		ui.weak(t!("rn.jni", n = analysis.jni_libraries.len()));
	}
	if let Some(hash) = &analysis.platform_loader {
		ui.add_space(6.0);
		ui.weak(t!("rn.loader"));
		ui.add(egui::Label::new(egui::RichText::new(hash).monospace().weak()).truncate());
	}
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
	ui.add_space(6.0);
	egui::CollapsingHeader::new(title)
		.default_open(true)
		.show(ui, body);
}

/// Rows of `(where, before, after, note)`; unchanged rows are shown dimmed.
fn changes_table(
	ui: &mut egui::Ui,
	id: &str,
	rows: impl Iterator<Item = (String, String, String, Option<String>)>,
) {
	let rows: Vec<_> = rows.collect();
	let with_place = rows.iter().any(|r| !r.0.is_empty());
	let mut table = egui_extras::TableBuilder::new(ui)
		.id_salt(id)
		.striped(true)
		.vscroll(false)
		.cell_layout(egui::Layout::left_to_right(egui::Align::Center));
	if with_place {
		table = table.column(egui_extras::Column::auto().at_most(220.0).clip(true));
	}
	table
		.column(egui_extras::Column::remainder().at_least(160.0).clip(true))
		.column(egui_extras::Column::remainder().at_least(160.0).clip(true))
		.body(|mut body| {
			for (place, before, after, note) in rows {
				body.row(20.0, |mut row| {
					if with_place {
						row.col(|ui| {
							ui.add(egui::Label::new(egui::RichText::new(place).small()).truncate());
						});
					}
					let same = before == after;
					row.col(|ui| {
						ui.add(
							egui::Label::new(egui::RichText::new(&before).monospace()).truncate(),
						);
					});
					row.col(|ui| {
						let mut text = egui::RichText::new(&after).monospace();
						if same {
							text = text.weak();
						}
						let label = ui.add(egui::Label::new(text).truncate());
						if let Some(note) = note {
							label.on_hover_text(note);
						}
					});
				});
			}
		});
}
