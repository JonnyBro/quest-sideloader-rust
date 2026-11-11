use std::collections::HashMap;

use quest_sideloader_core::adb::{DeviceEntry, DeviceInfo, DeviceState};
use quest_sideloader_core::{AdbStatus, Command};

use super::format_bytes;

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
			package_filter: String::new(),
		}
	}

	pub fn on_adb_status(&mut self, status: AdbStatus) {
		self.adb_status = status;
	}

	pub fn on_devices_changed(&mut self, devices: Vec<DeviceEntry>) {
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

	pub fn show(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		self.toolbar(ui, commands);
		ui.separator();

		if self.devices.is_empty() {
			ui.centered_and_justified(|ui| {
				ui.weak(match &self.adb_status {
					AdbStatus::Unavailable(_) => {
						"ADB is not available. Check the adb path in Settings."
					}
					_ => {
						"No devices. Connect the headset with a USB cable and enable developer mode."
					}
				});
			});
			return;
		}

		egui::Panel::left("device_list")
			.resizable(true)
			.default_size(220.0)
			.show(ui, |ui| self.device_list(ui));

		egui::CentralPanel::default().show(ui, |ui| self.details(ui, commands));
	}

	fn toolbar(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		ui.horizontal(|ui| {
			match &self.adb_status {
				AdbStatus::Starting => {
					ui.spinner();
					ui.label("Starting ADB…");
				}
				AdbStatus::Running { version } => {
					ui.label(format!("ADB server running (protocol {version})"));
				}
				AdbStatus::Unavailable(e) => {
					ui.colored_label(ui.visuals().error_fg_color, format!("ADB unavailable: {e}"));
				}
			}

			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				let address = self.wireless_address.trim().to_owned();
				let connect = ui.add_enabled(!address.is_empty(), egui::Button::new("Connect"));
				let field = ui.add(
					egui::TextEdit::singleline(&mut self.wireless_address)
						.hint_text("192.168.1.50:5555")
						.desired_width(160.0),
				);
				let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
				ui.label("Wi-Fi:");
				if (connect.clicked() || submitted) && !address.is_empty() {
					commands.push(Command::ConnectWireless(address));
				}
			});
		});
	}

	fn device_list(&mut self, ui: &mut egui::Ui) {
		egui::ScrollArea::vertical().show(ui, |ui| {
			for device in &self.devices {
				let selected = self.selected.as_deref() == Some(device.serial.as_str());
				let mut title = device.display_name();
				if device.is_wireless() {
					title.push_str(" · Wi-Fi");
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
			ui.weak("Select a device");
			return;
		};

		ui.horizontal(|ui| {
			ui.heading(device.display_name());
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if device.is_wireless() && ui.button("Disconnect").clicked() {
					commands.push(Command::DisconnectWireless(device.serial.clone()));
				}
				let loading = matches!(self.info.get(&device.serial), Some(InfoState::Loading));
				if device.state == DeviceState::Online
					&& ui
						.add_enabled(!loading, egui::Button::new("Refresh"))
						.clicked()
				{
					self.info.insert(device.serial.clone(), InfoState::Loading);
					commands.push(Command::RefreshDevice(device.serial.clone()));
				}
			});
		});
		ui.weak(&device.serial);
		ui.add_space(8.0);

		if let Some(hint) = state_hint(&device.state) {
			ui.label(hint);
			return;
		}

		match self.info.get(&device.serial) {
			None | Some(InfoState::Loading) => {
				ui.horizontal(|ui| {
					ui.spinner();
					ui.label("Reading device info…");
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
		DeviceState::Online => "connected".into(),
		DeviceState::Offline => "offline".into(),
		DeviceState::Unauthorized => "unauthorized".into(),
		DeviceState::Authorizing => "authorizing".into(),
		DeviceState::Connecting => "connecting".into(),
		DeviceState::NoPermissions => "no USB access".into(),
		DeviceState::Other(s) => s.clone(),
	}
}

fn state_hint(state: &DeviceState) -> Option<&'static str> {
	match state {
		DeviceState::Online => None,
		DeviceState::Unauthorized => {
			Some("Put on the headset and allow USB debugging for this computer.")
		}
		DeviceState::Authorizing | DeviceState::Connecting => Some("Waiting for the device…"),
		DeviceState::Offline => {
			Some("The device is offline. Reconnect the cable or restart the headset.")
		}
		DeviceState::NoPermissions => Some(
			"This user has no access to the USB device. Install udev rules for Android devices \
			 (for example the android-udev package) and replug the headset.",
		),
		DeviceState::Other(_) => Some("The device is in an unsupported state."),
	}
}

fn info_grid(ui: &mut egui::Ui, info: &DeviceInfo) {
	egui::Grid::new("device_info")
		.num_columns(2)
		.spacing([16.0, 6.0])
		.show(ui, |ui| {
			ui.label("Model");
			ui.label(&info.model);
			ui.end_row();

			ui.label("Android");
			ui.label(&info.android_version);
			ui.end_row();

			ui.label("Battery");
			match info.battery {
				Some(b) => ui.label(format!(
					"{}%{}",
					b.level,
					if b.charging { ", charging" } else { "" }
				)),
				None => ui.weak("unknown"),
			};
			ui.end_row();

			ui.label("Storage");
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
							.text(format!(
								"{} free of {}",
								format_bytes(s.free_bytes),
								format_bytes(s.total_bytes)
							)),
					);
				}
				None => {
					ui.weak("unknown");
				}
			}
			ui.end_row();
		});
}

fn packages(ui: &mut egui::Ui, info: &DeviceInfo, filter: &mut String) {
	ui.horizontal(|ui| {
		ui.strong(format!("Installed apps ({})", info.packages.len()));
		ui.add(
			egui::TextEdit::singleline(filter)
				.hint_text("Filter")
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
					ui.label(&package.name);
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
						if let Some(code) = package.version_code {
							ui.weak(code.to_string());
						}
					});
				});
			}
		});
}
