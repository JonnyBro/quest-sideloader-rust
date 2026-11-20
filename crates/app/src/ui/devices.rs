use rust_i18n::t;
use std::collections::HashMap;

use quest_sideloader_core::adb::{DeviceEntry, DeviceInfo, DeviceState};
use quest_sideloader_core::{AdbStatus, Command};

use super::format_bytes;

/// Whether there is a headset to work with, and if not, why.
pub enum Readiness {
	NoAdb,
	NoHeadset,
	Waiting(DeviceState),
	Ready,
}

enum InfoState {
	Loading,
	Loaded(DeviceInfo),
	Failed(String),
}

pub struct DevicesView {
	adb_status: AdbStatus,
	devices: Vec<DeviceEntry>,
	info: HashMap<String, InfoState>,
	selected: Option<String>,
	wireless_address: String,
	/// The last headset seen connected over Wi-Fi, kept between runs.
	last_wireless: Option<String>,
	/// Reconnecting to it is tried once, when adb first starts.
	reconnect: bool,
	package_filter: String,
}

impl DevicesView {
	pub fn new() -> Self {
		Self {
			adb_status: AdbStatus::Starting,
			devices: Vec::new(),
			info: HashMap::new(),
			selected: None,
			wireless_address: String::new(),
			last_wireless: None,
			reconnect: false,
			package_filter: String::new(),
		}
	}

	/// Returns the address to reconnect to the first time adb is running.
	pub fn on_adb_status(&mut self, status: AdbStatus) -> Option<String> {
		let running = matches!(status, AdbStatus::Running { .. });
		self.adb_status = status;
		if !(running && std::mem::take(&mut self.reconnect)) {
			return None;
		}
		self.last_wireless.clone()
	}

	pub fn last_wireless(&self) -> Option<&str> {
		self.last_wireless.as_deref()
	}

	pub fn restore_wireless(&mut self, address: String) {
		self.wireless_address = address.clone();
		self.last_wireless = Some(address);
		self.reconnect = true;
	}

	pub fn on_devices_changed(&mut self, devices: Vec<DeviceEntry>) {
		// mDNS serials (`adb-...`) can't be typed into the connect field.
		if let Some(device) = devices.iter().find(|d| {
			d.state == DeviceState::Online
				&& d.serial.contains(':')
				&& !d.serial.starts_with("adb-")
		}) {
			self.last_wireless = Some(device.serial.clone());
			if self.wireless_address.trim().is_empty() {
				self.wireless_address = device.serial.clone();
			}
		}
		self.info.retain(|serial, _| {
			devices
				.iter()
				.any(|d| &d.serial == serial && d.state == DeviceState::Online)
		});
		for device in devices.iter().filter(|d| d.state == DeviceState::Online) {
			// The engine fetches info for every device that comes online.
			self.info
				.entry(device.serial.clone())
				.or_insert(InfoState::Loading);
		}
		let selected_present = self
			.selected
			.as_ref()
			.is_some_and(|s| devices.iter().any(|d| &d.serial == s));
		if !selected_present {
			self.selected = devices
				.iter()
				.find(|d| d.state == DeviceState::Online)
				.or(devices.first())
				.map(|d| d.serial.clone());
		}
		self.devices = devices;
	}

	/// Where dropped files go: the selected headset if it's online, otherwise
	/// the only online one.
	pub fn info(&self, serial: &str) -> Option<&DeviceInfo> {
		match self.info.get(serial) {
			Some(InfoState::Loaded(info)) => Some(info),
			_ => None,
		}
	}

	pub fn install_target(&self) -> Option<&DeviceEntry> {
		let online = |d: &&DeviceEntry| d.state == DeviceState::Online;
		self.selected
			.as_ref()
			.and_then(|s| self.devices.iter().find(|d| &d.serial == s))
			.filter(online)
			.or_else(|| {
				let mut all = self.devices.iter().filter(online);
				match (all.next(), all.next()) {
					(Some(only), None) => Some(only),
					_ => None,
				}
			})
	}

