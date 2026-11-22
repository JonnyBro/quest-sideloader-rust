use rust_i18n::t;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, Theme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::{DeviceEntry, DeviceInfo, DeviceState};
use quest_sideloader_core::{AdbStatus, Command, Engine, format_bytes, platform_tools};

use crate::icons::AppIcon;
use crate::theme::Mono;

pub enum DevicesEvent {
	/// Work with this headset: the Headset tab, installs, the console.
	Select(String),
	/// Open the dialog that installs adb.
	SetUpAdb,
}

/// Every device adb sees, with its state and what to do about it.
pub struct DevicesView {
	engine: Rc<Engine>,
	adb: Option<AdbStatus>,
	devices: Vec<DeviceEntry>,
	infos: HashMap<String, DeviceInfo>,
	selected: Option<String>,
	address: Entity<InputState>,
	_address_events: Subscription,
}

impl EventEmitter<DevicesEvent> for DevicesView {}

impl DevicesView {
	pub fn new(
		engine: Rc<Engine>,
		last_wireless: Option<String>,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let address = cx.new(|cx| {
			InputState::new(window, cx)
				.placeholder("192.168.1.50:5555")
				.default_value(last_wireless.unwrap_or_default())
		});
		let events = cx.subscribe_in(&address, window, |this, _, event, _, cx| match event {
			InputEvent::PressEnter { .. } => this.connect(cx),
			InputEvent::Change => cx.notify(),
			_ => {}
		});
		Self {
			engine,
			adb: None,
			devices: Vec::new(),
			infos: HashMap::new(),
			selected: None,
			address,
			_address_events: events,
		}
	}

	pub fn update_state(
		&mut self,
		adb: Option<AdbStatus>,
		devices: Vec<DeviceEntry>,
		infos: HashMap<String, DeviceInfo>,
		selected: Option<String>,
		cx: &mut Context<Self>,
	) {
		self.adb = adb;
		self.devices = devices;
		self.infos = infos;
		self.selected = selected;
		cx.notify();
	}

	fn connect(&mut self, cx: &mut Context<Self>) {
		let address = self.address.read(cx).value().trim().to_owned();
		if !address.is_empty() {
			self.engine.send(Command::ConnectWireless(address));
		}
	}

	fn adb_down(&self) -> bool {
		matches!(
			self.adb,
			Some(AdbStatus::Missing(_) | AdbStatus::Unavailable(_))
		)
	}

