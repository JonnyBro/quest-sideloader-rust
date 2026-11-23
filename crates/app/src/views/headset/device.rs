use super::*;

impl HeadsetView {
	pub(super) fn find_leftovers(&mut self, cx: &mut Context<Self>) {
		if let Some(serial) = self.serial.clone() {
			self.searching_leftovers = true;
			self.engine.send(Command::FindLeftovers(serial));
			cx.notify();
		}
	}

	pub(super) fn leftovers_card(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let muted = |text: String| div().text_color(theme.muted_foreground).child(text);
		let search = Button::new("find-leftovers")
			.label(if self.leftovers.is_some() {
				t!("hs.leftovers_again")
			} else {
				t!("hs.leftovers_find")
			})
			.outline()
			.small()
			.loading(self.searching_leftovers)
			.on_click(cx.listener(|this, _, _, cx| this.find_leftovers(cx)));
		let serial = self.serial.clone().unwrap_or_default();
		let body = match &self.leftovers {
			None => muted(t!("hs.leftovers_hint").into_owned()).into_any_element(),
			Some(Err(e)) => div()
				.text_color(theme.danger)
				.child(e.clone())
				.into_any_element(),
			Some(Ok(list)) if list.is_empty() => {
				muted(t!("hs.leftovers_none").into_owned()).into_any_element()
			}
			Some(Ok(list)) => {
				let total: u64 = list.iter().map(Leftover::total).sum();
				let rows = list.iter().enumerate().map(|(ix, leftover)| {
					let mut parts = Vec::new();
					if let Some(obb) = leftover.obb_bytes {
						parts.push(format!("OBB {}", format_bytes(obb)));
					}
					if let Some(data) = leftover.data_bytes {
						parts.push(t!("hs.leftovers_data", size = format_bytes(data)).into_owned());
					}
					let name = self.names.get(&leftover.package).cloned();
					let engine = self.engine.clone();
					let (package, serial) = (leftover.package.clone(), serial.clone());
					let label = name.clone().unwrap_or_else(|| package.clone());
					let size = format_bytes(leftover.total());
					h_flex()
						.gap_3()
						.py_1()
						.child(
							v_flex()
								.flex_1()
								.min_w_0()
								.child(div().truncate().child(label))
								.when(name.is_some(), |d| {
									d.child(
										div()
											.text_sm()
											.text_color(theme.muted_foreground)
											.mono(cx)
											.truncate()
											.child(package.clone()),
									)
								}),
						)
						.child(
							div()
								.flex_shrink_0()
								.text_sm()
								.mono(cx)
								.child(parts.join(", ")),
						)
						.child(
							Button::new(("remove-leftover", ix))
								.icon(Icon::new(IconName::Delete))
								.ghost()
								.xsmall()
								.tooltip(t!("common.delete"))
								.on_click(move |_, window, cx| {
									confirm(
										&engine,
										Confirm {
											title: t!("hs.leftovers_remove_title").into_owned(),
											text: t!(
												"hs.leftovers_remove_one",
												name = package,
												size = size
											)
											.into_owned(),
											ok: t!("common.delete").into_owned(),
											danger: true,
										},
										Command::RemoveLeftovers {
											serial: serial.clone(),
											packages: vec![package.clone()],
										},
										window,
										cx,
									);
								}),
						)
				});
				let engine = self.engine.clone();
				let packages: Vec<String> = list.iter().map(|l| l.package.clone()).collect();
				let count = packages.len();
				let serial = serial.clone();
				v_flex()
					.gap_1()
					.children(rows)
					.child(
						h_flex().pt_2().child(
							Button::new("remove-all-leftovers")
								.label(t!(
									"hs.leftovers_remove_all",
									n = count,
									size = format_bytes(total)
								))
								.danger()
								.small()
								.on_click(move |_, window, cx| {
									confirm(
										&engine,
										Confirm {
											title: t!("hs.leftovers_remove_title").into_owned(),
											text: t!(
												"hs.leftovers_remove_text",
												n = count,
												size = format_bytes(total)
											)
											.into_owned(),
											ok: t!("common.delete").into_owned(),
											danger: true,
										},
										Command::RemoveLeftovers {
											serial: serial.clone(),
											packages: packages.clone(),
										},
										window,
										cx,
									);
								}),
						),
					)
					.into_any_element()
			}
		};
		card(
			t!("hs.leftovers").into(),
			v_flex().gap_3().child(body).child(h_flex().child(search)),
			&theme,
		)
		.into_any_element()
	}

