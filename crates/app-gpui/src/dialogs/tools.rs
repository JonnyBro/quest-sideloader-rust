use rust_i18n::t;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::{Command, Engine, platform_tools, rclone_tool};

/// Offers to download adb when the engine can't find it.
pub fn open_adb(engine: Rc<Engine>, window: &mut Window, cx: &mut App) {
	let can_install = platform_tools::download_url().is_some();
	window.open_dialog(cx, move |dialog, _, cx| {
		let muted = cx.theme().muted_foreground;
		let mut body = v_flex()
			.gap_3()
			.child(div().child(t!("adb.text").into_owned()));
		if cfg!(windows) {
			body = body.child(
				h_flex()
					.gap_1()
					.flex_wrap()
					.child(div().child(t!("adb.windows_driver").into_owned()))
					.child(link(
						"driver",
						t!("adb.driver_link").into(),
						platform_tools::QUEST_DRIVER_URL,
					)),
			);
		}
		body = body.child(if can_install {
			h_flex()
				.flex_wrap()
				.text_sm()
				.text_color(muted)
				.child(div().child(t!("adb.terms").into_owned()))
				.child(link(
					"terms",
					t!("adb.terms_link").into(),
					platform_tools::TERMS_URL,
				))
				.into_any_element()
		} else {
			div()
				.text_sm()
				.text_color(muted)
				.child(t!("adb.no_build").into_owned())
				.into_any_element()
		});
		let engine = engine.clone();
		dialog
			.title(t!("adb.title").into_owned())
			.w(px(520.))
			.child(body)
			.footer(footer(
				can_install.then_some(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
					engine.send(Command::InstallAdb);
					window.close_dialog(cx);
				}),
				platform_tools::INFO_URL,
			))
	});
}

/// Shown when a sync fails because rclone is missing.
pub fn open_rclone(engine: Rc<Engine>, window: &mut Window, cx: &mut App) {
	let can_install = rclone_tool::platform().is_some();
	window.open_dialog(cx, move |dialog, _, cx| {
		let mut body = v_flex()
			.gap_3()
			.child(div().child(t!("rclone.text").into_owned()));
		if !can_install {
			body = body.child(
				div()
					.text_sm()
					.text_color(cx.theme().muted_foreground)
					.child(t!("rclone.no_build").into_owned()),
			);
		}
		let engine = engine.clone();
		dialog
			.title(t!("rclone.title").into_owned())
			.w(px(520.))
			.child(body)
			.footer(footer(
				can_install.then_some(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
					engine.send(Command::InstallRclone);
					window.close_dialog(cx);
				}),
				rclone_tool::INFO_URL,
			))
	});
}

fn link(id: &'static str, label: String, url: &'static str) -> Button {
	Button::new(id)
		.label(label)
		.link()
		.on_click(move |_, _, cx| cx.open_url(url))
}

fn footer(
	install: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
	page: &'static str,
) -> impl IntoElement {
	h_flex()
		.w_full()
		.gap_2()
		.child(
			Button::new("not-now")
				.label(t!("adb.not_now"))
				.ghost()
				.on_click(|_, window, cx| window.close_dialog(cx)),
		)
		.child(div().flex_1())
		.child(
			Button::new("open-page")
				.label(t!("adb.open_page"))
				.outline()
				.on_click(move |_, window, cx| {
					cx.open_url(page);
					window.close_dialog(cx);
				}),
		)
		.children(install.map(|install| {
			Button::new("install")
				.label(t!("adb.install"))
				.primary()
				.on_click(install)
		}))
}
