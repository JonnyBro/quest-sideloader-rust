use rust_i18n::t;

use quest_sideloader_core::Command;
use quest_sideloader_core::{platform_tools, rclone_tool};

/// Offers to download adb when the engine can't find it.
#[derive(Default)]
pub struct AdbSetup {
	open: bool,
	/// Answered once this session; reopened only from the Devices tab.
	answered: bool,
}

impl AdbSetup {
	pub fn on_missing(&mut self) {
		if !self.answered {
			self.open = true;
		}
	}

	pub fn reopen(&mut self) {
		self.open = true;
	}

	pub fn show(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
		if !self.open {
			return;
		}
		let mut close = false;
		let modal = egui::Modal::new(egui::Id::unique("adb_setup")).show(ctx, |ui| {
			ui.set_max_width(480.0);
			ui.heading(t!("adb.title"));
			ui.label(t!("adb.text"));
			if cfg!(windows) {
				ui.add_space(4.0);
				ui.label(t!("adb.windows_driver"));
				ui.hyperlink_to(t!("adb.driver_link"), platform_tools::QUEST_DRIVER_URL);
			}
			ui.add_space(6.0);
			let can_install = platform_tools::download_url().is_some();
			if can_install {
				ui.horizontal_wrapped(|ui| {
					ui.spacing_mut().item_spacing.x = 0.0;
					ui.weak(t!("adb.terms"));
					ui.hyperlink_to(t!("adb.terms_link"), platform_tools::TERMS_URL);
				});
			} else {
				ui.weak(t!("adb.no_build"));
			}
			ui.add_space(6.0);
			ui.horizontal(|ui| {
				if can_install && ui.button(t!("adb.install")).clicked() {
					commands.push(Command::InstallAdb);
					close = true;
				}
				if ui.button(t!("adb.open_page")).clicked() {
					ui.ctx()
						.open_url(egui::OpenUrl::new_tab(platform_tools::INFO_URL));
					close = true;
				}
				close |= ui.button(t!("adb.not_now")).clicked();
			});
		});
		if close || modal.should_close() {
			self.open = false;
			self.answered = true;
		}
	}
}

/// Shown when a sync fails because rclone is missing.
#[derive(Default)]
pub struct RcloneSetup {
	open: bool,
}

impl RcloneSetup {
	pub fn on_missing(&mut self) {
		self.open = true;
	}

	pub fn show(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
		if !self.open {
			return;
		}
		let mut close = false;
		let modal = egui::Modal::new(egui::Id::unique("rclone_setup")).show(ctx, |ui| {
			ui.set_max_width(480.0);
			ui.heading(t!("rclone.title"));
			ui.label(t!("rclone.text"));
			let can_install = rclone_tool::platform().is_some();
			if !can_install {
				ui.add_space(4.0);
				ui.weak(t!("rclone.no_build"));
			}
			ui.add_space(6.0);
			ui.horizontal(|ui| {
				if can_install && ui.button(t!("adb.install")).clicked() {
					commands.push(Command::InstallRclone);
					close = true;
				}
				if ui.button(t!("adb.open_page")).clicked() {
					ui.ctx()
						.open_url(egui::OpenUrl::new_tab(rclone_tool::INFO_URL));
					close = true;
				}
				close |= ui.button(t!("adb.not_now")).clicked();
			});
		});
		if close || modal.should_close() {
			self.open = false;
		}
	}
}