	pub(super) fn device_section(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let details = self.device_details();
		let info = self.info.as_ref();

		// (label, value, monospace)
		type Facts = Vec<(String, String, bool)>;
		let mut system: Facts = Vec::new();
		let mut power: Facts = Vec::new();
		let mut network: Facts = Vec::new();
		if let Some(i) = info {
			system.push((t!("dev.model").into(), i.model.replace('_', " "), false));
		}
		if let Some(d) = details
			&& !d.codename.is_empty()
		{
			system.push((t!("hs.codename").into(), d.codename.clone(), true));
		}
		if let Some(i) = info {
			system.push((t!("dev.android").into(), i.android_version.clone(), true));
		}
		if let Some(d) = details {
			if let Some(v) = &d.horizon_version {
				system.push((t!("hs.horizon").into(), v.clone(), true));
			}
			if let Some(n) = &d.build_number {
				system.push((t!("hs.build_number").into(), n.clone(), true));
			}
			if !d.build.is_empty() {
				system.push((t!("hs.build").into(), d.build.clone(), true));
			}
			if let Some(p) = &d.security_patch {
				system.push((t!("hs.security_patch").into(), p.clone(), true));
			}
			if let Some(size) = &d.display {
				system.push((t!("hs.display").into(), size.clone(), true));
			}
			if let (Some(total), Some(free)) = (d.memory_total, d.memory_available) {
				system.push((
					t!("hs.memory").into(),
					t!(
						"dev.free_of",
						free = format_bytes(free),
						total = format_bytes(total)
					)
					.into_owned(),
					false,
				));
			}
		}
		if let Some(s) = info.and_then(|i| i.storage) {
			system.push((
				t!("hs.storage").into(),
				t!(
					"dev.free_of",
					free = format_bytes(s.free_bytes),
					total = format_bytes(s.total_bytes)
				)
				.into_owned(),
				false,
			));
		}
		if let Some(uptime) = details.and_then(|d| d.uptime) {
			system.push((t!("hs.uptime").into(), format_uptime(uptime), false));
		}

		if let Some(b) = info.and_then(|i| i.battery) {
			let mut text = format!("{}%", b.level);
			if b.charging {
				text.push_str(&format!(", {}", t!("hs.charging")));
			}
			power.push((t!("hs.battery").into(), text, false));
		}
		if let Some(d) = details {
			if let Some(p) = d.power {
				let mut text = match p.source {
					None => t!("hs.source_battery"),
					Some(PowerSource::Ac) => t!("hs.source_ac"),
					Some(PowerSource::Usb) => t!("hs.source_usb"),
					Some(PowerSource::Wireless) => t!("hs.source_wireless"),
					Some(PowerSource::Dock) => t!("hs.source_dock"),
				}
				.into_owned();
				if p.weak_charger {
					text.push_str(&format!(", {}", t!("hs.weak_charger")));
				}
				power.push((t!("hs.power_source").into(), text, false));
				if let Some(health) = p.health {
					let text = match health {
						BatteryHealth::Good => t!("hs.health_good"),
						BatteryHealth::Overheat => t!("hs.health_overheat"),
						BatteryHealth::Dead => t!("hs.health_dead"),
						BatteryHealth::OverVoltage => t!("hs.health_overvoltage"),
						BatteryHealth::Cold => t!("hs.health_cold"),
						BatteryHealth::Other => t!("hs.health_other"),
					};
					power.push((t!("hs.battery_health").into(), text.into_owned(), false));
				}
				if let Some(mv) = p.voltage_mv {
					power.push((
						t!("hs.voltage").into(),
						format!("{:.2} V", f64::from(mv) / 1000.0),
						true,
					));
				}
			}
			if let Some(temp) = d.battery_temperature {
				power.push((t!("hs.battery_temp").into(), format!("{temp:.0}C"), true));
			}
			if let Some(temp) = d.soc_temperature {
				power.push((t!("hs.soc_temp").into(), format!("{temp:.0}C"), true));
			}

			match &d.wifi {
				Some(w) => {
					network.push((t!("hs.wifi").into(), w.ssid.clone(), false));
					network.push((
						t!("hs.signal").into(),
						format!("{} dBm, {}", w.rssi, signal_quality(w.rssi)),
						false,
					));
					let mut link = Vec::new();
					if let Some(mbps) = w.link_mbps {
						link.push(t!("hs.mbps", speed = mbps).into_owned());
					}
					if let Some(band) = w.frequency_mhz.and_then(wifi_band) {
						link.push(band.to_owned());
					}
					if let Some(standard) = w.standard {
						link.push(format!("Wi-Fi {standard}"));
					}
					if !link.is_empty() {
						network.push((t!("hs.link").into(), link.join(", "), false));
					}
				}
				None => network.push((
					t!("hs.wifi").into(),
					t!("hs.not_connected").into_owned(),
					false,
				)),
			}
			if let Some(ip) = &d.wifi_ip {
				network.push((t!("hs.ip").into(), ip.clone(), true));
			}
			if let Some(mtp) = d.mtp {
				network.push((
					t!("hs.usb").into(),
					if mtp {
						t!("hs.usb_mtp")
					} else {
						t!("hs.usb_charging")
					}
					.into_owned(),
					false,
				));
			}
			network.push((
				t!("hs.wireless_adb").into(),
				d.adb_tcp_port.map_or_else(
					|| t!("hs.off").into_owned(),
					|port| t!("hs.on_port", port = port).into_owned(),
				),
				false,
			));
		}

		let fact_rows = |facts: Facts| {
			v_flex().children(facts.into_iter().map(|(label, value, mono)| {
				h_flex()
					.gap_4()
					.py_1()
					.child(
						div()
							.w(px(170.))
							.flex_shrink_0()
							.text_color(theme.muted_foreground)
							.child(label),
					)
					.child(div().min_w_0().when(mono, |d| d.mono(cx)).child(value))
			}))
		};
		let controllers = details.map(|d| d.controllers.clone()).unwrap_or_default();
		let controller_rows = if controllers.is_empty() {
			div()
				.text_color(theme.muted_foreground)
				.child(t!("hs.no_controllers").into_owned())
				.into_any_element()
		} else {
			let facts: Facts = controllers
				.iter()
				.map(|c| {
					let label = match c.side {
						ControllerSide::Left => t!("hs.left_controller"),
						ControllerSide::Right => t!("hs.right_controller"),
					};
					let mut value = c
						.battery
						.map_or_else(|| "?".to_owned(), |b| format!("{b}%"));
					if let Some(fw) = &c.firmware {
						value.push_str(&format!(", {}", t!("hs.firmware", version = fw)));
					}
					(label.into_owned(), value, false)
				})
				.collect();
			v_flex()
				.gap_2()
				.child(fact_rows(facts))
				.child(
					div()
						.text_sm()
						.text_color(theme.muted_foreground)
						.child(t!("hs.controllers_hint").into_owned()),
				)
				.into_any_element()
		};
		let info_cards = h_flex()
			.gap_4()
			.flex_wrap()
			.items_start()
			// Both columns are paddingless wrappers: flex grows from a zero basis plus
			// padding, so a padded card here would come out wider than the other column.
			.child(v_flex().flex_1().min_w(px(380.)).child(card(
				t!("hs.sec_system").into(),
				fact_rows(system),
				&theme,
			)))
			.child(
				v_flex()
					.gap_4()
					.flex_1()
					.min_w(px(380.))
					.when(!power.is_empty(), |d| {
						d.child(card(t!("hs.sec_power").into(), fact_rows(power), &theme))
					})
					.child(card(
						t!("hs.sec_controllers").into(),
						controller_rows,
						&theme,
					))
					.when(!network.is_empty(), |d| {
						d.child(card(
							t!("hs.sec_network").into(),
							fact_rows(network),
							&theme,
						))
					}),
			);

		let has_wifi = details.is_some_and(|d| d.wifi_ip.is_some());
		let mtp = details.and_then(|d| d.mtp);
		let tools = h_flex()
			.gap_2()
			.flex_wrap()
			.child(
				Button::new("wireless")
					.label(t!("hs.enable_wireless"))
					.outline()
					.small()
					.disabled(!has_wifi)
					.tooltip(if has_wifi {
						t!("hs.enable_wireless_hint")
					} else {
						t!("hs.no_wifi")
					})
					.on_click(cx.listener(|this, _, _, _| this.send(Command::EnableWireless))),
			)
			.child(
				Button::new("mtp")
					.label(if mtp == Some(true) {
						t!("hs.mtp_off")
					} else {
						t!("hs.mtp_on")
					})
					.outline()
					.small()
					.disabled(mtp.is_none())
					.tooltip(t!("hs.mtp_hint"))
					.on_click(cx.listener(move |this, _, _, cx| {
						let enabled = mtp != Some(true);
						this.send(|serial| Command::SetMtp { serial, enabled });
						// The headset only reports the new mode after the USB reset.
						if let Some(Ok((d, _))) = &mut this.details {
							d.mtp = Some(enabled);
						}
						cx.notify();
					})),
			)
			.child(
				Button::new("screenshot")
					.label(t!("hs.screenshot"))
					.outline()
					.small()
					.tooltip(t!("hs.screenshot_hint"))
					.on_click(cx.listener(|this, _, _, _| this.send(Command::Screenshot))),
			)
			.children(
				quest_sideloader_core::scrcpy::path()
					.is_some()
					.then(|| self.mirror_menu()),
			)
			.child(self.app_safety_menu())
			.child(self.reboot_menu());

		let current = |tweak: Tweak| -> Option<String> {
			let d = details?;
			match tweak {
				Tweak::RefreshRate => d.refresh_rate.clone(),
				Tweak::CpuLevel => d.cpu_level.clone(),
				Tweak::GpuLevel => d.gpu_level.clone(),
			}
		};
		let tweaks = h_flex().gap_4().flex_wrap().children(
			[
				(Tweak::RefreshRate, t!("hs.refresh_rate")),
				(Tweak::CpuLevel, t!("hs.cpu_level")),
				(Tweak::GpuLevel, t!("hs.gpu_level")),
			]
			.into_iter()
			.map(|(tweak, label)| {
				h_flex()
					.gap_2()
					.child(
						div()
							.text_color(theme.muted_foreground)
							.child(label.into_owned()),
					)
					.child(self.tweak_menu(tweak, current(tweak)))
			}),
		);

		v_flex()
			.size_full()
			.child(
				div().px_5().pt_5().pb_4().child(
					h_flex()
						.gap_4()
						.flex_wrap()
						.items_stretch()
						.child(
							card(t!("hs.tools").into(), tools, &theme)
								.flex_1()
								.min_w(px(380.)),
						)
						.child(
							card(t!("hs.performance").into(), tweaks, &theme)
								.flex_1()
								.min_w(px(380.)),
						),
				),
			)
			.child(
				div()
					.id("device-section")
					.flex_1()
					.min_h_0()
					.overflow_y_scroll()
					.child(
						v_flex()
							.gap_4()
							.px_5()
							.pb_5()
							.child(info_cards)
							.child(self.leftovers_card(cx)),
					),
			)
			.into_any_element()
	}

