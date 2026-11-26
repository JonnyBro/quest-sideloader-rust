use rust_i18n::t;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::{BrokenStream, Command, Engine};

pub fn open(
	engine: Rc<Engine>,
	serial: String,
	title: String,
	broken: BrokenStream,
	window: &mut Window,
	cx: &mut App,
) {
	window.open_dialog(cx, move |dialog, _, cx| {
		let muted = cx.theme().muted_foreground;
		let retry = {
			let (engine, serial, broken) = (engine.clone(), serial.clone(), broken.clone());
			move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
				engine.send(Command::RetryWithoutStreaming {
					serial: serial.clone(),
					source: broken.source.clone(),
					remove_source: broken.remove_after,
				});
				window.close_dialog(cx);
			}
		};
		dialog
			.title(t!("stream.title", name = title).into_owned())
			.w(px(520.))
			.child(
				v_flex()
					.gap_3()
					.child(div().child(t!("stream.text").into_owned()))
					.child(
						div()
							.text_sm()
							.text_color(muted)
							.child(t!("stream.space").into_owned()),
					),
			)
			.footer(
				h_flex()
					.w_full()
					.justify_end()
					.gap_2()
					.child(
						Button::new("stream-close")
							.label(t!("common.close"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("stream-retry")
							.label(t!("lib.install_no_stream"))
							.primary()
							.on_click(retry),
					),
			)
	});
}
