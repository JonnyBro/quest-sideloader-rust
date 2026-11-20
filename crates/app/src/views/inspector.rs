use rust_i18n::t;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Sizable, Theme, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::rename::Inspection;
use quest_sideloader_core::{Command, Engine, format_bytes};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::widgets::card;

enum State {
	Empty,
	Loading,
	Ready(Box<Inspection>),
	Failed(String),
}

/// What an APK is: manifest values, signature, contents, hashes.
pub struct InspectorView {
	engine: Rc<Engine>,
	path: Option<PathBuf>,
	id: u64,
	state: State,
}

impl InspectorView {
	pub fn new(engine: Rc<Engine>) -> Self {
		Self {
			engine,
			path: None,
			id: 0,
			state: State::Empty,
		}
	}

	/// Reads `path` (an APK or a release folder) and shows it.
	pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
		self.id += 1;
		self.engine.send(Command::Inspect {
			id: self.id,
			path: path.clone(),
		});
		self.path = Some(path);
		self.state = State::Loading;
		cx.notify();
	}

	pub fn on_result(
		&mut self,
		id: u64,
		result: Result<Box<Inspection>, String>,
		cx: &mut Context<Self>,
	) {
		if id != self.id {
			return;
		}
		self.state = match result {
			Ok(inspection) => State::Ready(inspection),
			Err(e) => State::Failed(e),
		};
		cx.notify();
	}

	fn pick(&mut self, directories: bool, window: &mut Window, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: !directories,
			directories,
			multiple: false,
			prompt: None,
		});
		cx.spawn_in(window, async move |this, cx| {
			if let Ok(Ok(Some(paths))) = paths.await
				&& let Some(path) = paths.into_iter().next()
			{
				let _ = this.update(cx, |this, cx| this.open(path, cx));
			}
		})
		.detach();
	}
}

fn dash() -> String {
	"-".to_owned()
}

fn yes_no(value: bool) -> String {
	if value {
		t!("common.yes")
	} else {
		t!("common.no")
	}
	.into_owned()
}

fn join_or_dash(items: &[String]) -> String {
	if items.is_empty() {
		dash()
	} else {
		items.join(", ")
	}
}

/// Label and value rows; `mono` values are identifiers and hashes.
fn rows(items: Vec<(String, String, bool)>, theme: &Theme, cx: &App) -> impl IntoElement {
	v_flex()
		.gap_1()
		.text_sm()
		.children(items.into_iter().map(|(label, value, mono)| {
			h_flex()
				.gap_4()
				.items_start()
				.child(
					div()
						.w(px(200.))
						.flex_shrink_0()
						.text_color(theme.muted_foreground)
						.child(label),
				)
				.child(
					div()
						.flex_1()
						.min_w_0()
						.when(mono, |d| d.mono(cx))
						.child(value),
				)
		}))
}

fn list(items: Vec<String>, theme: &Theme, cx: &App) -> impl IntoElement {
	v_flex()
		.text_sm()
		.mono(cx)
		.when(items.is_empty(), |d| {
			d.child(div().text_color(theme.muted_foreground).child(dash()))
		})
		.children(items.into_iter().map(|item| div().child(item)))
}