	fn wifi_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let empty = self.address.read(cx).value().trim().is_empty();
		h_flex()
			.gap_2()
			.child(
				div()
					.w(px(220.))
					.mono(cx)
					.child(Input::new(&self.address).small()),
			)
			.child(
				Button::new("connect-wifi")
					.label(t!("dev.connect"))
					.small()
					.outline()
					.disabled(empty)
					.on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
			)
	}

	/// What to do when there is no headset to work with yet. The Headset tab
	/// shows this too.
	pub fn help(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let title = |text: String| div().text_xl().child(text);
		let text = |text: String| div().text_color(theme.muted_foreground).child(text);
		let body = if self.adb_down() {
			v_flex()
				.gap_3()
				.child(title(t!("help.no_adb_title").into()))
				.child(text(t!("help.no_adb").into()))
				.child(
					h_flex().child(
						Button::new("set-up-adb")
							.label(t!("dev.set_up_adb"))
							.primary()
							.on_click(cx.listener(|_, _, _, cx| cx.emit(DevicesEvent::SetUpAdb))),
					),
				)
		} else if let Some(device) = self.devices.first() {
			let unauthorized = device.state == DeviceState::Unauthorized;
			v_flex()
				.gap_3()
				.child(title(if unauthorized {
					t!("help.unauthorized_title").into()
				} else {
					t!("help.waiting_title").into()
				}))
				.child(text(if unauthorized {
					t!("help.unauthorized").into()
				} else {
					state_hint(&device.state).unwrap_or_default()
				}))
		} else {
			let steps = [
				t!("help.step_dev_mode"),
				t!("help.step_cable"),
				t!("help.step_allow"),
			];
			v_flex()
				.gap_3()
				.child(title(t!("help.none_title").into()))
				.child(
					v_flex()
						.gap_2()
						.children(steps.into_iter().enumerate().map(|(i, step)| {
							h_flex()
								.gap_3()
								.items_start()
								.child(
									div()
										.size_6()
										.flex_shrink_0()
										.rounded_full()
										.bg(theme.primary)
										.text_color(theme.primary_foreground)
										.flex()
										.items_center()
										.justify_center()
										.text_sm()
										.child((i + 1).to_string()),
								)
								.child(div().flex_1().min_w_0().pt_0p5().child(step.into_owned()))
						})),
				)
				.when(cfg!(windows), |d| {
					d.child(
						h_flex()
							.gap_1()
							.flex_wrap()
							.child(text(t!("help.windows_driver").into()))
							.child(
								Button::new("driver-link")
									.label(t!("adb.driver_link"))
									.link()
									.on_click(|_, _, cx| {
										cx.open_url(platform_tools::QUEST_DRIVER_URL)
									}),
							),
					)
				})
				.child(
					v_flex()
						.gap_2()
						.pt_2()
						.child(text(t!("help.or_wifi").into()))
						.child(self.wifi_form(cx)),
				)
		};
		div()
			.size_full()
			.flex()
			.items_center()
			.justify_center()
			.p_8()
			.child(
				div()
					.w_full()
					.max_w(px(620.))
					.p_6()
					.rounded(theme.radius_lg)
					.border_1()
					.border_color(theme.border)
					.bg(theme.muted)
					.child(body),
			)
			.into_any_element()
	}

	fn device_card(&self, ix: usize, device: &DeviceEntry, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let online = device.state == DeviceState::Online;
		let selected = self.selected.as_deref() == Some(device.serial.as_str());
		let dot = match device.state {
			DeviceState::Online => theme.success,
			DeviceState::Unauthorized | DeviceState::Authorizing | DeviceState::Connecting => {
				theme.warning
			}
			_ => theme.danger,
		};
		let mut name = device.display_name();
		if device.is_wireless() {
			name.push_str(" (Wi-Fi)");
		}
		let serial = device.serial.clone();
		let mut actions = h_flex().gap_1();
		if online {
			let serial = serial.clone();
			actions = actions.child(
				Button::new(("refresh", ix))
					.icon(Icon::from(AppIcon::Refresh))
					.ghost()
					.xsmall()
					.tooltip(t!("common.refresh"))
					.on_click(cx.listener(move |this, _, _, cx| {
						// The card around takes clicks too.
						cx.stop_propagation();
						this.engine.send(Command::RefreshDevice(serial.clone()))
					})),
			);
		}
		if device.is_wireless() {
			let serial = serial.clone();
			actions = actions.child(
				Button::new(("disconnect", ix))
					.label(t!("dev.disconnect"))
					.xsmall()
					.outline()
					.on_click(cx.listener(move |this, _, _, cx| {
						cx.stop_propagation();
						this.engine
							.send(Command::DisconnectWireless(serial.clone()))
					})),
			);
		}

		let body = if let Some(hint) = state_hint(&device.state) {
			div()
				.text_sm()
				.text_color(theme.muted_foreground)
				.child(hint)
				.into_any_element()
		} else {
			match self.infos.get(&device.serial) {
				None => h_flex()
					.gap_2()
					.text_sm()
					.child(Spinner::new().xsmall())
					.child(t!("dev.reading").into_owned())
					.into_any_element(),
				Some(info) => info_grid(ix, info, &theme, cx).into_any_element(),
			}
		};

		let pick = device.serial.clone();
		v_flex()
			.id(("device", ix))
			.p_4()
			.gap_3()
			.rounded(theme.radius_lg)
			.border_1()
			.border_color(if selected {
				theme.list_active_border
			} else {
				theme.border
			})
			.bg(theme.muted)
			// Clicking an online card makes it the headset the app works with.
			.when(online && !selected, |card| {
				card.cursor_pointer()
					.hover(|s| s.border_color(theme.list_active_border.opacity(0.5)))
					.on_click(
						cx.listener(move |_, _, _, cx| cx.emit(DevicesEvent::Select(pick.clone()))),
					)
			})
			.child(
				h_flex()
					.gap_3()
					.child(div().size_2().flex_shrink_0().rounded_full().bg(dot))
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(name))
							.child(
								h_flex()
									.gap_2()
									.text_sm()
									.text_color(theme.muted_foreground)
									.child(div().mono(cx).child(serial))
									.child(div().child(state_label(&device.state))),
							),
					)
					.child(actions),
			)
			.child(body)
			.into_any_element()
	}
}

