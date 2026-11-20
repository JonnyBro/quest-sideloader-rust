use rust_i18n::t;
use std::path::{Path, PathBuf};

use quest_sideloader_core::Command;
use quest_sideloader_core::rename::Inspection;

use super::format_bytes;

enum State {
	Empty,
	Loading,
	Ready(Box<Inspection>),
	Failed(String),
}

pub struct InspectorView {
	path: Option<PathBuf>,
	id: u64,
	state: State,
}

impl InspectorView {
	pub fn new() -> Self {
		Self {
			path: None,
			id: 0,
			state: State::Empty,
		}
	}

	/// Reads `path` (an APK or a release folder) and shows it.
	pub fn open(&mut self, path: PathBuf, commands: &mut Vec<Command>) {
		self.id += 1;
		commands.push(Command::Inspect {
			id: self.id,
			path: path.clone(),
		});
		self.path = Some(path);
		self.state = State::Loading;
	}

	pub fn on_result(&mut self, id: u64, result: Result<Box<Inspection>, String>) {
		if id != self.id {
			return;
		}
		self.state = match result {
			Ok(inspection) => State::Ready(inspection),
			Err(e) => State::Failed(e),
		};
	}

	pub fn show(&mut self, ui: &mut egui::Ui, downloads_dir: &Path, commands: &mut Vec<Command>) {
		ui.horizontal(|ui| {
			ui.heading(t!("ins.heading"));
			if ui.button(t!("rn.add_apk")).clicked()
				&& let Some(file) = rfd::FileDialog::new()
					.set_directory(downloads_dir)
					.add_filter("APK", &["apk"])
					.pick_file()
			{
				self.open(file, commands);
			}
			if ui.button(t!("rn.add_folder")).clicked()
				&& let Some(dir) = rfd::FileDialog::new()
					.set_directory(downloads_dir)
					.pick_folder()
			{
				self.open(dir, commands);
			}
			if let Some(path) = self.path.clone()
				&& ui
					.add_enabled(
						!matches!(self.state, State::Loading),
						egui::Button::image(
							crate::icons::Icon::Refresh.image(16.0, ui.visuals().text_color()),
						),
					)
					.on_hover_text(t!("rn.reanalyze"))
					.clicked()
			{
				self.open(path, commands);
			}
		});
		if let Some(path) = &self.path {
			ui.weak(path.display().to_string());
		}
		ui.separator();
		match &self.state {
			State::Empty => {
				crate::theme::empty_state(ui, t!("ins.empty_title"), t!("ins.empty"));
			}
			State::Loading => {
				ui.horizontal(|ui| {
					ui.spinner();
					ui.label(t!("ins.reading"));
				});
			}
			State::Failed(e) => {
				ui.colored_label(ui.visuals().error_fg_color, e);
			}
			State::Ready(info) => {
				egui::ScrollArea::vertical()
					.auto_shrink(false)
					.show(ui, |ui| details(ui, info));
			}
		}
	}
}

fn yes_no(value: bool) -> String {
	if value {
		t!("common.yes")
	} else {
		t!("common.no")
	}
	.into_owned()
}

fn grid(ui: &mut egui::Ui, id: &str, rows: Vec<(String, String)>) {
	egui::Grid::new(id)
		.num_columns(2)
		.spacing([16.0, 4.0])
		.show(ui, |ui| {
			for (label, value) in rows {
				ui.label(label);
				ui.add(egui::Label::new(value).wrap());
				ui.end_row();
			}
		});
}

fn section(
	ui: &mut egui::Ui,
	title: impl Into<egui::WidgetText>,
	body: impl FnOnce(&mut egui::Ui),
) {
	ui.add_space(4.0);
	egui::CollapsingHeader::new(title)
		.default_open(true)
		.show(ui, body);
}

fn list(ui: &mut egui::Ui, items: &[String]) {
	if items.is_empty() {
		ui.weak("-");
	}
	for item in items {
		ui.monospace(item);
	}
}