	pub fn installed_apps(&self) -> Option<super::library::InstalledApps<'_>> {
		let device = self.install_target()?;
		let Some(InfoState::Loaded(info)) = self.info.get(&device.serial) else {
			return None;
		};
		Some(super::library::InstalledApps {
			device_name: device.display_name(),
			serial: &device.serial,
			versions: info
				.packages
				.iter()
				.map(|p| (p.name.as_str(), p.version_code))
				.collect(),
			free_bytes: info.storage.as_ref().map(|s| s.free_bytes),
		})
	}

	pub fn on_info(&mut self, serial: String, info: Result<DeviceInfo, String>) {
		if !self.devices.iter().any(|d| d.serial == serial) {
			return;
		}
		let state = match info {
			Ok(info) => InfoState::Loaded(info),
			Err(e) => InfoState::Failed(e),
		};
		self.info.insert(serial, state);
	}

	/// Returns true when the user asks to set up adb.
	pub fn show(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) -> bool {
		let setup = self.toolbar(ui, commands);
		ui.separator();

		if self.devices.is_empty() {
			ui.centered_and_justified(|ui| {
				ui.weak(match &self.adb_status {
					AdbStatus::Unavailable(_) => t!("dev.adb_unavailable_hint"),
					AdbStatus::Missing(_) => t!("dev.adb_missing_hint"),
					_ => t!("dev.none"),
				});
			});
			return setup;
		}

		egui::Panel::left("device_list")
			.resizable(true)
			.default_size(220.0)
			.show(ui, |ui| self.device_list(ui));

		egui::CentralPanel::default().show(ui, |ui| self.details(ui, commands));
		setup
	}

	fn toolbar(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) -> bool {
		let mut setup = false;
		ui.horizontal(|ui| {
			match &self.adb_status {
				AdbStatus::Starting => {
					ui.spinner();
					ui.label(t!("dev.starting"));
				}
				AdbStatus::Running { version } => {
					ui.label(t!("dev.running", version = version));
				}
				AdbStatus::Unavailable(e) => {
					ui.colored_label(
						ui.visuals().error_fg_color,
						t!("dev.unavailable", error = e),
					);
				}
				AdbStatus::Missing(e) => {
					ui.colored_label(
						ui.visuals().error_fg_color,
						t!("dev.unavailable", error = e),
					);
					setup = ui.button(t!("dev.set_up_adb")).clicked();
				}
			}

			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				self.wireless_form(ui, commands);
			});
		});
		setup
	}

	/// The Wi-Fi address field and Connect button, laid out right to left.
	fn wireless_form(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		let address = self.wireless_address.trim().to_owned();
		let connect = ui.add_enabled(!address.is_empty(), egui::Button::new(t!("dev.connect")));
		let field = ui.add(
			egui::TextEdit::singleline(&mut self.wireless_address)
				.hint_text("192.168.1.50:5555")
				.desired_width(160.0),
		);
		let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
		ui.label(t!("dev.wifi"));
		if (connect.clicked() || submitted) && !address.is_empty() {
			commands.push(Command::ConnectWireless(address));
		}
	}

	pub fn adb_status(&self) -> &AdbStatus {
		&self.adb_status
	}

	pub fn readiness(&self) -> Readiness {
		if matches!(
			self.adb_status,
			AdbStatus::Missing(_) | AdbStatus::Unavailable(_)
		) {
			return Readiness::NoAdb;
		}
		if self.install_target().is_some() {
			return Readiness::Ready;
		}
		match self.devices.first() {
			Some(device) => Readiness::Waiting(device.state.clone()),
			None => Readiness::NoHeadset,
		}
	}

	/// The headset in the corner of the sidebar: its state, battery and free
	/// space. Returns (card clicked, "check all" clicked).
	pub fn sidebar_card(&self, ui: &mut egui::Ui) -> (bool, bool) {
		use crate::theme;
		let mut check = false;
		let response = theme::panel(ui)
			.show(ui, |ui| {
				ui.set_width(ui.available_width());
				// The sidebar stacks from the bottom; the card itself reads top down.
				ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
					let (title, color) = match self.readiness() {
						Readiness::Ready => (
							self.install_target()
								.map(|d| d.display_name())
								.unwrap_or_default(),
							theme::GOOD,
						),
						Readiness::Waiting(state) => (state_label(&state), theme::WARN),
						Readiness::NoHeadset => (t!("dev.pill_none").into_owned(), theme::MUTED),
						Readiness::NoAdb => (t!("dev.pill_no_adb").into_owned(), theme::BAD),
					};
					ui.horizontal(|ui| {
						let (dot, _) =
							ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
						ui.painter().circle_filled(dot.center(), 4.0, color);
						ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
					});
					let info = self.install_target().and_then(|d| self.info(&d.serial));
					if let Some(b) = info.and_then(|i| i.battery) {
						ui.weak(
							egui::RichText::new(t!("dev.battery_level", level = b.level)).small(),
						);
						let color = if b.level < 15 {
							theme::BAD
						} else {
							theme::GOOD
						};
						theme::meter(ui, f32::from(b.level) / 100.0, color);
					}
					if let Some(s) = info.and_then(|i| i.storage).filter(|s| s.total_bytes > 0) {
						ui.weak(
							egui::RichText::new(t!(
								"dev.free_short",
								size = format_bytes(s.free_bytes)
							))
							.small(),
						);
						let free = s.free_bytes as f32 / s.total_bytes as f32;
						let color = if free < 0.1 {
							theme::BAD
						} else if free < 0.2 {
							theme::WARN
						} else {
							theme::GOOD
						};
						theme::meter(ui, 1.0 - free, color);
					}
					ui.add_space(2.0);
					let icon = crate::icons::Icon::Check.image(14.0, ui.visuals().text_color());
					check = ui
						.add(egui::Button::image_and_text(icon, t!("chk.button")).small())
						.on_hover_text(t!("chk.hint"))
						.clicked();
				});
			})
			.response
			.interact(egui::Sense::click())
			.on_hover_text(t!("dev.pill_hint"));
		(response.clicked(), check)
	}

	/// Steps to get the headset ready, for when it isn't. Returns true when
	/// the user asks to set up adb.
	pub fn help(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) -> bool {
		let mut setup = false;
		crate::theme::panel(ui).show(ui, |ui| {
			ui.set_width(ui.available_width().min(560.0));
			match self.readiness() {
				Readiness::Ready => {}
				Readiness::NoAdb => {
					ui.heading(t!("help.no_adb_title"));
					ui.label(t!("help.no_adb"));
					ui.add_space(6.0);
					setup = ui
						.add(super::toned(t!("dev.set_up_adb"), super::Tone::Go))
						.clicked();
				}
				Readiness::Waiting(DeviceState::Unauthorized) => {
					ui.heading(t!("help.unauthorized_title"));
					ui.label(t!("help.unauthorized"));
				}
				Readiness::Waiting(state) => {
					ui.heading(t!("help.waiting_title"));
					if let Some(hint) = state_hint(&state) {
						ui.label(hint);
					}
				}
				Readiness::NoHeadset => {
					ui.heading(t!("help.none_title"));
					for (i, step) in [
						t!("help.step_dev_mode"),
						t!("help.step_cable"),
						t!("help.step_allow"),
					]
					.into_iter()
					.enumerate()
					{
						ui.label(format!("{}. {step}", i + 1));
					}
					if cfg!(windows) {
						ui.horizontal_wrapped(|ui| {
							ui.label(t!("help.windows_driver"));
							ui.hyperlink_to(
								t!("adb.driver_link"),
								quest_sideloader_core::platform_tools::QUEST_DRIVER_URL,
							);
						});
					}
					ui.add_space(8.0);
					ui.weak(t!("help.or_wifi"));
					ui.horizontal(|ui| {
						ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
							ui.with_layout(
								egui::Layout::right_to_left(egui::Align::Center),
								|ui| {
									self.wireless_form(ui, commands);
								},
							);
						});
					});
				}
			}
		});
		setup
	}

	fn device_list(&mut self, ui: &mut egui::Ui) {
		egui::ScrollArea::vertical().show(ui, |ui| {
			for device in &self.devices {
				let selected = self.selected.as_deref() == Some(device.serial.as_str());
				let mut title = device.display_name();
				if device.is_wireless() {
					title.push_str(" (Wi-Fi)");
				}
				let response = ui.selectable_label(selected, title);
				ui.horizontal(|ui| {
					ui.add_space(8.0);
					ui.small(state_label(&device.state));
				});
				if response.clicked() {
					self.selected = Some(device.serial.clone());
				}
				ui.add_space(4.0);
			}
		});
	}

	fn details(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		let Some(device) = self
			.selected
			.as_ref()
			.and_then(|s| self.devices.iter().find(|d| &d.serial == s))
		else {
			ui.weak(t!("dev.select"));
			return;
		};

		ui.horizontal(|ui| {
			ui.heading(device.display_name());
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if device.is_wireless() && ui.button(t!("dev.disconnect")).clicked() {
					commands.push(Command::DisconnectWireless(device.serial.clone()));
				}
				let loading = matches!(self.info.get(&device.serial), Some(InfoState::Loading));
				if device.state == DeviceState::Online
					&& ui
						.add_enabled(!loading, egui::Button::new(t!("common.refresh")))
						.clicked()
				{
					self.info.insert(device.serial.clone(), InfoState::Loading);
					commands.push(Command::RefreshDevice(device.serial.clone()));
				}
			});
		});
		ui.weak(&device.serial);
		ui.add_space(8.0);

		if device.state == DeviceState::Online {
			ui.horizontal(|ui| {
				if ui
					.add(super::toned(t!("dev.install_file"), super::Tone::Go))
					.clicked()
					&& let Some(file) = rfd::FileDialog::new()
						.add_filter(t!("dev.apk_or_zip"), &["apk", "zip"])
						.pick_file()
				{
					commands.push(Command::Install {
						serial: device.serial.clone(),
						path: file,
					});
				}
				if ui
					.add(super::toned(t!("dev.install_folder"), super::Tone::Go))
					.clicked()
					&& let Some(folder) = rfd::FileDialog::new().pick_folder()
				{
					commands.push(Command::Install {
						serial: device.serial.clone(),
						path: folder,
					});
				}
				ui.weak(t!("dev.or_drop"));
			});
			ui.add_space(8.0);
		}

		if let Some(hint) = state_hint(&device.state) {
			ui.label(hint);
			return;
		}

		match self.info.get(&device.serial) {
			None | Some(InfoState::Loading) => {
				ui.horizontal(|ui| {
					ui.spinner();
					ui.label(t!("dev.reading"));
				});
			}
			Some(InfoState::Failed(e)) => {
				ui.colored_label(ui.visuals().error_fg_color, e);
			}
			Some(InfoState::Loaded(info)) => {
				info_grid(ui, info);
				ui.add_space(12.0);
				packages(ui, info, &mut self.package_filter);
			}
		}
	}
}