	pub(super) fn mirror_menu(&self) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		Button::new("mirror")
			.label(t!("hs.mirror"))
			.outline()
			.small()
			.dropdown_caret(true)
			.tooltip(t!("hs.mirror_hint"))
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for (eye, label) in [
					(Eye::Both, t!("hs.mirror_both")),
					(Eye::Left, t!("hs.mirror_left")),
					(Eye::Right, t!("hs.mirror_right")),
				] {
					menu = menu.item(send_item(
						&engine,
						label.into_owned(),
						Command::MirrorScreen {
							serial: serial.clone(),
							eye,
						},
					));
				}
				menu
			})
	}

	pub(super) fn app_safety_menu(&self) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		Button::new("app-safety")
			.label(t!("hs.app_safety"))
			.outline()
			.small()
			.dropdown_caret(true)
			.tooltip(t!("hs.app_safety_hint"))
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for (enabled, label, title, text) in [
					(
						false,
						t!("hs.app_safety_off"),
						t!("hs.app_safety_off_title"),
						t!("hs.app_safety_off_text"),
					),
					(
						true,
						t!("hs.app_safety_on"),
						t!("hs.app_safety_on_title"),
						t!("hs.app_safety_on_text"),
					),
				] {
					let engine = engine.clone();
					let serial = serial.clone();
					let (label, title, text) =
						(label.into_owned(), title.into_owned(), text.into_owned());
					menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
						move |_, window, cx| {
							confirm(
								&engine,
								Confirm {
									title: title.clone(),
									text: text.clone(),
									ok: label.clone(),
									danger: !enabled,
								},
								Command::SetAppSafety {
									serial: serial.clone(),
									enabled,
								},
								window,
								cx,
							);
						},
					));
				}
				menu
			})
	}

	pub(super) fn reboot_menu(&self) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		Button::new("reboot")
			.label(t!("hs.restart_menu"))
			.outline()
			.small()
			.dropdown_caret(true)
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for (mode, label) in [
					(RebootMode::Normal, t!("hs.restart")),
					(RebootMode::PowerOff, t!("hs.power_off")),
					(RebootMode::Recovery, t!("hs.restart_recovery")),
					(RebootMode::Bootloader, t!("hs.restart_bootloader")),
				] {
					let engine = engine.clone();
					let serial = serial.clone();
					let label = label.into_owned();
					menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
						move |_, window, cx| {
							confirm(
								&engine,
								Confirm {
									title: t!("hs.reboot_title", action = label).into_owned(),
									text: t!("hs.reboot_text").into_owned(),
									ok: label.clone(),
									danger: mode != RebootMode::Normal,
								},
								Command::Reboot {
									serial: serial.clone(),
									mode,
								},
								window,
								cx,
							);
						},
					));
				}
				menu
			})
	}

	pub(super) fn tweak_menu(&self, tweak: Tweak, current: Option<String>) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let default = t!("hs.default").into_owned();
		let shown = current
			.clone()
			.filter(|c| !c.is_empty())
			.unwrap_or_else(|| default.clone());
		Button::new(tweak.property())
			.label(shown)
			.outline()
			.small()
			.dropdown_caret(true)
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				for choice in tweak.choices() {
					let text = if choice.is_empty() {
						default.clone()
					} else {
						(*choice).to_owned()
					};
					let checked = current.as_deref().unwrap_or("") == *choice;
					menu = menu.item(
						send_item(
							&engine,
							text,
							Command::SetTweak {
								serial: serial.clone(),
								tweak,
								value: (*choice).to_owned(),
							},
						)
						.checked(checked),
					);
				}
				menu
			})
	}
}

pub(super) fn signal_quality(rssi: i32) -> std::borrow::Cow<'static, str> {
	match rssi {
		-55.. => t!("hs.signal_excellent"),
		-67.. => t!("hs.signal_good"),
		-75.. => t!("hs.signal_fair"),
		_ => t!("hs.signal_weak"),
	}
}

pub(super) fn wifi_band(mhz: u32) -> Option<&'static str> {
	match mhz {
		2400..=2500 => Some("2.4 GHz"),
		5000..=5900 => Some("5 GHz"),
		5925..=7125 => Some("6 GHz"),
		_ => None,
	}
}

pub(super) fn format_uptime(uptime: Duration) -> String {
	let mins = uptime.as_secs() / 60;
	match (mins / 1440, (mins / 60) % 24, mins % 60) {
		(0, 0, m) => t!("hs.uptime_m", m = m).into_owned(),
		(0, h, m) => t!("hs.uptime_hm", h = h, m = m).into_owned(),
		(d, h, _) => t!("hs.uptime_dh", d = d, h = h).into_owned(),
	}
}
