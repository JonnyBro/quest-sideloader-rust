use rust_i18n::t;
use std::collections::HashMap;
use std::time::Duration;

use quest_sideloader_core::adb::{AppUsage, DeviceDetails, DeviceInfo, RebootMode, Tweak};
use quest_sideloader_core::saves::Backup;
use quest_sideloader_core::{AppInfo, Command};

use super::format_bytes;

/// The headset this tab works with, as the devices view knows it.
pub struct Target<'a> {
	pub serial: &'a str,
	pub name: String,
	pub info: Option<&'a DeviceInfo>,
	/// Game names from the library, by package.
	pub names: &'a HashMap<String, String>,
	/// Renamed copies made here, listed first when restoring into another app.
	pub copies: &'a std::collections::HashSet<String>,
	/// A package's cover from the library, if it has one.
	pub cover: &'a dyn Fn(&str) -> Option<std::path::PathBuf>,
	/// The version the library has of a package.
	pub latest: &'a dyn Fn(&str) -> Option<u64>,
}

/// Details and per-app storage as the engine reports them.
type Details = Result<(DeviceDetails, HashMap<String, AppUsage>), String>;

enum Dialog {
	Uninstall {
		app: AppInfo,
		back_up: bool,
		remove_obb: bool,
	},
	Reboot(RebootMode),
	DeleteBackup(Backup),
	RestoreInto {
		backup: Backup,
		package: String,
		name: String,
	},
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppSort {
	Name,
	Size,
}

pub struct HeadsetView {
	serial: Option<String>,
	details: Option<Details>,
	loading: bool,
	backups: Vec<Backup>,
	backups_loaded: bool,
	filter: String,
	sort: AppSort,
	dialog: Option<Dialog>,
}

impl HeadsetView {
	pub fn new() -> Self {
		Self {
			serial: None,
			details: None,
			loading: false,
			backups: Vec::new(),
			backups_loaded: false,
			filter: String::new(),
			sort: AppSort::Size,
			dialog: None,
		}
	}

	pub fn on_details(
		&mut self,
		serial: String,
		details: Result<(DeviceDetails, HashMap<String, AppUsage>), String>,
	) {
		if self.serial.as_deref() == Some(serial.as_str()) {
			self.details = Some(details);
			self.loading = false;
		}
	}

	pub fn on_backups(&mut self, backups: Vec<Backup>) {
		self.backups = backups;
		self.backups_loaded = true;
	}

	/// Something on the headset changed (an install, an uninstall...).
	pub fn invalidate(&mut self) {
		self.details = None;
	}

	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		target: Option<Target<'_>>,
		commands: &mut Vec<Command>,
	) {
		if !self.backups_loaded {
			self.backups_loaded = true;
			commands.push(Command::ListBackups);
		}
		let Some(target) = target else {
			self.serial = None;
			ui.centered_and_justified(|ui| ui.weak(t!("hs.connect")));
			return;
		};
		if self.serial.as_deref() != Some(target.serial) {
			self.serial = Some(target.serial.to_owned());
			self.details = None;
		}
		if self.details.is_none() && !self.loading {
			self.loading = true;
			commands.push(Command::RefreshHeadset(target.serial.to_owned()));
		}

		egui::ScrollArea::vertical()
			.auto_shrink([false, false])
			.show(ui, |ui| {
				let section = |ui: &mut egui::Ui, body: &mut dyn FnMut(&mut egui::Ui)| {
					crate::theme::panel(ui).show(ui, |ui| {
						ui.set_width(ui.available_width());
						body(ui);
					});
					ui.add_space(10.0);
				};
				self.apps(ui, &target, commands);
				ui.add_space(12.0);
				egui::CollapsingHeader::new(t!("hs.details_tools"))
					.id_salt("headset_details")
					.show(ui, |ui| {
						section(ui, &mut |ui| self.overview(ui, &target, commands));
						section(ui, &mut |ui| self.tools(ui, &target, commands));
					});
				egui::CollapsingHeader::new(t!("hs.backups", n = self.backups.len()))
					.id_salt("headset_backups_section")
					.show(ui, |ui| {
						section(ui, &mut |ui| self.saves(ui, &target, commands));
					});
			});
		self.dialog(ui.ctx(), target.serial, commands);
	}

