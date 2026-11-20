use rust_i18n::t;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Selectable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::settings::{Language, Theme};
use quest_sideloader_core::{Command, Engine, Settings};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
	Look,
	Folder,
	Headset,
}

/// Where to go once the wizard is closed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
	Closed,
	OpenMirrors,
}

type OnClose = Rc<dyn Fn(Outcome, &mut Window, &mut App)>;

/// The first-run setup, shown while there is no settings file yet. Closing it
/// any way saves the settings, so it doesn't come back.
pub struct Welcome {
	engine: Rc<Engine>,
	step: Step,
	draft: Settings,
	default_downloads: PathBuf,
	folder: Entity<InputState>,
	on_close: OnClose,
	_folder_changes: Subscription,
}

impl Welcome {
	pub fn open(
		engine: Rc<Engine>,
		settings: Settings,
		default_downloads: PathBuf,
		on_close: impl Fn(Outcome, &mut Window, &mut App) + 'static,
		window: &mut Window,
		cx: &mut App,
	) {
		let view = cx.new(|cx| {
			let folder = cx.new(|cx| {
				InputState::new(window, cx)
					.default_value(settings.downloads_dir.display().to_string())
			});
			let changes =
				cx.subscribe_in(&folder, window, |this: &mut Self, input, event, _, cx| {
					if matches!(event, InputEvent::Change) {
						this.draft.downloads_dir = input.read(cx).value().trim().into();
					}
				});
			Self {
				engine,
				step: Step::Look,
				draft: settings,
				default_downloads,
				folder,
				on_close: Rc::new(on_close),
				_folder_changes: changes,
			}
		});
		window.open_dialog(cx, move |dialog, _, _| {
			dialog
				.w(px(560.))
				.close_button(false)
				.overlay_closable(false)
				.keyboard(false)
				.child(view.clone())
		});
	}

	fn finish(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
		let mut settings = self.draft.clone();
		if settings.downloads_dir.as_os_str().is_empty() {
			settings.downloads_dir = self.default_downloads.clone();
		}
		settings.normalize();
		self.engine.send(Command::SaveSettings(settings));
		window.close_dialog(cx);
		(self.on_close)(outcome, window, cx);
	}

	fn set_theme(&mut self, theme: Theme, window: &mut Window, cx: &mut Context<Self>) {
		self.draft.theme = theme;
		// Applied right away, so the wizard itself shows the choice.
		crate::theme::apply(theme, window, cx);
	}

