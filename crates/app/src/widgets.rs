//! Pieces every view builds with: tables, cards, badges, confirmations.

use rust_i18n::t;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
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

/// Asks for a file or folder name. `on_ok` gets it trimmed; it isn't called
/// while the name is empty or one `valid` rejects.
pub fn prompt_name(
	title: String,
	initial: &str,
	ok: String,
	valid: fn(&str) -> bool,
	on_ok: impl Fn(String, &mut App) + 'static,
	window: &mut Window,
	cx: &mut App,
) {
	let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.to_owned()));
	input.update(cx, |state, cx| {
		state.focus(window, cx);
		state.select_all(window, cx);
	});
	let submit = {
		let input = input.clone();
		Rc::new(move |window: &mut Window, cx: &mut App| {
			let name = input.read(cx).value().trim().to_owned();
			if valid(&name) {
				window.close_dialog(cx);
				on_ok(name, cx);
			}
		})
	};
	let enter = {
		let submit = submit.clone();
		window.subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
			if matches!(event, InputEvent::PressEnter { .. }) {
				submit(window, cx);
			}
		})
	};
	// The dialog's builder runs on every render; it holds the subscription so
	// Enter keeps working while the dialog is open.
	let enter = Rc::new(enter);
	window.open_dialog(cx, move |dialog, _, _| {
		let _ = &enter;
		let submit = submit.clone();
		dialog
			.title(title.clone())
			.w(px(440.))
			.child(Input::new(&input))
			.footer(
				h_flex()
					.w_full()
					.justify_end()
					.gap_2()
					.child(
						Button::new("name-cancel")
							.label(t!("common.cancel"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("name-ok")
							.label(ok.clone())
							.primary()
							.on_click(move |_, window, cx| submit(window, cx)),
					),
			)
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

/// `element` with `tip` shown on hover.
pub fn with_tip(id: impl Into<ElementId>, element: impl IntoElement, tip: String) -> AnyElement {
	div()
		.id(id)
		.child(element)
		.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
		.into_any_element()
}

/// What "update available" is about: the version on the headset, when it was
/// installed, and the version it would update to.
pub fn update_tip(have: Option<u64>, latest: Option<u64>, installed: Option<u64>) -> String {
	let version = |v: Option<u64>| v.map_or_else(|| "?".to_owned(), |v| v.to_string());
	let mut tip = t!(
		"common.update_tip",
		have = version(have),
		latest = version(latest)
	)
	.into_owned();
	if let Some(installed) = installed {
		tip.push('\n');
		tip.push_str(&t!("common.installed_at", date = format_date(installed)));
	}
	tip
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

/// The sidebar's width; views measure what's left of the window from it.
pub const SIDEBAR_WIDTH: f32 = 240.;
/// The gap `table_row` and `column_header` put between columns.
const COLUMN_GAP: f32 = 16.;

/// Width a view's content has: the window minus the sidebar and `beside`, a
/// side panel. gpui knows element sizes only after layout, so this goes by the
/// window.
pub fn content_width(window: &Window, beside: f32) -> f32 {
	window.viewport_size().width.as_f32() - SIDEBAR_WIDTH - beside
}

/// Which optional columns fit in `width`. `fixed` is what always shows (row
/// padding, cover, the name column's minimum); each column is (width,
/// priority), and the lowest priorities are hidden first.
pub fn fit_columns(width: f32, fixed: f32, columns: &[(f32, u8)]) -> Vec<bool> {
	let mut shown = vec![true; columns.len()];
	let mut used = fixed + columns.iter().map(|(w, _)| w + COLUMN_GAP).sum::<f32>();
	let mut order: Vec<usize> = (0..columns.len()).collect();
	order.sort_by_key(|&i| columns[i].1);
	for i in order {
		if used <= width {
			break;
		}
		shown[i] = false;
		used -= columns[i].0 + COLUMN_GAP;
	}
	shown
}

#[cfg(test)]
mod tests {
	use super::*;
	use ::core::prelude::v1::test;

	#[test]
	fn hides_lowest_priority_first() {
		let columns = [(100., 2), (100., 0), (100., 1)];
		assert_eq!(fit_columns(1000., 100., &columns), [true, true, true]);
		// 100 + 3 * 116 = 448; dropping the priority-0 column leaves 332.
		assert_eq!(fit_columns(400., 100., &columns), [true, false, true]);
		assert_eq!(fit_columns(250., 100., &columns), [true, false, false]);
		assert_eq!(fit_columns(50., 100., &columns), [false, false, false]);
	}
}
