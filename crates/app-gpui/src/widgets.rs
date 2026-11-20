//! Pieces every view builds with: tables, cards, badges, confirmations.

use rust_i18n::t;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme, Sizable, Theme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::{Command, Engine};

pub struct Confirm {
	pub title: String,
	pub text: String,
	pub ok: String,
	pub danger: bool,
}

/// Asks first, then sends `command`. Dialog callbacks are `Fn`, so the command
/// waits in a cell until OK takes it.
pub fn confirm(
	engine: &Rc<Engine>,
	confirm: Confirm,
	command: Command,
	window: &mut Window,
	cx: &mut App,
) {
	let engine = engine.clone();
	let command = Rc::new(RefCell::new(Some(command)));
	window.open_alert_dialog(cx, move |alert, _, _| {
		let engine = engine.clone();
		let command = command.clone();
		alert
			.title(confirm.title.clone())
			.description(confirm.text.clone())
			.confirm()
			.ok_text(confirm.ok.clone())
			.cancel_text(t!("common.cancel").into_owned())
			.when(confirm.danger, |a| a.ok_variant(ButtonVariant::Danger))
			.on_ok(move |_, _, _| {
				if let Some(command) = command.borrow_mut().take() {
					engine.send(command);
				}
				true
			})
	});
}

/// A menu item that sends one command.
pub fn send_item(engine: &Rc<Engine>, label: String, command: Command) -> PopupMenuItem {
	let engine = engine.clone();
	let command = RefCell::new(Some(command));
	PopupMenuItem::new(label).on_click(move |_, _, _| {
		if let Some(command) = command.borrow_mut().take() {
			engine.send(command);
		}
	})
}

pub fn card(title: String, content: impl IntoElement, theme: &Theme) -> Div {
	v_flex()
		.gap_3()
		.p_4()
		.rounded(theme.radius_lg)
		.border_1()
		.border_color(theme.border)
		.bg(theme.muted)
		.child(div().text_color(theme.muted_foreground).child(title))
		.child(content)
}

/// Tinted rather than solid: a solid badge on every row is louder than its news.
pub fn soft_tag(color: Hsla, text: String) -> Tag {
	Tag::custom(color.opacity(0.15), color, color.opacity(0.4))
		.small()
		.child(text)
}

pub fn column_header(cx: &App) -> Div {
	let theme = cx.theme();
	h_flex()
		.w_full()
		.h(px(30.))
		.px_5()
		.gap_4()
		.text_sm()
		.text_color(theme.muted_foreground)
		.border_b_1()
		.border_color(theme.border)
}

pub fn table_row(id: impl Into<ElementId>, height: f32, theme: &Theme) -> Stateful<Div> {
	h_flex()
		.id(id)
		.w_full()
		.h(px(height))
		.px_5()
		.gap_4()
		.border_b_1()
		.border_color(theme.border.opacity(0.6))
		.hover(|s| s.bg(theme.list_hover))
}

pub fn centered(text: String, cx: &App) -> AnyElement {
	div()
		.size_full()
		.flex()
		.items_center()
		.justify_center()
		.p_8()
		.text_color(cx.theme().muted_foreground)
		.child(text)
		.into_any_element()
}

pub fn format_date(unix: u64) -> String {
	chrono::DateTime::from_timestamp(unix as i64, 0)
		.map(|t| {
			t.with_timezone(&chrono::Local)
				.format("%Y-%m-%d %H:%M")
				.to_string()
		})
		.unwrap_or_default()
}

pub fn open_folder(path: &Path) {
	let program = if cfg!(windows) {
		"explorer"
	} else if cfg!(target_os = "macos") {
		"open"
	} else {
		"xdg-open"
	};
	if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
		tracing::warn!("could not open {}: {e}", path.display());
	}
}