fn state_label(state: &DeviceState) -> String {
	match state {
		DeviceState::Online => t!("dev.state_online").into(),
		DeviceState::Offline => t!("dev.state_offline").into(),
		DeviceState::Unauthorized => t!("dev.state_unauthorized").into(),
		DeviceState::Authorizing => t!("dev.state_authorizing").into(),
		DeviceState::Connecting => t!("dev.state_connecting").into(),
		DeviceState::NoPermissions => t!("dev.state_no_permissions").into(),
		DeviceState::Other(s) => s.clone(),
	}
}

pub fn state_hint(state: &DeviceState) -> Option<std::borrow::Cow<'static, str>> {
	match state {
		DeviceState::Online => None,
		DeviceState::Unauthorized => Some(t!("dev.hint_unauthorized")),
		DeviceState::Authorizing | DeviceState::Connecting => Some(t!("dev.hint_waiting")),
		DeviceState::Offline => Some(t!("dev.hint_offline")),
		DeviceState::NoPermissions => Some(t!("dev.hint_no_permissions")),
		DeviceState::Other(_) => Some(t!("dev.hint_other")),
	}
}

fn info_grid(ui: &mut egui::Ui, info: &DeviceInfo) {
	egui::Grid::new("device_info")
		.num_columns(2)
		.spacing([16.0, 6.0])
		.show(ui, |ui| {
			ui.label(t!("dev.model"));
			ui.label(&info.model);
			ui.end_row();

			ui.label(t!("dev.android"));
			ui.label(&info.android_version);
			ui.end_row();

			ui.label(t!("dev.battery"));
			match info.battery {
				Some(b) => ui.label(format!(
					"{}%{}",
					b.level,
					if b.charging {
						t!("dev.charging")
					} else {
						"".into()
					}
				)),
				None => ui.weak(t!("dev.unknown")),
			};
			ui.end_row();

			ui.label(t!("dev.storage"));
			match info.storage {
				Some(s) => {
					let used = s.total_bytes.saturating_sub(s.free_bytes);
					let fraction = if s.total_bytes == 0 {
						0.0
					} else {
						used as f32 / s.total_bytes as f32
					};
					ui.add(
						egui::ProgressBar::new(fraction)
							.desired_width(260.0)
							.text(t!(
								"dev.free_of",
								free = format_bytes(s.free_bytes),
								total = format_bytes(s.total_bytes)
							)),
					);
				}
				None => {
					ui.weak(t!("dev.unknown"));
				}
			}
			ui.end_row();
		});
}

fn packages(ui: &mut egui::Ui, info: &DeviceInfo, filter: &mut String) {
	ui.horizontal(|ui| {
		ui.strong(t!("dev.installed_apps", n = info.packages.len()));
		ui.add(
			egui::TextEdit::singleline(filter)
				.hint_text(t!("dev.filter"))
				.desired_width(200.0),
		);
	});
	let needle = filter.trim().to_lowercase();
	let rows: Vec<_> = info
		.packages
		.iter()
		.filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
		.collect();

	let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
	egui::ScrollArea::vertical()
		.auto_shrink([false, false])
		.show_rows(ui, row_height, rows.len(), |ui, range| {
			for package in &rows[range] {
				ui.horizontal(|ui| {
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
						if let Some(code) = package.version_code {
							ui.weak(code.to_string());
						}
						ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
							ui.add(egui::Label::new(&package.name).truncate());
						});
					});
				});
			}
		});
}