fn info_grid(ix: usize, info: &DeviceInfo, theme: &Theme, cx: &App) -> impl IntoElement {
	let row = |label: String, value: AnyElement| {
		h_flex()
			.gap_4()
			.text_sm()
			.child(
				div()
					.w(px(140.))
					.flex_shrink_0()
					.text_color(theme.muted_foreground)
					.child(label),
			)
			.child(value)
	};
	let battery = match info.battery {
		Some(b) => format!(
			"{}%{}",
			b.level,
			if b.charging {
				t!("dev.charging")
			} else {
				"".into()
			}
		),
		None => t!("dev.unknown").into_owned(),
	};
	let storage = match info.storage {
		Some(s) => {
			let used = s.total_bytes.saturating_sub(s.free_bytes);
			let fraction = if s.total_bytes == 0 {
				0.
			} else {
				used as f32 / s.total_bytes as f32
			};
			h_flex()
				.gap_3()
				.child(
					Progress::new(("storage", ix))
						.value(fraction * 100.)
						.w(px(180.)),
				)
				.child(
					div().child(
						t!(
							"dev.free_of",
							free = format_bytes(s.free_bytes),
							total = format_bytes(s.total_bytes)
						)
						.into_owned(),
					),
				)
				.into_any_element()
		}
		None => div()
			.child(t!("dev.unknown").into_owned())
			.into_any_element(),
	};
	v_flex()
		.gap_1()
		.child(row(
			t!("dev.model").into(),
			div().child(info.model.replace('_', " ")).into_any_element(),
		))
		.child(row(
			t!("dev.android").into(),
			div()
				.mono(cx)
				.child(info.android_version.clone())
				.into_any_element(),
		))
		.child(row(
			t!("dev.battery").into(),
			div().child(battery).into_any_element(),
		))
		.child(row(t!("dev.storage").into(), storage))
}

pub fn state_label(state: &DeviceState) -> String {
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

pub fn state_hint(state: &DeviceState) -> Option<String> {
	Some(
		match state {
			DeviceState::Online => return None,
			DeviceState::Unauthorized => t!("dev.hint_unauthorized"),
			DeviceState::Authorizing | DeviceState::Connecting => t!("dev.hint_waiting"),
			DeviceState::Offline => t!("dev.hint_offline"),
			DeviceState::NoPermissions => t!("dev.hint_no_permissions"),
			DeviceState::Other(_) => t!("dev.hint_other"),
		}
		.into_owned(),
	)
}

impl Render for DevicesView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let adb = match &self.adb {
			None | Some(AdbStatus::Starting) => h_flex()
				.gap_2()
				.child(Spinner::new().xsmall())
				.child(t!("dev.starting").into_owned())
				.into_any_element(),
			Some(AdbStatus::Running { version }) => div()
				.text_color(theme.muted_foreground)
				.child(t!("dev.running", version = version).into_owned())
				.into_any_element(),
			Some(AdbStatus::Unavailable(e) | AdbStatus::Missing(e)) => div()
				.text_color(theme.danger)
				.truncate()
				.child(t!("dev.unavailable", error = e).into_owned())
				.into_any_element(),
		};
		let header = h_flex()
			.px_5()
			.pt_4()
			.pb_3()
			.gap_4()
			.border_b_1()
			.border_color(theme.border)
			.child(
				div()
					.text_xl()
					.flex_shrink_0()
					.child(t!("tab.devices").into_owned()),
			)
			.child(div().flex_1().min_w_0().text_sm().child(adb))
			.child(
				h_flex()
					.flex_shrink_0()
					.gap_2()
					.child(
						div()
							.text_color(theme.muted_foreground)
							.child(t!("dev.wifi").into_owned()),
					)
					.child(self.wifi_form(cx)),
			);
		let body = if self.devices.is_empty() {
			self.help(cx)
		} else {
			let cards: Vec<AnyElement> = self
				.devices
				.iter()
				.enumerate()
				.map(|(ix, d)| self.device_card(ix, d, cx))
				.collect();
			div()
				.id("devices")
				.size_full()
				.overflow_y_scroll()
				.child(v_flex().p_5().gap_3().max_w(px(900.)).children(cards))
				.into_any_element()
		};
		v_flex()
			.size_full()
			.child(header)
			.child(div().flex_1().min_h_0().child(body))
	}
}
