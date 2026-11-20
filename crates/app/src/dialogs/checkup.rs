use rust_i18n::t;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::widgets::soft_tag;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
	Ok,
	Warning,
	Problem,
	/// Not set up, and that's fine.
	Skipped,
}

#[derive(Clone)]
pub struct Check {
	pub level: Level,
	pub title: String,
	pub detail: String,
}

impl Check {
	pub fn new(level: Level, title: impl Into<String>, detail: impl Into<String>) -> Self {
		Self {
			level,
			title: title.into(),
			detail: detail.into(),
		}
	}
}

/// "Check everything": what's ready and what to do about what isn't.
pub fn open(checks: Vec<Check>, window: &mut Window, cx: &mut App) {
	window.open_dialog(cx, move |dialog, _, cx| {
		let theme = cx.theme();
		let rows = checks.iter().map(|check| {
			let (label, color) = match check.level {
				Level::Ok => (t!("chk.ok"), theme.success),
				Level::Warning => (t!("chk.warning"), theme.warning),
				Level::Problem => (t!("chk.problem"), theme.danger),
				Level::Skipped => (t!("chk.skipped"), theme.muted_foreground),
			};
			h_flex()
				.gap_3()
				.items_start()
				.child(
					h_flex()
						.w(px(110.))
						.flex_shrink_0()
						.child(soft_tag(color, label.into_owned())),
				)
				.child(
					v_flex()
						.min_w_0()
						.child(div().child(check.title.clone()))
						.when(!check.detail.is_empty(), |d| {
							d.child(
								div()
									.text_sm()
									.text_color(theme.muted_foreground)
									.child(check.detail.clone()),
							)
						}),
				)
		});
		dialog
			.title(t!("chk.title").into_owned())
			.w(px(560.))
			.child(v_flex().gap_3().children(rows))
			.footer(
				h_flex().w_full().justify_end().child(
					Button::new("close")
						.label(t!("common.close"))
						.primary()
						.on_click(|_, window, cx| window.close_dialog(cx)),
				),
			)
	});
}