	fn set_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
		self.draft.language = language;
		crate::apply_language(language);
		window.refresh();
		cx.notify();
	}

	fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: false,
			directories: true,
			multiple: false,
			prompt: None,
		});
		cx.spawn_in(window, async move |this, cx| {
			if let Ok(Ok(Some(paths))) = paths.await
				&& let Some(dir) = paths.into_iter().next()
			{
				let _ = this.update_in(cx, |this, window, cx| this.set_folder(dir, window, cx));
			}
		})
		.detach();
	}

	fn set_folder(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
		let text = dir.display().to_string();
		self.draft.downloads_dir = dir;
		self.folder
			.update(cx, |input, cx| input.set_value(text, window, cx));
	}

	fn look(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let choice = |id: &'static str, label: String, selected: bool| {
			Button::new(id)
				.label(label)
				.small()
				.outline()
				.selected(selected)
		};
		let language = self.draft.language;
		let theme = self.draft.theme;
		v_flex()
			.gap_4()
			.child(div().child(t!("welcome.intro").into_owned()))
			.child(row(
				t!("set.language").into(),
				h_flex().gap_1().children(
					[
						(Language::System, t!("set.language_system").into_owned()),
						// Language names stay in their own language.
						(Language::English, "English".to_owned()),
						(Language::Russian, "Русский".to_owned()),
					]
					.into_iter()
					.enumerate()
					.map(|(ix, (value, label))| {
						choice(["lang-0", "lang-1", "lang-2"][ix], label, language == value)
							.on_click(cx.listener(move |this, _, window, cx| {
								this.set_language(value, window, cx)
							}))
					}),
				),
				cx,
			))
			.child(row(
				t!("set.theme").into(),
				h_flex().gap_1().children(
					[
						(Theme::System, t!("set.theme_system")),
						(Theme::Dark, t!("set.theme_dark")),
						(Theme::Light, t!("set.theme_light")),
					]
					.into_iter()
					.enumerate()
					.map(|(ix, (value, label))| {
						choice(
							["theme-0", "theme-1", "theme-2"][ix],
							label.into_owned(),
							theme == value,
						)
						.on_click(
							cx.listener(move |this, _, window, cx| {
								this.set_theme(value, window, cx)
							}),
						)
					}),
				),
				cx,
			))
	}

	fn folder(&self, cx: &mut Context<Self>) -> impl IntoElement {
		v_flex()
			.gap_3()
			.child(div().child(t!("welcome.folder_text").into_owned()))
			.child(
				h_flex()
					.gap_2()
					.child(div().flex_1().child(Input::new(&self.folder).small()))
					.child(
						Button::new("browse")
							.label(t!("common.browse"))
							.small()
							.outline()
							.on_click(
								cx.listener(|this, _, window, cx| this.pick_folder(window, cx)),
							),
					)
					.child(
						Button::new("default")
							.label(t!("set.default"))
							.small()
							.ghost()
							.on_click(cx.listener(|this, _, window, cx| {
								let dir = this.default_downloads.clone();
								this.set_folder(dir, window, cx);
							})),
					),
			)
			.child(
				Checkbox::new("keep-downloads")
					.label(t!("set.keep_downloads").into_owned())
					.checked(self.draft.keep_downloads)
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.draft.keep_downloads = *checked;
						cx.notify();
					})),
			)
	}

	fn buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let go = |id: &'static str, label: String, step: Step| {
			Button::new(id)
				.label(label)
				.small()
				.on_click(cx.listener(move |this, _, _, cx| {
					this.step = step;
					cx.notify();
				}))
		};
		let close = |id: &'static str, label: String, outcome: Outcome| {
			Button::new(id)
				.label(label)
				.small()
				.on_click(cx.listener(move |this, _, window, cx| this.finish(outcome, window, cx)))
		};
		let right = match self.step {
			Step::Look => {
				h_flex().child(go("next", t!("welcome.next").into(), Step::Folder).primary())
			}
			Step::Folder => h_flex()
				.gap_2()
				.child(go("back", t!("welcome.back").into(), Step::Look).ghost())
				.child(go("next", t!("welcome.next").into(), Step::Headset).primary()),
			Step::Headset => h_flex()
				.gap_2()
				.child(go("back", t!("welcome.back").into(), Step::Folder).ghost())
				.child(
					close(
						"mirror",
						t!("welcome.add_mirror").into(),
						Outcome::OpenMirrors,
					)
					.outline(),
				)
				.child(close("finish", t!("welcome.finish").into(), Outcome::Closed).primary()),
		};
		h_flex()
			.pt_2()
			.child(close("skip", t!("welcome.skip").into(), Outcome::Closed).ghost())
			.child(div().flex_1())
			.child(right)
	}
}

fn row(label: String, control: impl IntoElement, cx: &App) -> impl IntoElement {
	h_flex()
		.gap_4()
		.child(
			div()
				.w(px(120.))
				.text_color(cx.theme().muted_foreground)
				.child(label),
		)
		.child(control)
}

impl Render for Welcome {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let step = match self.step {
			Step::Look => 1,
			Step::Folder => 2,
			Step::Headset => 3,
		};
		let body = match self.step {
			Step::Look => self.look(cx).into_any_element(),
			Step::Folder => self.folder(cx).into_any_element(),
			Step::Headset => v_flex()
				.gap_3()
				.child(div().child(t!("welcome.headset_text").into_owned()))
				.child(div().child(t!("welcome.mirror_text").into_owned()))
				.into_any_element(),
		};
		v_flex()
			.gap_4()
			.child(
				v_flex()
					.child(div().text_xl().child(t!("welcome.title").into_owned()))
					.child(
						div()
							.text_sm()
							.text_color(cx.theme().muted_foreground)
							.child(t!("welcome.step", n = step, total = 3).into_owned()),
					),
			)
			.child(body)
			.child(self.buttons(cx))
	}
}
