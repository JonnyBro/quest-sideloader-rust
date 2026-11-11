use std::path::{Path, PathBuf};

use quest_sideloader_core::Settings;
use quest_sideloader_core::catalog::{Header, Mirror, MirrorKind, rclone_remotes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormKind {
	VrpJson,
	Http,
	Rclone,
}

/// The "add mirror" form.
pub struct MirrorForm {
	open: bool,
	kind: FormKind,
	name: String,
	json: String,
	base_uri: String,
	password: String,
	headers: Vec<(String, String)>,
	rclone_file: Option<PathBuf>,
	remotes: Vec<String>,
	remote: String,
	error: Option<String>,
}

impl MirrorForm {
	pub fn new() -> Self {
		Self {
			open: false,
			kind: FormKind::VrpJson,
			name: String::new(),
			json: String::new(),
			base_uri: String::new(),
			password: String::new(),
			headers: Vec::new(),
			rclone_file: None,
			remotes: Vec::new(),
			remote: String::new(),
			error: None,
		}
	}

	fn headers(&self) -> Vec<Header> {
		self.headers
			.iter()
			.filter(|(name, _)| !name.trim().is_empty())
			.map(|(name, value)| Header {
				name: name.trim().to_owned(),
				value: value.trim().to_owned(),
			})
			.collect()
	}

	fn name_or(&self, fallback: &str) -> String {
		let name = self.name.trim();
		if name.is_empty() {
			fallback.to_owned()
		} else {
			name.to_owned()
		}
	}

	fn build(&self, rclone_dir: &Path) -> Result<Mirror, String> {
		let mirror = match self.kind {
			FormKind::VrpJson => {
				let mut mirror = Mirror::from_vrp_json(&self.name_or("Mirror"), &self.json)
					.map_err(|e| e.to_string())?;
				mirror.headers = self.headers();
				mirror
			}
			FormKind::Http => Mirror {
				id: Mirror::new_id(),
				name: self.name_or("Mirror"),
				kind: MirrorKind::Http {
					base_uri: self.base_uri.trim().to_owned(),
				},
				password: self.password.trim().to_owned(),
				headers: self.headers(),
			},
			FormKind::Rclone => {
				let source = self
					.rclone_file
					.as_ref()
					.ok_or("choose an rclone config file")?;
				let id = Mirror::new_id();
				let mirror = Mirror {
					name: self.name_or(&self.remote),
					kind: MirrorKind::Rclone {
						config_file: rclone_dir.join(format!("{id}.conf")),
						remote: self.remote.clone(),
					},
					id,
					password: self.password.trim().to_owned(),
					headers: self.headers(),
				};
				mirror.validate().map_err(|e| e.to_string())?;
				if let MirrorKind::Rclone { config_file, .. } = &mirror.kind {
					copy_private(source, config_file)
						.map_err(|e| format!("could not copy the rclone config: {e}"))?;
				}
				return Ok(mirror);
			}
		};
		mirror.validate().map_err(|e| e.to_string())?;
		Ok(mirror)
	}

	fn load_rclone_file(&mut self, path: PathBuf) {
		match std::fs::read_to_string(&path) {
			Ok(text) => {
				self.remotes = rclone_remotes(&text);
				self.remote = self.remotes.first().cloned().unwrap_or_default();
				self.error = self
					.remotes
					.is_empty()
					.then(|| "no remotes found in this file".to_owned());
				self.rclone_file = Some(path);
			}
			Err(e) => self.error = Some(format!("cannot read {}: {e}", path.display())),
		}
	}
}

/// The copy holds credentials, so keep it readable by this user only.
fn copy_private(from: &Path, to: &Path) -> std::io::Result<()> {
	if let Some(parent) = to.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let bytes = std::fs::read(from)?;
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	std::io::Write::write_all(&mut options.open(to)?, &bytes)
}

/// Deletes copied rclone configs that no saved mirror uses any more.
pub fn remove_unused_rclone_configs(settings: &Settings, rclone_dir: &Path) {
	let Ok(entries) = std::fs::read_dir(rclone_dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let used = settings.mirrors.iter().any(
			|m| matches!(&m.kind, MirrorKind::Rclone { config_file, .. } if *config_file == path),
		);
		if !used && let Err(e) = std::fs::remove_file(&path) {
			tracing::warn!("could not remove {}: {e}", path.display());
		}
	}
}

fn describe(mirror: &Mirror) -> String {
	let base = match &mirror.kind {
		MirrorKind::Http { base_uri } => {
			let host = reqwest_host(base_uri).unwrap_or_else(|| base_uri.clone());
			format!("HTTP · {host}")
		}
		MirrorKind::Rclone { remote, .. } => format!("rclone · {remote}"),
	};
	match mirror.headers.len() {
		0 => base,
		n => format!("{base} · {n} header(s)"),
	}
}

fn reqwest_host(uri: &str) -> Option<String> {
	let rest = uri.split_once("://")?.1;
	let host = rest.split(['/', '?']).next()?;
	Some(host.rsplit('@').next().unwrap_or(host).to_owned())
}

pub fn show(ui: &mut egui::Ui, draft: &mut Settings, form: &mut MirrorForm, rclone_dir: &Path) {
	ui.heading("Mirrors");
	ui.weak("Servers the library is loaded from. The highlighted one is used.");
	ui.add_space(4.0);

	let mut remove = None;
	if draft.mirrors.is_empty() {
		ui.weak("No mirrors yet.");
	}
	egui::Grid::new("mirrors_grid")
		.num_columns(3)
		.spacing([12.0, 6.0])
		.show(ui, |ui| {
			for mirror in &draft.mirrors {
				let active = draft.active_mirror.as_deref() == Some(mirror.id.as_str());
				if ui.selectable_label(active, &mirror.name).clicked() {
					draft.active_mirror = Some(mirror.id.clone());
				}
				ui.weak(describe(mirror));
				if ui.small_button("Remove").clicked() {
					remove = Some(mirror.id.clone());
				}
				ui.end_row();
			}
		});
	if let Some(id) = remove {
		draft.mirrors.retain(|m| m.id != id);
		draft.normalize();
	}

	ui.add_space(6.0);
	if !form.open {
		if ui.button("Add mirror…").clicked() {
			form.open = true;
		}
		return;
	}

	egui::Frame::group(ui.style()).show(ui, |ui| {
		ui.horizontal(|ui| {
			ui.selectable_value(&mut form.kind, FormKind::VrpJson, "Server config (JSON)");
			ui.selectable_value(&mut form.kind, FormKind::Http, "HTTP");
			ui.selectable_value(&mut form.kind, FormKind::Rclone, "rclone config");
		});
		ui.add_space(4.0);
		egui::Grid::new("mirror_form")
			.num_columns(2)
			.spacing([12.0, 6.0])
			.show(ui, |ui| {
				ui.label("Name");
				ui.add(egui::TextEdit::singleline(&mut form.name).hint_text("Mirror"));
				ui.end_row();

				match form.kind {
					FormKind::VrpJson => {
						ui.label("Config");
						ui.vertical(|ui| {
							ui.add(
								egui::TextEdit::multiline(&mut form.json)
									.hint_text(r#"{"baseUri":"https://…/","password":"…"}"#)
									.desired_rows(3)
									.desired_width(420.0)
									.font(egui::TextStyle::Monospace),
							);
							if ui.button("Load from file…").clicked()
								&& let Some(path) = rfd::FileDialog::new()
									.add_filter("JSON", &["json"])
									.pick_file()
							{
								match std::fs::read_to_string(&path) {
									Ok(text) => form.json = text,
									Err(e) => {
										form.error =
											Some(format!("cannot read {}: {e}", path.display()))
									}
								}
							}
						});
						ui.end_row();
					}
					FormKind::Http => {
						ui.label("Base URI");
						ui.add(
							egui::TextEdit::singleline(&mut form.base_uri)
								.hint_text("https://example.org/files/")
								.desired_width(420.0),
						);
						ui.end_row();
						ui.label("Password");
						ui.add(
							egui::TextEdit::singleline(&mut form.password)
								.password(true)
								.hint_text("base64, as in VRP configs"),
						);
						ui.end_row();
					}
					FormKind::Rclone => {
						ui.label("Config file");
						ui.horizontal(|ui| {
							let label = form
								.rclone_file
								.as_ref()
								.map_or_else(|| "none".to_owned(), |p| p.display().to_string());
							ui.weak(label);
							if ui.button("Choose…").clicked()
								&& let Some(path) = rfd::FileDialog::new()
									.add_filter("rclone config", &["conf"])
									.pick_file()
							{
								form.load_rclone_file(path);
							}
						});
						ui.end_row();
						if !form.remotes.is_empty() {
							ui.label("Remote");
							egui::ComboBox::from_id_salt("rclone_remote")
								.selected_text(&form.remote)
								.show_ui(ui, |ui| {
									for remote in &form.remotes {
										ui.selectable_value(
											&mut form.remote,
											remote.clone(),
											remote,
										);
									}
								});
							ui.end_row();
						}
						ui.label("Password");
						ui.add(
							egui::TextEdit::singleline(&mut form.password)
								.password(true)
								.hint_text("base64 archive password"),
						);
						ui.end_row();
					}
				}

				ui.label("Headers");
				ui.vertical(|ui| {
					let mut drop_header = None;
					for (i, (name, value)) in form.headers.iter_mut().enumerate() {
						ui.horizontal(|ui| {
							ui.add(
								egui::TextEdit::singleline(name)
									.hint_text("X-Api-Key")
									.desired_width(140.0),
							);
							ui.add(
								egui::TextEdit::singleline(value)
									.password(true)
									.desired_width(220.0),
							);
							if ui.small_button("✕").clicked() {
								drop_header = Some(i);
							}
						});
					}
					if let Some(i) = drop_header {
						form.headers.remove(i);
					}
					if ui.small_button("Add header").clicked() {
						form.headers.push((String::new(), String::new()));
					}
				});
				ui.end_row();
			});

		if let Some(error) = &form.error {
			ui.colored_label(ui.visuals().error_fg_color, error);
		}
		ui.horizontal(|ui| {
			if ui.button("Add").clicked() {
				match form.build(rclone_dir) {
					Ok(mirror) => {
						let id = mirror.id.clone();
						draft.mirrors.push(mirror);
						if draft.active_mirror.is_none() {
							draft.active_mirror = Some(id);
						}
						*form = MirrorForm::new();
					}
					Err(e) => form.error = Some(e),
				}
			}
			if ui.button("Cancel").clicked() {
				*form = MirrorForm::new();
			}
		});
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hosts_hide_credentials() {
		assert_eq!(
			reqwest_host("https://user:pw@example.org/a/b").as_deref(),
			Some("example.org")
		);
		assert_eq!(
			reqwest_host("http://10.0.0.2:8080").as_deref(),
			Some("10.0.0.2:8080")
		);
		assert_eq!(reqwest_host("nonsense"), None);
	}

	#[test]
	fn rclone_mirror_copies_config_privately() {
		let dir = tempfile::tempdir().unwrap();
		let source = dir.path().join("rclone.conf");
		std::fs::write(&source, "[vrp]\ntype = ftp\n").unwrap();
		let mut form = MirrorForm::new();
		form.kind = FormKind::Rclone;
		form.load_rclone_file(source);
		form.password = "aGk=".into();
		form.headers = vec![("X-Api-Key".into(), "k".into())];
		let rclone_dir = dir.path().join("cfg/rclone");
		let mirror = form.build(&rclone_dir).unwrap();
		let MirrorKind::Rclone {
			config_file,
			remote,
		} = &mirror.kind
		else {
			panic!()
		};
		assert_eq!(remote, "vrp");
		assert_eq!(mirror.headers.len(), 1);
		assert_eq!(mirror.name, "vrp");
		assert!(config_file.starts_with(&rclone_dir));
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			assert_eq!(
				std::fs::metadata(config_file).unwrap().permissions().mode() & 0o777,
				0o600
			);
		}

		let mut settings = Settings::default();
		remove_unused_rclone_configs(&settings, &rclone_dir);
		assert!(!config_file.exists());
		settings.mirrors.push(mirror);
	}

	#[test]
	fn json_form_keeps_headers() {
		let mut form = MirrorForm::new();
		form.json = r#"{"baseUri":"https://e.org/","password":"aGk="}"#.into();
		form.headers = vec![
			("X-Api-Key".into(), " secret ".into()),
			(String::new(), "ignored".into()),
		];
		let mirror = form.build(Path::new("/unused")).unwrap();
		assert_eq!(
			&mirror.headers,
			&[Header {
				name: "X-Api-Key".into(),
				value: "secret".into()
			}]
		);
	}
}
