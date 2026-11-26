use rust_i18n::t;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::sideload::script::{LineKind, ScriptAnalysis, ScriptLine, Verdict};
use quest_sideloader_core::{Command, Engine, ReviewDecision, TaskId};

use crate::theme::Mono;

pub struct PendingReview {
	pub task: TaskId,
	pub title: String,
	pub analysis: ScriptAnalysis,
}

/// Can't be dismissed without a decision; `on_answer` runs after it is sent.
pub fn open(
	engine: Rc<Engine>,
	review: PendingReview,
	on_answer: impl Fn(&mut Window, &mut App) + 'static,
	window: &mut Window,
	cx: &mut App,
) {
	let review = Rc::new(review);
	let on_answer = Rc::new(on_answer);
	window.open_dialog(cx, move |dialog, window, cx| {
		let theme = cx.theme();
		let suspicious_bg = theme.warning.opacity(0.18);
		let blocked_bg = theme.danger.opacity(0.18);
		let muted = theme.muted_foreground;
		let height = (window.viewport_size().height.as_f32() - 360.).max(160.);

		let suspicious = count(&review.analysis, |v| matches!(v, Verdict::Suspicious(_)));
		let blocked = count(&review.analysis, |v| matches!(v, Verdict::Blocked(_)));
		let lines = review
			.analysis
			.lines
			.iter()
			.map(|line| script_line(line, suspicious_bg, blocked_bg, muted, cx));

		let answer = |id: &'static str, label: String, decision: ReviewDecision| {
			let engine = engine.clone();
			let on_answer = on_answer.clone();
			let task = review.task;
			Button::new(id).label(label).on_click(move |_, window, cx| {
				engine.send(Command::ReviewScript { task, decision });
				window.close_dialog(cx);
				on_answer(window, cx);
			})
		};

		let swatch = |color: Hsla, text: String| {
			h_flex()
				.gap_1()
				.child(div().size_3().rounded_sm().bg(color))
				.child(div().child(text))
		};

		dialog
			.title(t!("rev.heading", title = review.title).into_owned())
			.w(px(880.))
			.close_button(false)
			.overlay_closable(false)
			.keyboard(false)
			.child(
				v_flex()
					.gap_3()
					.child(div().child(t!("rev.intro").into_owned()))
					.children(review.analysis.package.as_ref().map(|package| {
						h_flex()
							.gap_2()
							.child(div().child(t!("rev.package").into_owned()))
							.child(div().mono(cx).child(package.clone()))
					}))
					.child(
						h_flex()
							.gap_4()
							.flex_wrap()
							.text_sm()
							.child(swatch(suspicious_bg, t!("rev.needs_permission").into()))
							.child(swatch(blocked_bg, t!("rev.blocked").into()))
							.child(div().text_color(muted).child(t!("rev.grey").into_owned())),
					)
					.child(
						div()
							.id("script")
							.max_h(px(height))
							.overflow_y_scroll()
							.border_1()
							.border_color(theme.border)
							.rounded(theme.radius)
							.py_1()
							.text_sm()
							.mono(cx)
							.children(lines),
					)
					.child(div().child(
						t!("rev.counts", suspicious = suspicious, blocked = blocked).into_owned(),
					)),
			)
			.footer(
				h_flex()
					.w_full()
					.gap_2()
					.child(
						answer("cancel", t!("rev.cancel").into(), ReviewDecision::Cancel).outline(),
					)
					.child(div().flex_1())
					.child(
						answer("safe", t!("rev.safe_only").into(), ReviewDecision::SafeOnly)
							.primary()
							.tooltip(t!("rev.safe_only_hint")),
					)
					.when(suspicious > 0, |row| {
						row.child(
							answer("all", t!("rev.run_all").into(), ReviewDecision::RunAll)
								.warning()
								.tooltip(t!("rev.run_all_hint")),
						)
					}),
			)
	});
}

fn count(analysis: &ScriptAnalysis, pred: impl Fn(&Verdict) -> bool) -> usize {
	analysis
		.lines
		.iter()
		.filter(|l| l.verdict().is_some_and(&pred))
		.count()
}

fn script_line(
	line: &ScriptLine,
	suspicious: Hsla,
	blocked: Hsla,
	muted: Hsla,
	cx: &App,
) -> impl IntoElement {
	let (fill, reason) = match &line.kind {
		LineKind::Command {
			verdict: Verdict::Suspicious(reason),
			..
		} => (Some(suspicious), Some(reason.clone())),
		LineKind::Command {
			verdict: Verdict::Blocked(reason),
			..
		} => (Some(blocked), Some(reason.clone())),
		_ => (None, None),
	};
	let ignored = matches!(line.kind, LineKind::Ignored(_));
	v_flex()
		.px_2()
		.when_some(fill, |d, fill| d.bg(fill))
		.child(
			h_flex()
				.gap_3()
				.child(
					div()
						.w(px(32.))
						.flex_shrink_0()
						.text_right()
						.text_color(muted)
						.child(line.number.to_string()),
				)
				.child(
					div()
						.whitespace_nowrap()
						.when(ignored, |d| d.text_color(muted))
						.child(line.text.clone()),
				),
		)
		.children(reason.map(|reason| {
			div()
				.pl(px(44.))
				.pb_1()
				.font_family(cx.theme().font_family.clone())
				.font_weight(FontWeight::SEMIBOLD)
				.child(reason)
		}))
}