fn details(info: &Inspection, theme: &Theme, cx: &App) -> impl IntoElement {
	let version = match (&info.version_name, info.version_code) {
		(Some(name), Some(code)) => format!("{name} ({code})"),
		(Some(name), None) => name.clone(),
		(None, Some(code)) => code.to_string(),
		(None, None) => dash(),
	};
	let app = rows(
		vec![
			(
				t!("ins.label").into(),
				info.label.clone().unwrap_or_else(dash),
				false,
			),
			(t!("rn.package").into(), info.package.clone(), true),
			(t!("rn.version").into(), version, true),
			(t!("ins.size").into(), format_bytes(info.size), true),
			(
				t!("ins.sdk").into(),
				t!(
					"ins.sdk_values",
					min = info.min_sdk.clone().unwrap_or_else(dash),
					target = info.target_sdk.clone().unwrap_or_else(dash),
					compile = info.compile_sdk.clone().unwrap_or_else(dash)
				)
				.into(),
				false,
			),
			(
				"OpenGL ES".into(),
				info.gl_es.clone().unwrap_or_else(dash),
				true,
			),
			("debuggable".into(), yes_no(info.debuggable), false),
			(
				"extractNativeLibs".into(),
				info.extract_native_libs.map_or_else(dash, yes_no),
				false,
			),
		],
		theme,
		cx,
	);
	let quest = rows(
		vec![
			(
				t!("ins.devices").into(),
				join_or_dash(&info.supported_devices),
				true,
			),
			(t!("ins.loader").into(), yes_no(info.platform_loader), false),
			("Firebase".into(), yes_no(info.firebase), false),
		],
		theme,
		cx,
	);
	let sig = &info.signature;
	let schemes = if sig.schemes.is_empty() {
		t!("ins.unsigned").into_owned()
	} else {
		sig.schemes.join(", ")
	};
	let check = match &sig.v2_valid {
		Some(Ok(())) => t!("ins.valid").into_owned(),
		Some(Err(e)) => t!("ins.invalid", error = e).into_owned(),
		None => dash(),
	};
	let mut signature_rows = vec![
		(t!("ins.schemes").into(), schemes, true),
		(t!("ins.v2_check").into(), check, false),
	];
	for cert in &sig.certificates {
		signature_rows.push((t!("ins.certificate").into(), cert.subject.clone(), false));
		signature_rows.push(("SHA-256".into(), cert.sha256.clone(), true));
	}
	let signature = v_flex()
		.gap_2()
		.child(rows(signature_rows, theme, cx))
		.children(sig.ours.as_ref().map(|package| {
			div()
				.text_sm()
				.text_color(theme.info)
				.child(t!("ins.ours", package = package).into_owned())
		}));
	let components = info
		.components
		.iter()
		.map(|(kind, n)| format!("{kind}: {n}"))
		.collect::<Vec<_>>()
		.join(", ");
	let contents = rows(
		vec![
			(t!("ins.abis").into(), join_or_dash(&info.abis), true),
			(t!("ins.locales").into(), join_or_dash(&info.locales), true),
			("dex".into(), info.dex_files.to_string(), true),
			(
				t!("ins.native").into(),
				info.native_libraries.to_string(),
				true,
			),
			(t!("ins.entries").into(), info.entries.to_string(), true),
			(t!("ins.components").into(), components, false),
		],
		theme,
		cx,
	);
	let features: Vec<String> = info
		.features
		.iter()
		.map(|f| {
			if f.required {
				f.name.clone()
			} else {
				format!("{} ({})", f.name, t!("ins.optional"))
			}
		})
		.collect();
	let hashes = rows(
		vec![
			("SHA-256".into(), info.sha256.clone(), true),
			("MD5".into(), info.md5.clone(), true),
		],
		theme,
		cx,
	);
	v_flex()
		.gap_4()
		.child(card(t!("ins.app").into(), app, theme))
		.child(card("Quest".into(), quest, theme))
		.child(card(t!("ins.signature").into(), signature, theme))
		.child(card(t!("ins.contents").into(), contents, theme))
		.child(card(
			t!("ins.permissions", n = info.permissions.len()).into(),
			list(info.permissions.clone(), theme, cx),
			theme,
		))
		.child(card(
			t!("ins.features", n = info.features.len()).into(),
			list(features, theme, cx),
			theme,
		))
		.child(card(t!("ins.hashes").into(), hashes, theme))
}

impl Render for InspectorView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let loading = matches!(self.state, State::Loading);
		let header = h_flex()
			.px_5()
			.pt_4()
			.pb_3()
			.gap_3()
			.border_b_1()
			.border_color(theme.border)
			.child(
				v_flex()
					.flex_1()
					.min_w_0()
					.child(div().text_xl().child(t!("ins.heading").into_owned()))
					.children(self.path.as_ref().map(|p| {
						div()
							.truncate()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(p.display().to_string())
					})),
			)
			.when_some(self.path.clone(), |h, path| {
				h.child(
					Button::new("reread")
						.icon(Icon::from(AppIcon::Refresh))
						.ghost()
						.small()
						.disabled(loading)
						.tooltip(t!("rn.reanalyze"))
						.on_click(cx.listener(move |this, _, _, cx| this.open(path.clone(), cx))),
				)
			})
			.child(
				Button::new("open-apk")
					.label(t!("rn.add_apk"))
					.icon(Icon::new(IconName::File))
					.small()
					.outline()
					.on_click(cx.listener(|this, _, window, cx| this.pick(false, window, cx))),
			)
			.child(
				Button::new("open-folder")
					.label(t!("rn.add_folder"))
					.icon(Icon::new(IconName::FolderOpen))
					.small()
					.outline()
					.on_click(cx.listener(|this, _, window, cx| this.pick(true, window, cx))),
			);
		let body = match &self.state {
			State::Empty => v_flex()
				.size_full()
				.items_center()
				.justify_center()
				.gap_1()
				.p_8()
				.child(div().text_lg().child(t!("ins.empty_title").into_owned()))
				.child(
					div()
						.text_color(theme.muted_foreground)
						.child(t!("ins.empty").into_owned()),
				)
				.into_any_element(),
			State::Loading => h_flex()
				.p_5()
				.gap_2()
				.child(Spinner::new())
				.child(t!("ins.reading").into_owned())
				.into_any_element(),
			State::Failed(e) => div()
				.p_5()
				.text_color(theme.danger)
				.child(e.clone())
				.into_any_element(),
			State::Ready(info) => div()
				.id("inspection")
				.size_full()
				.overflow_y_scroll()
				.child(
					div()
						.p_5()
						.max_w(px(1000.))
						.child(details(info, &theme, cx)),
				)
				.into_any_element(),
		};
		v_flex()
			.size_full()
			.child(header)
			.child(div().flex_1().min_h_0().child(body))
	}
}