	fn overview(&mut self, ui: &mut egui::Ui, target: &Target<'_>, commands: &mut Vec<Command>) {
		ui.horizontal(|ui| {
			ui.heading(&target.name);
			ui.weak(target.serial);
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if ui
					.add_enabled(!self.loading, egui::Button::new(t!("common.refresh")))
					.clicked()
				{
					self.loading = true;
					commands.push(Command::RefreshHeadset(target.serial.to_owned()));
					commands.push(Command::RefreshDevice(target.serial.to_owned()));
				}
				if self.loading {
					ui.spinner();
				}
			});
		});
		let details = match &self.details {
			Some(Ok((details, _))) => Some(details),
			Some(Err(e)) => {
				ui.colored_label(ui.visuals().error_fg_color, e);
				None
			}
			None => None,
		};
		egui::Grid::new("headset_overview")
			.num_columns(2)
			.spacing([16.0, 4.0])
			.show(ui, |ui| {
				let mut row = |label: std::borrow::Cow<'_, str>, value: String| {
					ui.label(label);
					ui.label(value);
					ui.end_row();
				};
				if let Some(info) = target.info {
					row(t!("dev.model"), info.model.clone());
					row(t!("dev.android"), info.android_version.clone());
				}
				if let Some(d) = details {
					if !d.codename.is_empty() {
						row(t!("hs.codename"), d.codename.clone());
					}
					if !d.build.is_empty() {
						row(t!("hs.build"), d.build.clone());
					}
					if let (Some(total), Some(free)) = (d.memory_total, d.memory_available) {
						row(
							t!("hs.memory"),
							t!(
								"dev.free_of",
								free = format_bytes(free),
								total = format_bytes(total)
							)
							.into_owned(),
						);
					}
					if let Some(uptime) = d.uptime {
						row(t!("hs.uptime"), format_uptime(uptime));
					}
					row(
						t!("hs.wifi"),
						d.wifi_ip
							.clone()
							.unwrap_or_else(|| t!("hs.not_connected").into_owned()),
					);
					if let Some(mtp) = d.mtp {
						row(
							t!("hs.usb"),
							if mtp {
								t!("hs.usb_mtp")
							} else {
								t!("hs.usb_charging")
							}
							.into_owned(),
						);
					}
					row(
						t!("hs.wireless_adb"),
						d.adb_tcp_port.map_or_else(
							|| t!("hs.off").into_owned(),
							|port| t!("hs.on_port", port = port).into_owned(),
						),
					);
				}
			});
	}

	fn tools(&mut self, ui: &mut egui::Ui, target: &Target<'_>, commands: &mut Vec<Command>) {
		ui.strong(t!("hs.tools"));
		ui.horizontal_wrapped(|ui| {
			let has_wifi = matches!(&self.details, Some(Ok((d, _))) if d.wifi_ip.is_some());
			if ui
				.add_enabled(has_wifi, egui::Button::new(t!("hs.enable_wireless")))
				.on_hover_text(t!("hs.enable_wireless_hint"))
				.on_disabled_hover_text(t!("hs.no_wifi"))
				.clicked()
			{
				commands.push(Command::EnableWireless(target.serial.to_owned()));
			}
			let mtp = match &self.details {
				Some(Ok((d, _))) => d.mtp,
				_ => None,
			};
			let label = if mtp == Some(true) {
				t!("hs.mtp_off")
			} else {
				t!("hs.mtp_on")
			};
			if ui
				.add_enabled(mtp.is_some(), egui::Button::new(label))
				.on_hover_text(t!("hs.mtp_hint"))
				.clicked()
			{
				let enabled = mtp != Some(true);
				commands.push(Command::SetMtp {
					serial: target.serial.to_owned(),
					enabled,
				});
				// The headset only reports the new mode after the USB reset.
				if let Some(Ok((d, _))) = &mut self.details {
					d.mtp = Some(enabled);
				}
			}
			if ui
				.button(t!("hs.screenshot"))
				.on_hover_text(t!("hs.screenshot_hint"))
				.clicked()
			{
				commands.push(Command::Screenshot(target.serial.to_owned()));
			}
			ui.menu_button(t!("hs.restart_menu"), |ui| {
				for (mode, label) in [
					(RebootMode::Normal, t!("hs.restart")),
					(RebootMode::PowerOff, t!("hs.power_off")),
					(RebootMode::Recovery, t!("hs.restart_recovery")),
					(RebootMode::Bootloader, t!("hs.restart_bootloader")),
				] {
					if ui.button(label).clicked() {
						self.dialog = Some(Dialog::Reboot(mode));
						ui.close();
					}
				}
			});
		});

		ui.add_space(6.0);
		ui.horizontal_wrapped(|ui| {
			ui.strong(t!("hs.performance"));
			let current = match &self.details {
				Some(Ok((d, _))) => [
					d.refresh_rate.clone(),
					d.cpu_level.clone(),
					d.gpu_level.clone(),
				],
				_ => [None, None, None],
			};
			for (tweak, label, current) in [
				(Tweak::RefreshRate, t!("hs.refresh_rate"), &current[0]),
				(Tweak::CpuLevel, t!("hs.cpu_level"), &current[1]),
				(Tweak::GpuLevel, t!("hs.gpu_level"), &current[2]),
			] {
				let default = t!("hs.default");
				let shown = current.as_deref().unwrap_or(&default);
				ui.add_space(8.0);
				ui.label(label);
				egui::ComboBox::from_id_salt(tweak.property())
					.selected_text(shown)
					.show_ui(ui, |ui| {
						for choice in tweak.choices() {
							let text = if choice.is_empty() { &default } else { *choice };
							if ui
								.selectable_label(current.as_deref() == Some(*choice), text)
								.clicked()
							{
								commands.push(Command::SetTweak {
									serial: target.serial.to_owned(),
									tweak,
									value: (*choice).to_owned(),
								});
								self.details = None;
							}
						}
					});
			}
			ui.weak(t!("hs.until_restart"));
		});
	}

	fn apps(&mut self, ui: &mut egui::Ui, target: &Target<'_>, commands: &mut Vec<Command>) {
		let Some(info) = target.info else { return };
		let usage = match &self.details {
			Some(Ok((_, usage))) => Some(usage),
			_ => None,
		};
		ui.horizontal(|ui| {
			ui.heading(t!("hs.apps_title"));
			ui.weak(info.packages.len().to_string());
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if ui
					.add(super::toned(t!("dev.install_file"), super::Tone::Go))
					.clicked()
					&& let Some(file) = rfd::FileDialog::new()
						.add_filter(t!("dev.apk_or_zip"), &["apk", "zip"])
						.pick_file()
				{
					commands.push(Command::Install {
						serial: target.serial.to_owned(),
						path: file,
					});
				}
				if ui.button(t!("dev.install_folder")).clicked()
					&& let Some(folder) = rfd::FileDialog::new().pick_folder()
				{
					commands.push(Command::Install {
						serial: target.serial.to_owned(),
						path: folder,
					});
				}
				let refresh = crate::icons::Icon::Refresh.image(16.0, ui.visuals().text_color());
				if ui
					.add_enabled(!self.loading, egui::Button::image(refresh))
					.on_hover_text(t!("common.refresh"))
					.clicked()
				{
					self.loading = true;
					commands.push(Command::RefreshHeadset(target.serial.to_owned()));
					commands.push(Command::RefreshDevice(target.serial.to_owned()));
				}
				if self.loading {
					ui.spinner();
				}
			});
		});
		ui.add_space(4.0);
		ui.horizontal(|ui| {
			ui.add(
				egui::TextEdit::singleline(&mut self.filter)
					.hint_text(t!("dev.filter"))
					.desired_width(220.0),
			);
			ui.selectable_value(&mut self.sort, AppSort::Size, t!("hs.largest"));
			ui.selectable_value(&mut self.sort, AppSort::Name, t!("hs.by_name"));
		});

		let needle = self.filter.trim().to_lowercase();
		let mut rows: Vec<(AppInfo, AppUsage)> = info
			.packages
			.iter()
			.map(|p| {
				let name = target
					.names
					.get(&p.name)
					.cloned()
					.unwrap_or_else(|| p.name.clone());
				let usage = usage
					.and_then(|u| u.get(&p.name))
					.copied()
					.unwrap_or_default();
				(
					AppInfo {
						package: p.name.clone(),
						name,
						version_code: p.version_code,
					},
					usage,
				)
			})
			.filter(|(app, _)| {
				needle.is_empty()
					|| app.name.to_lowercase().contains(&needle)
					|| app.package.to_lowercase().contains(&needle)
			})
			.collect();
		match self.sort {
			AppSort::Name => rows.sort_by_cached_key(|(app, _)| app.name.to_lowercase()),
			AppSort::Size => rows.sort_by_key(|(_, u)| std::cmp::Reverse(u.obb + u.data)),
		}

		let mut uninstall = None;
		let backups = &self.backups;
		egui_extras::TableBuilder::new(ui)
			.id_salt("headset_apps")
			.striped(true)
			// The tab scrolls as a whole; a second scroll area inside would fight it.
			.vscroll(false)
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(egui_extras::Column::exact(52.0))
			.column(egui_extras::Column::remainder().at_least(200.0).clip(true))
			.column(egui_extras::Column::auto().at_least(90.0))
			.column(egui_extras::Column::auto())
			.body(|mut body| {
				for (app, usage) in rows {
					body.row(44.0, |mut row| {
						row.col(|ui| {
							let size = egui::vec2(48.0, 27.0);
							match (target.cover)(&app.package) {
								Some(path) => {
									ui.add(
										egui::Image::new(format!("file://{}", path.display()))
											.max_size(size)
											.corner_radius(crate::theme::radius(3.0)),
									);
								}
								None => {
									let (rect, _) =
										ui.allocate_exact_size(size, egui::Sense::hover());
									ui.painter().rect_filled(
										rect,
										crate::theme::radius(3.0),
										ui.visuals().faint_bg_color,
									);
								}
							}
						});
						row.col(|ui| {
							ui.vertical(|ui| {
								ui.add(egui::Label::new(&app.name).truncate());
								let mut line = Vec::new();
								if let Some(v) = app.version_code {
									line.push(format!("v{v}"));
								}
								let total = usage.obb + usage.data;
								if total > 0 {
									line.push(format_bytes(total));
								}
								if target.copies.contains(&app.package) {
									line.push(t!("hs.copy").into_owned());
								}
								if app.name != app.package {
									line.push(app.package.clone());
								}
								ui.add(
									egui::Label::new(
										egui::RichText::new(line.join(", ")).small().weak(),
									)
									.truncate(),
								);
							});
						});
						row.col(|ui| {
							if let Some(latest) = (target.latest)(&app.package) {
								let (text, color) = if app.version_code.is_some_and(|v| latest > v)
								{
									(t!("lib.status_update"), crate::theme::WARN)
								} else {
									(t!("hs.up_to_date"), crate::theme::GOOD)
								};
								crate::theme::pill(ui, text, color);
							}
						});
						row.col(|ui| {
							let dots =
								crate::icons::Icon::Menu.image(16.0, ui.visuals().text_color());
							ui.menu_image_button(dots, |ui| {
								if ui.button(t!("hs.start")).clicked() {
									commands.push(Command::LaunchApp {
										serial: target.serial.to_owned(),
										package: app.package.clone(),
									});
								}
								if ui.button(t!("hs.stop")).clicked() {
									commands.push(Command::StopApp {
										serial: target.serial.to_owned(),
										package: app.package.clone(),
									});
								}
								ui.separator();
								if ui
									.button(t!("hs.back_up"))
									.on_hover_text(t!("hs.back_up_hint"))
									.clicked()
								{
									commands.push(Command::BackUpSaves {
										serial: target.serial.to_owned(),
										app: app.clone(),
									});
								}
								let latest_backup = backups
									.iter()
									.filter(|b| b.meta.package == app.package)
									.max_by_key(|b| b.meta.created);
								if let Some(backup) = latest_backup
									&& ui
										.button(t!(
											"hs.restore_latest",
											date = super::format_unix_time(backup.meta.created)
										))
										.on_hover_text(t!("hs.restore_hint"))
										.clicked()
								{
									commands.push(Command::RestoreBackup {
										serial: target.serial.to_owned(),
										backup: backup.clone(),
										package: app.package.clone(),
									});
								}
								ui.separator();
								let remove = egui::RichText::new(t!("hs.uninstall_button"))
									.color(ui.visuals().error_fg_color);
								if ui.button(remove).clicked() {
									uninstall = Some(app.clone());
								}
							})
							.response
							.on_hover_text(t!("hs.actions"));
						});
					});
				}
			});
		if let Some(app) = uninstall {
			self.dialog = Some(Dialog::Uninstall {
				app,
				back_up: true,
				remove_obb: true,
			});
		}
	}

	fn saves(&mut self, ui: &mut egui::Ui, target: &Target<'_>, commands: &mut Vec<Command>) {
		ui.horizontal(|ui| {
			ui.strong(t!("hs.backups", n = self.backups.len()));
			if let Some(dir) = self
				.backups
				.first()
				.and_then(|b| b.dir.parent()?.parent().map(|p| p.to_path_buf()))
				&& ui.small_button(t!("common.open_folder")).clicked()
			{
				super::open_folder(&dir);
			}
		});
		if self.backups.is_empty() {
			ui.weak(t!("hs.no_backups"));
			return;
		}
		let mut delete = None;
		let mut restore_into = None;
		// Copies first, then the rest by name.
		let mut others: Vec<(bool, String, String)> = target
			.info
			.map(|info| {
				info.packages
					.iter()
					.map(|p| {
						let name = target
							.names
							.get(&p.name)
							.cloned()
							.unwrap_or_else(|| p.name.clone());
						(!target.copies.contains(&p.name), name, p.name.clone())
					})
					.collect()
			})
			.unwrap_or_default();
		others.sort_by_key(|(not_copy, name, _)| (*not_copy, name.to_lowercase()));
		egui_extras::TableBuilder::new(ui)
			.id_salt("headset_backups")
			.striped(true)
			.vscroll(false)
			.cell_layout(egui::Layout::left_to_right(egui::Align::Center))
			.column(egui_extras::Column::remainder().at_least(200.0).clip(true))
			.column(egui_extras::Column::auto())
			.column(egui_extras::Column::auto())
			.column(egui_extras::Column::auto())
			.body(|mut body| {
				for backup in &self.backups {
					body.row(40.0, |mut row| {
						row.col(|ui| {
							ui.vertical(|ui| {
								ui.add(egui::Label::new(&backup.meta.name).truncate());
								ui.add(
									egui::Label::new(
										egui::RichText::new(&backup.meta.package).small(),
									)
									.truncate(),
								);
							});
						});
						row.col(|ui| {
							ui.label(super::format_unix_time(backup.meta.created));
						});
						row.col(|ui| {
							let private = if backup.meta.private_files > 0 {
								t!("hs.incl_private")
							} else {
								"".into()
							};
							ui.label(t!(
								"hs.backup_info",
								size = format_bytes(backup.meta.bytes),
								n = backup.meta.files,
								private = private
							));
						});
						row.col(|ui| {
							if ui
								.add(super::toned(t!("hs.restore"), super::Tone::Go).small())
								.on_hover_text(t!("hs.restore_hint"))
								.clicked()
							{
								commands.push(Command::RestoreBackup {
									serial: target.serial.to_owned(),
									backup: backup.clone(),
									package: backup.meta.package.clone(),
								});
							}
							ui.add_enabled_ui(!others.is_empty(), |ui| {
								ui.menu_button(t!("hs.restore_into"), |ui| {
									egui::ScrollArea::vertical()
										.max_height(320.0)
										.show(ui, |ui| {
											for (not_copy, name, package) in &others {
												if package == &backup.meta.package {
													continue;
												}
												let label = if *not_copy {
													format!("{name} ({package})")
												} else {
													t!(
														"hs.copy_entry",
														name = name,
														package = package
													)
													.into_owned()
												};
												if ui.button(label).clicked() {
													restore_into = Some((
														backup.clone(),
														package.clone(),
														name.clone(),
													));
													ui.close();
												}
											}
										});
								})
								.response
								.on_hover_text(t!("hs.restore_into_hint"));
							});
							if ui.small_button(t!("common.open")).clicked() {
								super::open_folder(&backup.dir);
							}
							if ui
								.add(
									super::toned(t!("hs.delete_button"), super::Tone::Danger)
										.small(),
								)
								.clicked()
							{
								delete = Some(backup.clone());
							}
						});
					});
				}
			});
		if let Some(backup) = delete {
			self.dialog = Some(Dialog::DeleteBackup(backup));
		}
		if let Some((backup, package, name)) = restore_into {
			self.dialog = Some(Dialog::RestoreInto {
				backup,
				package,
				name,
			});
		}
	}

	fn dialog(&mut self, ctx: &egui::Context, serial: &str, commands: &mut Vec<Command>) {
		let Some(dialog) = &mut self.dialog else {
			return;
		};
		let mut close = false;
		let modal = egui::Modal::new(egui::Id::unique("headset_dialog")).show(ctx, |ui| {
			ui.set_max_width(440.0);
			match dialog {
				Dialog::Uninstall {
					app,
					back_up,
					remove_obb,
				} => {
					ui.heading(t!("hs.uninstall_title", name = app.name));
					ui.checkbox(back_up, t!("hs.backup_first"));
					ui.checkbox(remove_obb, t!("hs.delete_obb"));
					ui.horizontal(|ui| {
						if ui
							.add(super::toned(t!("hs.uninstall"), super::Tone::Danger))
							.clicked()
						{
							commands.push(Command::Uninstall {
								serial: serial.to_owned(),
								app: app.clone(),
								back_up: *back_up,
								remove_obb: *remove_obb,
							});
							self.details = None;
							close = true;
						}
						close |= ui.button(t!("common.cancel")).clicked();
					});
				}
				Dialog::Reboot(mode) => {
					ui.heading(match mode {
						RebootMode::Normal => t!("hs.confirm_restart"),
						RebootMode::PowerOff => t!("hs.confirm_power_off"),
						RebootMode::Recovery => t!("hs.confirm_recovery"),
						RebootMode::Bootloader => t!("hs.confirm_bootloader"),
					});
					ui.horizontal(|ui| {
						if ui
							.add(super::toned(t!("common.yes"), super::Tone::Danger))
							.clicked()
						{
							commands.push(Command::Reboot {
								serial: serial.to_owned(),
								mode: *mode,
							});
							close = true;
						}
						close |= ui.button(t!("common.cancel")).clicked();
					});
				}
				Dialog::RestoreInto {
					backup,
					package,
					name,
				} => {
					ui.heading(t!("hs.restore_into_title"));
					ui.label(t!(
						"hs.restore_into_text",
						backup = backup.meta.name,
						date = super::format_unix_time(backup.meta.created),
						name = name,
						package = package
					));
					ui.horizontal(|ui| {
						if ui
							.add(super::toned(t!("hs.restore"), super::Tone::Go))
							.clicked()
						{
							commands.push(Command::RestoreBackup {
								serial: serial.to_owned(),
								backup: backup.clone(),
								package: package.clone(),
							});
							close = true;
						}
						close |= ui.button(t!("common.cancel")).clicked();
					});
				}
				Dialog::DeleteBackup(backup) => {
					ui.heading(t!("hs.delete_backup_title"));
					ui.label(t!(
						"hs.delete_backup_text",
						name = backup.meta.name,
						date = super::format_unix_time(backup.meta.created)
					));
					ui.horizontal(|ui| {
						if ui
							.add(super::toned(t!("common.delete"), super::Tone::Danger))
							.clicked()
						{
							commands.push(Command::DeleteBackup(backup.clone()));
							close = true;
						}
						close |= ui.button(t!("common.cancel")).clicked();
					});
				}
			}
		});
		if close || modal.should_close() {
			self.dialog = None;
		}
	}
}

fn format_uptime(uptime: Duration) -> String {
	let mins = uptime.as_secs() / 60;
	match (mins / 1440, (mins / 60) % 24, mins % 60) {
		(0, 0, m) => t!("hs.uptime_m", m = m).into_owned(),
		(0, h, m) => t!("hs.uptime_hm", h = h, m = m).into_owned(),
		(d, h, _) => t!("hs.uptime_dh", d = d, h = h).into_owned(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn uptime_text() {
		assert_eq!(format_uptime(Duration::from_secs(5 * 60)), "5 min");
		assert_eq!(
			format_uptime(Duration::from_secs(3 * 3600 + 7 * 60)),
			"3 h 7 min"
		);
		assert_eq!(
			format_uptime(Duration::from_secs(2 * 86_400 + 5 * 3600)),
			"2 d 5 h"
		);
	}
}
