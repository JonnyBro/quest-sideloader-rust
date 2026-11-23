use super::*;

impl Shell {
	pub(super) fn open_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		self.logs_open = true;
		self.logs_seen = 0;
		self.poll_logs(window, cx);
		cx.notify();
	}

	pub(super) fn set_log_level(
		&mut self,
		level: LogLevel,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		self.log_level = level;
		self.logs_seen = 0;
		self.poll_logs(window, cx);
		cx.notify();
	}

	pub(super) fn poll_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let generation = self.logs.generation();
		if !self.logs_open {
			// The badge on the logs button counts new errors.
			if self.logs.errors() != self.errors_seen {
				cx.notify();
			}
			return;
		}
		self.errors_seen = self.logs.errors();
		if generation == self.logs_seen {
			return;
		}
		let shown = self.log_lines;
		let min = self.log_level;
		// Lines without a level continue the line above; they stay.
		let lines: Vec<String> = self
			.logs
			.lines()
			.into_iter()
			.filter(|line| level_of(line).is_none_or(|level| level >= min))
			.collect();
		let updated = self.log_view.update(cx, |editor, cx| {
			// Replacing the text drops the selection; wait until the user is done with it.
			if !editor.selected_range().is_empty() {
				return false;
			}
			let at_bottom = editor
				.visible_row_range()
				.is_none_or(|rows| rows.end + 1 >= shown);
			editor.set_value(lines.join("\n"), window, cx);
			if at_bottom {
				// Clamped to the last line on the next layout.
				editor.set_scroll_offset(point(px(0.), px(-1e9)), cx);
			}
			true
		});
		if updated {
			self.logs_seen = generation;
			self.log_lines = lines.len();
		}
	}

	pub(super) fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let muted = theme.muted_foreground;
		let sep = || div().w_px().h_4().bg(theme.border);
		let item = |text: String| div().child(text);

		let mut left = h_flex().gap_3().min_w_0().overflow_hidden();
		match self.selected_device() {
			Some(device) => {
				left = left
					.child(
						h_flex()
							.gap_2()
							.child(div().size_2().rounded_full().bg(theme.success))
							.child(item(device.display_name())),
					)
					.child(sep())
					.child(item(transport_label(device)).text_color(muted).mono(cx));
				if let Some(info) = self.infos.get(&device.serial) {
					if let Some(b) = info.battery {
						let charging = if b.charging { "+" } else { "" };
						let mut text = format!("{}{charging}%", b.level);
						if let Some(temp) = self
							.details
							.get(&device.serial)
							.and_then(|d| d.battery_temperature)
						{
							text.push_str(&format!("  {temp:.0}C"));
						}
						left = left.child(sep()).child(
							h_flex()
								.gap_1()
								.child(Icon::new(battery_icon(b.level, b.charging)).small())
								.child(item(text)),
						);
					}
					let pads = self
						.details
						.get(&device.serial)
						.map(|d| controllers_text(&d.controllers))
						.unwrap_or_default();
					if !pads.is_empty() {
						left = left.child(sep()).child(
							h_flex()
								.id("status-controllers")
								.gap_1()
								.child(Icon::from(AppIcon::Controller).small())
								.child(item(pads))
								.tooltip(|window, cx| {
									Tooltip::new(t!("hs.controllers_hint").into_owned())
										.build(window, cx)
								}),
						);
					}
					if let Some(s) = info.storage {
						left = left.child(sep()).child(
							h_flex()
								.gap_1()
								.child(Icon::new(IconName::HardDrive).small())
								.child(item(
									t!(
										"dev.free_of",
										free = format_bytes(s.free_bytes),
										total = format_bytes(s.total_bytes)
									)
									.into_owned(),
								)),
						);
					}
				}
			}
			None => {
				left = left.child(item(t!("dev.none").into_owned()).text_color(muted));
			}
		}
		if let Some(adb) = &self.adb {
			let text = match adb {
				AdbStatus::Running { version } => format!("adb {version}"),
				AdbStatus::Starting => t!("adb.starting").into_owned(),
				AdbStatus::Unavailable(_) | AdbStatus::Missing(_) => t!("adb.down").into_owned(),
			};
			let down = matches!(adb, AdbStatus::Unavailable(_) | AdbStatus::Missing(_));
			left = left.child(sep()).child(
				div()
					.id("adb-status")
					.text_color(if down { theme.danger } else { muted })
					.mono(cx)
					.when(down, |d| {
						d.cursor_pointer()
							.on_click(cx.listener(|this, _, window, cx| {
								tools::open_adb(this.engine.clone(), window, cx)
							}))
					})
					.child(text),
			);
		}

		let active: Vec<&TaskSnapshot> = self.active_tasks().collect();
		let task = active.first().map(|task| {
			let mut text = task.title.clone();
			if let Some(p) = task.progress {
				text.push_str(&format!("  {:.0}%", p * 100.));
			}
			if active.len() > 1 {
				text.push_str(&format!("  +{}", active.len() - 1));
			}
			h_flex()
				.id("status-task")
				.gap_2()
				.max_w(px(360.))
				.cursor_pointer()
				.child(Icon::from(AppIcon::Downloads).small())
				.child(div().truncate().child(text))
				.on_click(
					cx.listener(|this, _, window, cx| this.set_tab(Tab::Downloads, window, cx)),
				)
		});

		h_flex()
			.h(px(30.))
			.px_3()
			.gap_3()
			.text_sm()
			.bg(theme.status_bar)
			.border_t_1()
			.border_color(theme.status_bar_border)
			.child(left.flex_1())
			.children(task)
			.children(self.app_update.as_ref().map(|release| {
				let url = release.url.clone();
				Button::new("app-update")
					.label(t!("upd.available", version = release.version))
					.ghost()
					.xsmall()
					.text_color(theme.success)
					.tooltip(t!("upd.hint"))
					.on_click(move |_, _, cx| cx.open_url(&url))
			}))
			.child(
				Button::new("toggle-logs")
					.icon(Icon::from(AppIcon::Logs))
					.label(match self.logs.errors().saturating_sub(self.errors_seen) {
						0 => t!("tab.logs").into_owned(),
						n => t!("logs.button_errors", n = n).into_owned(),
					})
					.ghost()
					.xsmall()
					.when(self.logs.errors() > self.errors_seen, |b| b.danger())
					.tooltip(t!("logs.button_hint"))
					.selected(self.logs_open)
					.on_click(cx.listener(|this, _, window, cx| {
						this.logs_open = !this.logs_open;
						this.logs_seen = 0;
						this.poll_logs(window, cx);
						cx.notify();
					})),
			)
			.child(
				div()
					.text_xs()
					.text_color(theme.muted_foreground)
					.mono(cx)
					.child(concat!("v", env!("CARGO_PKG_VERSION"))),
			)
	}

	pub(super) fn logs_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		self.log_colors.set(LevelColors {
			error: theme.danger,
			warn: theme.warning,
			debug: theme.muted_foreground,
		});
		let level = self.log_level;
		let shell = cx.entity().downgrade();
		let folder = self.paths.logs_dir();
		let toolbar = h_flex()
			.px_3()
			.py_1()
			.gap_2()
			.text_sm()
			.border_b_1()
			.border_color(theme.border)
			.child(
				Button::new("log-level")
					.label(level_label(level))
					.ghost()
					.xsmall()
					.dropdown_caret(true)
					.dropdown_menu(move |mut menu, _, _| {
						for choice in [
							LogLevel::Trace,
							LogLevel::Debug,
							LogLevel::Info,
							LogLevel::Warn,
							LogLevel::Error,
						] {
							let shell = shell.clone();
							menu = menu.item(
								PopupMenuItem::new(level_label(choice))
									.checked(choice == level)
									.on_click(move |_, window, cx| {
										let _ = shell.update(cx, |this, cx| {
											this.set_log_level(choice, window, cx)
										});
									}),
							);
						}
						menu
					}),
			)
			.child(
				div()
					.text_color(theme.muted_foreground)
					.child(t!("logs.count", n = self.log_lines).into_owned()),
			)
			.child(div().flex_1())
			.child(
				Button::new("logs-folder")
					.label(t!("common.open_folder"))
					.icon(Icon::new(IconName::FolderOpen))
					.ghost()
					.xsmall()
					.on_click(move |_, _, _| crate::widgets::open_folder(&folder)),
			)
			.child(
				Button::new("logs-close")
					.icon(Icon::new(IconName::Close))
					.ghost()
					.xsmall()
					.on_click(cx.listener(|this, _, _, cx| {
						this.logs_open = false;
						cx.notify();
					})),
			);
		v_flex()
			.h(px(LOGS_HEIGHT))
			.flex_shrink_0()
			.border_t_1()
			.border_color(theme.border)
			.bg(theme.sidebar)
			.child(toolbar)
			.child(
				div().flex_1().min_h_0().child(
					Editor::new(&self.log_view)
						.readonly(true)
						.bordered(false)
						.appearance(false)
						.h_full(),
				),
			)
	}
}

pub(super) fn level_label(level: LogLevel) -> String {
	match level {
		LogLevel::Trace => t!("logs.everything"),
		LogLevel::Debug => t!("logs.debug"),
		LogLevel::Info => t!("logs.info"),
		LogLevel::Warn => t!("logs.warnings"),
		LogLevel::Error => t!("logs.errors"),
	}
	.into_owned()
}

/// `L 60%  R 70%`, or empty when no controller is paired.
pub(super) fn controllers_text(controllers: &[Controller]) -> String {
	controllers
		.iter()
		.filter_map(|c| {
			let side = match c.side {
				ControllerSide::Left => t!("hs.left_short"),
				ControllerSide::Right => t!("hs.right_short"),
			};
			Some(format!("{side} {}%", c.battery?))
		})
		.collect::<Vec<_>>()
		.join("  ")
}

pub(super) fn battery_icon(level: u8, charging: bool) -> IconName {
	match level {
		_ if charging => IconName::BatteryCharging,
		0..=15 => IconName::BatteryWarning,
		16..=40 => IconName::BatteryLow,
		41..=80 => IconName::BatteryMedium,
		_ => IconName::BatteryFull,
	}
}