fn details(ui: &mut egui::Ui, info: &Inspection) {
	let dash = || "-".to_owned();
	let version = match (&info.version_name, info.version_code) {
		(Some(name), Some(code)) => format!("{name} ({code})"),
		(Some(name), None) => name.clone(),
		(None, Some(code)) => code.to_string(),
		(None, None) => dash(),
	};
	section(ui, t!("ins.app"), |ui| {
		grid(
			ui,
			"ins_app",
			vec![
				(
					t!("ins.label").into(),
					info.label.clone().unwrap_or_else(dash),
				),
				(t!("rn.package").into(), info.package.clone()),
				(t!("rn.version").into(), version),
				(t!("ins.size").into(), format_bytes(info.size)),
				(
					t!("ins.sdk").into(),
					t!(
						"ins.sdk_values",
						min = info.min_sdk.clone().unwrap_or_else(dash),
						target = info.target_sdk.clone().unwrap_or_else(dash),
						compile = info.compile_sdk.clone().unwrap_or_else(dash)
					)
					.into(),
				),
				("OpenGL ES".into(), info.gl_es.clone().unwrap_or_else(dash)),
				("debuggable".into(), yes_no(info.debuggable)),
				(
					"extractNativeLibs".into(),
					info.extract_native_libs.map_or_else(dash, yes_no),
				),
			],
		);
	});

	section(ui, "Quest", |ui| {
		let devices = if info.supported_devices.is_empty() {
			dash()
		} else {
			info.supported_devices.join(", ")
		};
		grid(
			ui,
			"ins_quest",
			vec![
				(t!("ins.devices").into(), devices),
				(t!("ins.loader").into(), yes_no(info.platform_loader)),
				("Firebase".into(), yes_no(info.firebase)),
			],
		);
	});

	section(ui, t!("ins.signature"), |ui| {
		let sig = &info.signature;
		let schemes = if sig.schemes.is_empty() {
			t!("ins.unsigned").into_owned()
		} else {
			sig.schemes.join(", ")
		};
		let check = match &sig.v2_valid {
			Some(Ok(())) => t!("ins.valid").into_owned(),
			Some(Err(e)) => t!("ins.invalid", error = e).into_owned(),
			None => dash(),
		};
		let mut rows = vec![
			(t!("ins.schemes").into(), schemes),
			(t!("ins.v2_check").into(), check),
		];
		for cert in &sig.certificates {
			rows.push((t!("ins.certificate").into(), cert.subject.clone()));
			rows.push(("SHA-256".into(), cert.sha256.clone()));
		}
		grid(ui, "ins_signature", rows);
		if let Some(package) = &sig.ours {
			ui.label(t!("ins.ours", package = package));
		}
	});

	section(ui, t!("ins.contents"), |ui| {
		let components = info
			.components
			.iter()
			.map(|(kind, n)| format!("{kind}: {n}"))
			.collect::<Vec<_>>()
			.join(", ");
		grid(
			ui,
			"ins_contents",
			vec![
				(t!("ins.abis").into(), join_or_dash(&info.abis)),
				(t!("ins.locales").into(), join_or_dash(&info.locales)),
				("dex".into(), info.dex_files.to_string()),
				(t!("ins.native").into(), info.native_libraries.to_string()),
				(t!("ins.entries").into(), info.entries.to_string()),
				(t!("ins.components").into(), components),
			],
		);
	});

	section(
		ui,
		t!("ins.permissions", n = info.permissions.len()),
		|ui| {
			list(ui, &info.permissions);
		},
	);
	section(ui, t!("ins.features", n = info.features.len()), |ui| {
		let features: Vec<String> = info
			.features
			.iter()
			.map(|f| {
				if f.required {
					f.name.clone()
				} else {
					format!("{} ({})", f.name, t!("ins.optional"))
				}
			})
			.collect();
		list(ui, &features);
	});
	section(ui, t!("ins.hashes"), |ui| {
		grid(
			ui,
			"ins_hashes",
			vec![
				("SHA-256".into(), info.sha256.clone()),
				("MD5".into(), info.md5.clone()),
			],
		);
	});
}

fn join_or_dash(items: &[String]) -> String {
	if items.is_empty() {
		"-".into()
	} else {
		items.join(", ")
	}
}
