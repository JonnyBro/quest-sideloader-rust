use rust_i18n::t;
use std::path::PathBuf;

use quest_sideloader_core::rename::KeyStore;

struct Row {
	package: String,
	/// The certificate's SHA-256, or why the key file can't be read.
	fingerprint: Result<String, String>,
}

pub struct KeysView {
	store: KeyStore,
	rows: Option<Vec<Row>>,
	message: Option<Result<String, String>>,
	changed: bool,
}

impl KeysView {
	pub fn new(dir: PathBuf) -> Self {
		Self {
			store: KeyStore::new(dir),
			rows: None,
			message: None,
			changed: false,
		}
	}

	/// Packages with a key: apps the user made with the Rename tab.
	pub fn packages(&self) -> Vec<String> {
		self.store.packages()
	}

	/// Read the keys again next time, after a copy was built.
	pub fn invalidate(&mut self) {
		self.rows = None;
	}

	/// True once after a key was imported.
	pub fn take_changed(&mut self) -> bool {
		std::mem::take(&mut self.changed)
	}

	fn load(&self) -> Vec<Row> {
		self.store
			.packages()
			.into_iter()
			.map(|package| {
				let fingerprint = match self.store.load(&package) {
					Ok(Some(key)) => Ok(key.fingerprint()),
					Ok(None) => Err(String::new()),
					Err(e) => Err(e.to_string()),
				};
				Row {
					package,
					fingerprint,
				}
			})
			.collect()
	}

	pub fn show(&mut self, ui: &mut egui::Ui) {
		ui.heading(t!("keys.heading"));
		ui.weak(t!("keys.intro"));
		ui.add_space(4.0);
		if self.rows.is_none() {
			self.rows = Some(self.load());
		}
		let mut export = None;
		let rows = self.rows.as_deref().unwrap_or_default();
		if rows.is_empty() {
			ui.weak(t!("keys.none"));
		}
		egui::Grid::new("signing_keys")
			.num_columns(3)
			.spacing([16.0, 6.0])
			.show(ui, |ui| {
				for row in rows {
					ui.monospace(&row.package);
					match &row.fingerprint {
						Ok(fingerprint) => {
							ui.weak(format!(
								"SHA-256 {}...",
								&fingerprint[..16.min(fingerprint.len())]
							))
							.on_hover_text(fingerprint);
						}
						Err(e) => {
							ui.colored_label(ui.visuals().error_fg_color, e);
						}
					}
					if ui.small_button(t!("keys.export")).clicked() {
						export = Some(row.package.clone());
					}
					ui.end_row();
				}
			});
		ui.add_space(4.0);
		if ui.button(t!("keys.import")).clicked()
			&& let Some(file) = rfd::FileDialog::new()
				.add_filter("PEM", &["pem"])
				.pick_file()
		{
			self.message = Some(match self.store.import(&file) {
				Ok(package) => {
					self.changed = true;
					self.rows = None;
					Ok(t!("keys.imported", package = package).into_owned())
				}
				Err(e) => Err(e.to_string()),
			});
		}
		if let Some(package) = export
			&& let Some(dest) = rfd::FileDialog::new()
				.set_file_name(format!("{package}.pem"))
				.add_filter("PEM", &["pem"])
				.save_file()
		{
			self.message = Some(match self.store.export(&package, &dest) {
				Ok(()) => Ok(t!("keys.exported", path = dest.display()).into_owned()),
				Err(e) => Err(e.to_string()),
			});
		}
		match &self.message {
			Some(Ok(text)) => {
				ui.label(text);
			}
			Some(Err(text)) => {
				ui.colored_label(ui.visuals().error_fg_color, text);
			}
			None => {}
		}
	}
}
