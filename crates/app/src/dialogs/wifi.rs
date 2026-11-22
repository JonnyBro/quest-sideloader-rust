use rust_i18n::t;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::MdnsService;
use quest_sideloader_core::{Command, Engine};

use crate::theme::Mono;

/// The server finds headsets over time, so the list is asked for again while
/// the dialog is open.
const POLL: Duration = Duration::from_secs(3);

pub struct WifiDialog {
	engine: Rc<Engine>,
	input: Entity<InputState>,
	/// `None` until the first answer.
	found: Option<Result<Vec<MdnsService>, String>>,
	/// Serials of devices adb already has, to mark those as connected.
	connected: HashSet<String>,
	_poll: Task<()>,
}

impl WifiDialog {
	pub fn open(
		engine: Rc<Engine>,
		connected: HashSet<String>,
		window: &mut Window,
		cx: &mut App,
	) -> Entity<Self> {
		let form = cx.new(|cx| {
			let poll = cx.spawn(async move |this: WeakEntity<Self>, cx| {
				loop {
					let Ok(()) =
						this.update(cx, |this, _| this.engine.send(Command::DiscoverWireless))
					else {
						break;
					};
					cx.background_executor().timer(POLL).await;
				}
			});
			Self {
				engine,
				input: cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.50:5555")),
				found: None,
				connected,
				_poll: poll,
			}
		});
		let dialog_form = form.clone();
		window.open_dialog(cx, move |dialog, _, _| {
			dialog
				.title(t!("wifi.title").into_owned())
				.w(px(480.))
				.child(dialog_form.clone())
		});
		form
	}

	pub fn set_found(
		&mut self,
		found: Result<Vec<MdnsService>, String>,
		connected: HashSet<String>,
		cx: &mut Context<Self>,
	) {
		self.found = Some(found);
		self.connected = connected;
		cx.notify();
	}

	fn connect(&self, address: String, window: &mut Window, cx: &mut App) {
		if !address.is_empty() {
			self.engine.send(Command::ConnectWireless(address));
			window.close_dialog(cx);
		}
	}

	fn found_list(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let note = |text: String| {
			div()
				.text_sm()
				.text_color(theme.muted_foreground)
				.child(text)
				.into_any_element()
		};
		match &self.found {
			None => note(t!("wifi.searching").into_owned()),
			Some(Err(e)) => note(t!("wifi.search_failed", error = e).into_owned()),
			Some(Ok(found)) if found.is_empty() => note(t!("wifi.none_found").into_owned()),
			Some(Ok(found)) => v_flex()
				.gap_1()
				.children(found.iter().enumerate().map(|(ix, service)| {
					let connected = self.connected.contains(&service.address);
					let address = service.address.clone();
					let name = service.name.strip_prefix("adb-").unwrap_or(&service.name);
					h_flex()
						.gap_3()
						.px_3()
						.py_2()
						.rounded(theme.radius)
						.border_1()
						.border_color(theme.border)
						.child(
							v_flex()
								.flex_1()
								.min_w_0()
								.child(div().truncate().child(name.to_owned()))
								.child(
									div()
										.text_sm()
										.text_color(theme.muted_foreground)
										.mono(cx)
										.child(service.address.clone()),
								),
						)
						.child(
							Button::new(("wifi-found", ix))
								.label(if connected {
									t!("wifi.connected")
								} else {
									t!("wifi.connect")
								})
								.small()
								.when(!connected, |b| b.primary())
								.when(connected, |b| b.ghost())
								.disabled(connected)
								.when(service.tls, |b| b.tooltip(t!("wifi.tls_hint")))
								.on_click(cx.listener(move |this, _, window, cx| {
									this.connect(address.clone(), window, cx)
								})),
						)
				}))
				.into_any_element(),
		}
	}
}

impl Render for WifiDialog {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let muted = cx.theme().muted_foreground;
		v_flex()
			.gap_3()
			.child(
				div()
					.text_color(muted)
					.text_sm()
					.child(t!("wifi.found_title").into_owned()),
			)
			.child(self.found_list(cx))
			.child(
				div()
					.pt_2()
					.text_sm()
					.text_color(muted)
					.child(t!("wifi.hint").into_owned()),
			)
			.child(Input::new(&self.input))
			.child(
				h_flex()
					.w_full()
					.justify_end()
					.gap_2()
					.child(
						Button::new("wifi-cancel")
							.label(t!("common.cancel"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("wifi-connect")
							.label(t!("wifi.connect"))
							.primary()
							.on_click(cx.listener(|this, _, window, cx| {
								let address = this.input.read(cx).value().trim().to_owned();
								this.connect(address, window, cx)
							})),
					),
			)
	}
}
