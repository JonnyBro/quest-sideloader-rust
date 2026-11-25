use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::rename::KeyStore;
use quest_sideloader_core::settings::{Language, MAX_CONCURRENT_DOWNLOADS, SpeedUnit, Theme};
use quest_sideloader_core::{AppPaths, Command, Engine, Settings};

use crate::dialogs::mirror::{self, MirrorForm};
use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::widgets::{column_header, table_row};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
	Appearance,
	Downloads,
	Mirrors,
	Tools,
	Keys,
}

impl Category {
	const ALL: [Category; 5] = [
		Category::Appearance,
		Category::Downloads,
		Category::Mirrors,
		Category::Tools,
		Category::Keys,
	];

	fn label(self) -> String {
		match self {
			Category::Appearance => t!("set.cat_appearance"),
			Category::Downloads => t!("set.cat_downloads"),
			Category::Mirrors => t!("mir.heading"),
			Category::Tools => t!("set.cat_tools"),
			Category::Keys => t!("set.cat_keys"),
		}
		.into_owned()
	}

	fn icon(self) -> Icon {
		match self {
			Category::Appearance => Icon::new(IconName::Palette),
			Category::Downloads => Icon::from(AppIcon::Downloads),
			Category::Mirrors => Icon::new(IconName::Globe),
			Category::Tools => Icon::from(AppIcon::Console),
			Category::Keys => Icon::new(IconName::Asterisk),
		}
	}
}

pub enum SettingsEvent {
	/// "Check everything" was clicked; the shell knows the state to check.
	Checkup,
	/// A key was imported or deleted, so the set of known copies changed.
	KeysChanged,
}

struct KeyRow {
	package: String,
	/// The certificate's SHA-256, or why the key file can't be read.
	fingerprint: Result<String, String>,
}

/// Theme and language apply and save at once. Everything else goes into a
/// draft that the bar at the bottom saves or throws away.
pub struct SettingsView {
	engine: Rc<Engine>,
	default_downloads: PathBuf,
	rclone_dir: PathBuf,
	saved: Settings,
	draft: Settings,
	saving: bool,
	/// The next save came from the Save button, so it gets a notice.
	announce: bool,
	category: Category,
	downloads_dir: Entity<InputState>,
	adb_path: Entity<InputState>,
	rclone_path: Entity<InputState>,
	speed: Entity<InputState>,
	keys: KeyStore,
	key_rows: Option<Vec<KeyRow>>,
	key_message: Option<Result<String, String>>,
	/// Game names by package, from the synced catalog.
	game_names: HashMap<String, String>,
	_inputs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
	pub fn new(
		engine: Rc<Engine>,
		paths: &AppPaths,
		settings: Settings,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let mut input = |cx: &mut Context<Self>, placeholder: String| {
			cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
		};
		let downloads_dir = input(cx, String::new());
		let adb_path = input(cx, t!("set.adb_hint").into_owned());
		let rclone_path = input(cx, t!("set.rclone_hint").into_owned());
		let speed = input(cx, "0".to_owned());
		let subscriptions = vec![
			cx.subscribe_in(&downloads_dir, window, |this, input, event, _, cx| {
				if matches!(event, InputEvent::Change) {
					this.draft.downloads_dir = input.read(cx).value().trim().into();
					cx.notify();
				}
			}),
			cx.subscribe_in(&adb_path, window, |this, input, event, _, cx| {
				if matches!(event, InputEvent::Change) {
					this.draft.adb_path = optional_path(&input.read(cx).value());
					cx.notify();
				}
			}),
			cx.subscribe_in(&rclone_path, window, |this, input, event, _, cx| {
				if matches!(event, InputEvent::Change) {
					this.draft.rclone_path = optional_path(&input.read(cx).value());
					cx.notify();
				}
			}),
			cx.subscribe_in(&speed, window, |this, input, event, _, cx| {
				if matches!(event, InputEvent::Change) {
					let text = input.read(cx).value();
					this.draft.download_limit.value = text.trim().parse().unwrap_or(0);
					cx.notify();
				}
			}),
		];
		let mut view = Self {
			engine,
			default_downloads: paths.default_downloads_dir(),
			rclone_dir: paths.rclone_dir(),
			draft: settings.clone(),
			saved: settings,
			saving: false,
			announce: false,
			category: Category::Appearance,
			downloads_dir,
			adb_path,
			rclone_path,
			speed,
			keys: KeyStore::new(paths.signing_dir()),
			key_rows: None,
			key_message: None,
			game_names: HashMap::new(),
			_inputs: subscriptions,
		};
		view.fill_inputs(window, cx);
		view
	}

	pub fn open_mirrors(&mut self, cx: &mut Context<Self>) {
		self.category = Category::Mirrors;
		cx.notify();
	}

	/// The engine saved settings. Returns true when the user asked for that
	/// save with the Save button.
	pub fn on_saved(
		&mut self,
		settings: Settings,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> bool {
		mirror::remove_unused_rclone_configs(&settings.mirrors, &self.rclone_dir);
		if self.is_dirty() && !self.saving {
			// Saved elsewhere (a theme pick, "don't ask again"); keep the edits.
			self.draft.theme = settings.theme;
			self.draft.language = settings.language;
			self.draft.contribute_ignored = settings.contribute_ignored.clone();
			self.draft.update_ignored = settings.update_ignored.clone();
			self.saved = settings;
		} else {
			self.saved = settings.clone();
			self.draft = settings;
			self.fill_inputs(window, cx);
		}
		self.saving = false;
		cx.notify();
		std::mem::take(&mut self.announce)
	}

	pub fn set_game_names(&mut self, names: HashMap<String, String>, cx: &mut Context<Self>) {
		self.game_names = names;
		cx.notify();
	}

	pub fn save_failed(&mut self, cx: &mut Context<Self>) {
		self.saving = false;
		self.announce = false;
		cx.notify();
	}

	/// A copy was built or a key imported; read the key list again.
	pub fn keys_changed(&mut self, cx: &mut Context<Self>) {
		self.key_rows = None;
		cx.notify();
	}

	fn is_dirty(&self) -> bool {
		self.draft != self.saved
	}

	fn fill_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let set = |input: &Entity<InputState>,
		           text: String,
		           window: &mut Window,
		           cx: &mut Context<Self>| {
			input.update(cx, |input, cx| input.set_value(text, window, cx));
		};
		let path_text = |p: &Option<PathBuf>| {
			p.as_ref()
				.map(|p| p.display().to_string())
				.unwrap_or_default()
		};
		set(
			&self.downloads_dir,
			self.draft.downloads_dir.display().to_string(),
			window,
			cx,
		);
		set(&self.adb_path, path_text(&self.draft.adb_path), window, cx);
		set(
			&self.rclone_path,
			path_text(&self.draft.rclone_path),
			window,
			cx,
		);
		set(
			&self.speed,
			self.draft.download_limit.value.to_string(),
			window,
			cx,
		);
	}

	/// Applied and saved right away, without touching the draft's other edits.
	fn save_now(&mut self, change: impl Fn(&mut Settings)) {
		change(&mut self.draft);
		let mut settings = self.saved.clone();
		change(&mut settings);
		self.engine.send(Command::SaveSettings(settings));
	}

	fn set_theme(&mut self, theme: Theme, window: &mut Window, cx: &mut Context<Self>) {
		self.save_now(|s| s.theme = theme);
		crate::theme::apply(&self.draft, window, cx);
		cx.notify();
	}

	fn set_scheme(
		&mut self,
		dark: bool,
		name: String,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		// Picking a scheme also picks its mode; "System" stays until a scheme is clicked.
		self.save_now(|s| {
			if dark {
				s.dark_scheme = name.clone();
				s.theme = Theme::Dark;
			} else {
				s.light_scheme = name.clone();
				s.theme = Theme::Light;
			}
		});
		crate::theme::apply(&self.draft, window, cx);
		cx.notify();
	}

	/// Small previews of the bundled dark or light schemes; a click picks one.
	fn scheme_picker(&self, dark: bool, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let (current, default) = if dark {
			(&self.draft.dark_scheme, crate::theme::DEFAULT_DARK)
		} else {
			(&self.draft.light_scheme, crate::theme::DEFAULT_LIGHT)
		};
		let current = if current.is_empty() {
			default
		} else {
			current.as_str()
		};
		let cards = crate::theme::schemes()
			.iter()
			.filter(|s| s.dark == dark)
			.enumerate()
			.map(|(ix, scheme)| {
				let selected = scheme.name == current;
				// The scheme of the other mode is remembered, shown fainter.
				let in_use = selected
					&& match self.draft.theme {
						Theme::System => true,
						Theme::Dark => dark,
						Theme::Light => !dark,
					};
				let label = if scheme.name == default {
					t!("set.scheme_default").into_owned()
				} else {
					scheme.name.clone()
				};
				let name = scheme.name.clone();
				let line = |width: f32| {
					div()
						.h(px(4.))
						.w(px(width))
						.rounded_full()
						.bg(scheme.foreground.opacity(0.6))
				};
				v_flex()
					.id((if dark { "dark-scheme" } else { "light-scheme" }, ix))
					.w(px(150.))
					.gap_1()
					.p_1()
					.rounded(theme.radius_lg)
					.border_2()
					.border_color(if in_use {
						theme.primary
					} else if selected {
						theme.muted_foreground.opacity(0.4)
					} else {
						theme.transparent
					})
					.cursor_pointer()
					.hover(|s| s.bg(theme.list_hover))
					.on_click(cx.listener(move |this, _, window, cx| {
						this.set_scheme(dark, name.clone(), window, cx)
					}))
					.child(
						h_flex()
							.h(px(64.))
							.rounded(theme.radius)
							.overflow_hidden()
							.border_1()
							.border_color(theme.border)
							.bg(scheme.background)
							.child(div().w(px(36.)).h_full().bg(scheme.surface))
							.child(
								v_flex()
									.flex_1()
									.p_2()
									.gap_1p5()
									.child(line(70.))
									.child(line(50.))
									.child(
										div()
											.mt_1()
											.h(px(12.))
											.w(px(40.))
											.rounded(px(3.))
											.bg(scheme.accent),
									),
							),
					)
					.child(div().px_1().text_sm().truncate().child(label))
			});
		h_flex()
			.gap_2()
			.flex_wrap()
			.children(cards)
			.into_any_element()
	}

	fn set_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
		self.save_now(|s| s.language = language);
		crate::apply_language(language);
		window.refresh();
		cx.notify();
	}

	fn save(&mut self, cx: &mut Context<Self>) {
		let mut settings = self.draft.clone();
		settings.normalize();
		self.saving = true;
		self.announce = true;
		self.engine.send(Command::SaveSettings(settings));
		cx.notify();
	}

	fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		self.draft = self.saved.clone();
		self.fill_inputs(window, cx);
		cx.notify();
	}

	fn pick_path(
		&mut self,
		target: Entity<InputState>,
		directories: bool,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: !directories,
			directories,
			multiple: false,
			prompt: None,
		});
		cx.spawn_in(window, async move |_, cx| {
			if let Ok(Ok(Some(paths))) = paths.await
				&& let Some(path) = paths.into_iter().next()
			{
				let _ = cx.update(|window, cx| {
					target.update(cx, |input, cx| {
						input.set_value(path.display().to_string(), window, cx)
					});
				});
			}
		})
		.detach();
	}

	fn scale_control(&self, cx: &mut Context<Self>) -> AnyElement {
		let scale = crate::theme::scale(cx);
		let step = |id: &'static str, label: &'static str, delta: f32, enabled: bool| {
			Button::new(id)
				.label(label)
				.small()
				.outline()
				.disabled(!enabled)
				.on_click(move |_, window, cx| {
					crate::theme::set_scale(crate::theme::scale(cx) + delta, window, cx)
				})
		};
		v_flex()
			.gap_1()
			.child(
				h_flex()
					.gap_2()
					.child(step(
						"scale-down",
						"-",
						-0.1,
						scale > crate::theme::SCALE_MIN,
					))
					.child(
						div()
							.w(px(56.))
							.text_center()
							.mono(cx)
							.child(format!("{:.0}%", scale * 100.)),
					)
					.child(step("scale-up", "+", 0.1, scale < crate::theme::SCALE_MAX))
					.child(
						Button::new("scale-reset")
							.label(t!("set.scale_reset"))
							.small()
							.outline()
							.disabled((scale - 1.).abs() < f32::EPSILON)
							.on_click(|_, window, cx| crate::theme::set_scale(1., window, cx)),
					),
			)
			.into_any_element()
	}

	fn appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let choice = |id: &'static str, label: String, selected: bool| {
			Button::new(id)
				.label(label)
				.small()
				.outline()
				.selected(selected)
		};
		let theme = self.draft.theme;
		let language = self.draft.language;
		v_flex()
			.gap_4()
			.child(hint(t!("set.instant_hint").into(), cx))
			.child(field(
				t!("set.language").into(),
				h_flex().gap_1().children(
					[
						(
							"lang-system",
							Language::System,
							t!("set.language_system").into_owned(),
						),
						// Language names stay in their own language.
						("lang-en", Language::English, "English".to_owned()),
						("lang-ru", Language::Russian, "Русский".to_owned()),
						("lang-uk", Language::Ukrainian, "Українська".to_owned()),
					]
					.into_iter()
					.map(|(id, value, label)| {
						choice(id, label, language == value).on_click(cx.listener(
							move |this, _, window, cx| this.set_language(value, window, cx),
						))
					}),
				),
				cx,
			))
			.child(field(t!("set.scale").into(), self.scale_control(cx), cx))
			.child(field(
				t!("set.theme").into(),
				h_flex().gap_1().children(
					[("theme-system", Theme::System, t!("set.theme_system"))]
						.into_iter()
						.map(|(id, value, label)| {
							choice(id, label.into_owned(), theme == value).on_click(cx.listener(
								move |this, _, window, cx| this.set_theme(value, window, cx),
							))
						}),
				),
				cx,
			))
			.child(field(
				t!("set.dark_scheme").into(),
				self.scheme_picker(true, cx),
				cx,
			))
			.child(field(
				t!("set.light_scheme").into(),
				self.scheme_picker(false, cx),
				cx,
			))
	}

	#[cfg(target_os = "linux")]
	fn desktop_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
		let installed = crate::desktop::is_installed();
		let run = |install: bool| {
			move |_: &mut Self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
				let result = if install {
					crate::desktop::install()
				} else {
					crate::desktop::uninstall()
				};
				let note = match result {
					Ok(()) if install => {
						Notification::success(t!("set.desktop_added").into_owned())
					}
					Ok(()) => Notification::info(t!("set.desktop_removed").into_owned()),
					Err(e) => Notification::error(e),
				};
				window.push_notification(note, cx);
				cx.notify();
			}
		};
		h_flex()
			.gap_2()
			.child(
				Button::new("desktop-install")
					.small()
					.outline()
					.label(if installed {
						t!("set.desktop_update")
					} else {
						t!("set.desktop_add")
					})
					.on_click(cx.listener(run(true))),
			)
			.when(installed, |this| {
				this.child(
					Button::new("desktop-remove")
						.small()
						.ghost()
						.label(t!("set.desktop_remove"))
						.on_click(cx.listener(run(false))),
				)
			})
			.into_any_element()
	}

	#[cfg(not(target_os = "linux"))]
	fn desktop_buttons(&self, _cx: &mut Context<Self>) -> AnyElement {
		div().into_any_element()
	}

	fn downloads(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let parallel = self.draft.max_concurrent_downloads;
		let unit = self.draft.download_limit.unit;
		let unlimited = self.draft.download_limit.value == 0;
		let downloads_dir = self.downloads_dir.clone();
		v_flex()
			.gap_4()
			.child(field(
				t!("set.downloads_folder").into(),
				h_flex()
					.gap_2()
					.child(
						div()
							.flex_1()
							.child(Input::new(&self.downloads_dir).small()),
					)
					.child(
						Button::new("browse-downloads")
							.label(t!("common.browse"))
							.small()
							.outline()
							.on_click(cx.listener(move |this, _, window, cx| {
								this.pick_path(downloads_dir.clone(), true, window, cx)
							})),
					)
					.child(
						Button::new("default-downloads")
							.label(t!("set.default"))
							.small()
							.outline()
							.on_click(cx.listener(|this, _, window, cx| {
								let text = this.default_downloads.display().to_string();
								this.downloads_dir
									.update(cx, |input, cx| input.set_value(text, window, cx));
							})),
					),
				cx,
			))
			.child(field(
				t!("set.after_installing").into(),
				Checkbox::new("keep-downloads")
					.label(t!("set.keep_downloads").into_owned())
					.checked(self.draft.keep_downloads)
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.draft.keep_downloads = *checked;
						cx.notify();
					})),
				cx,
			))
			.child(field(
				t!("set.before_updating").into(),
				Checkbox::new("backup-before-update")
					.label(t!("set.backup_before_update").into_owned())
					.checked(self.draft.backup_before_update)
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.draft.backup_before_update = *checked;
						cx.notify();
					})),
				cx,
			))
			.child(field(
				t!("set.held_updates").into(),
				self.held_updates(cx),
				cx,
			))
			.child(field(
				t!("set.notifications").into(),
				Checkbox::new("notifications")
					.label(t!("set.notify").into_owned())
					.checked(self.draft.notifications)
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.draft.notifications = *checked;
						cx.notify();
					})),
				cx,
			))
			.child(field(
				t!("set.app_updates").into(),
				Checkbox::new("check-app-updates")
					.label(t!("set.check_app_updates").into_owned())
					.checked(self.draft.check_app_updates)
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.draft.check_app_updates = *checked;
						cx.notify();
					})),
				cx,
			))
			.when(cfg!(target_os = "linux"), |this| {
				this.child(field(
					t!("set.desktop").into(),
					v_flex()
						.gap_1()
						.child(self.desktop_buttons(cx))
						.child(hint(t!("set.desktop_hint").into(), cx)),
					cx,
				))
			})
			.child(field(
				t!("set.parallel").into(),
				h_flex()
					.gap_1()
					.children((1..=MAX_CONCURRENT_DOWNLOADS).map(|n| {
						Button::new(("parallel", n as usize))
							.label(n.to_string())
							.small()
							.outline()
							.selected(parallel == n)
							.on_click(cx.listener(move |this, _, _, cx| {
								this.draft.max_concurrent_downloads = n;
								cx.notify();
							}))
					})),
				cx,
			))
			.child(field(
				t!("set.speed_limit").into(),
				h_flex()
					.gap_2()
					.child(div().w(px(120.)).child(Input::new(&self.speed).small()))
					.child(
						h_flex()
							.gap_1()
							.children([SpeedUnit::KB, SpeedUnit::MB].map(|value| {
								Button::new(value.label())
									.label(value.label())
									.small()
									.outline()
									.selected(unit == value)
									.on_click(cx.listener(move |this, _, _, cx| {
										this.draft.download_limit.unit = value;
										cx.notify();
									}))
							})),
					)
					.child(
						div()
							.text_sm()
							.text_color(cx.theme().muted_foreground)
							.child(
								if unlimited {
									t!("set.unlimited")
								} else {
									t!("set.all_downloads")
								}
								.into_owned(),
							),
					),
				cx,
			))
	}

	/// Packages whose updates aren't offered; removing one saves at once.
	fn held_updates(&self, cx: &mut Context<Self>) -> AnyElement {
		let packages = &self.saved.update_ignored;
		if packages.is_empty() {
			return hint(t!("set.held_none").into(), cx).into_any_element();
		}
		let theme = cx.theme().clone();
		let header = column_header(cx)
			.px_3()
			.child(
				div()
					.w(px(200.))
					.flex_shrink_0()
					.child(t!("lib.col_game").into_owned()),
			)
			.child(div().flex_1().child(t!("lib.package").into_owned()));
		let rows = packages.iter().enumerate().map(|(ix, package)| {
			let remove = package.clone();
			let name = match self.game_names.get(package) {
				Some(name) => div().child(name.clone()),
				None => div().text_color(theme.muted_foreground).child("-"),
			};
			table_row(("held-row", ix), 32., &theme)
				.px_3()
				.text_sm()
				.when(ix + 1 == packages.len(), |row| row.border_b_0())
				.child(name.w(px(200.)).flex_shrink_0().truncate())
				.child(
					div()
						.flex_1()
						.min_w_0()
						.truncate()
						.mono(cx)
						.text_color(theme.muted_foreground)
						.child(package.clone()),
				)
				.child(
					Button::new(("unhold", ix))
						.icon(Icon::new(IconName::Delete))
						.xsmall()
						.ghost()
						.tooltip(t!("set.held_remove"))
						.on_click(cx.listener(move |this, _, _, cx| {
							this.save_now(|s| s.update_ignored.retain(|p| p != &remove));
							cx.notify();
						})),
				)
		});
		v_flex()
			.gap_2()
			.child(hint(t!("set.held_intro").into(), cx))
			.child(
				v_flex()
					.border_1()
					.border_color(theme.border)
					.rounded(px(6.))
					.overflow_hidden()
					.child(header)
					.children(rows),
			)
			.into_any_element()
	}

	fn mirrors(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let active = self.draft.active_mirror.clone();
		let rows = self.draft.mirrors.iter().enumerate().map(|(ix, m)| {
			let is_active = active.as_deref() == Some(m.id.as_str());
			let (pick, remove) = (m.id.clone(), m.id.clone());
			h_flex()
				.id(("mirror", ix))
				.gap_3()
				.px_3()
				.py_2()
				.rounded(theme.radius)
				.border_1()
				.border_color(if is_active {
					theme.list_active_border
				} else {
					theme.border
				})
				.when(is_active, |r| r.bg(theme.list_active))
				.cursor_pointer()
				.on_click(cx.listener(move |this, _, _, cx| {
					this.draft.active_mirror = Some(pick.clone());
					cx.notify();
				}))
				.child(
					div()
						.size_3()
						.flex_shrink_0()
						.rounded_full()
						.border_1()
						.border_color(theme.primary)
						.when(is_active, |d| d.bg(theme.primary)),
				)
				.child(div().child(m.name.clone()))
				.child(
					div()
						.flex_1()
						.min_w_0()
						.truncate()
						.text_sm()
						.mono(cx)
						.text_color(theme.muted_foreground)
						.child(mirror::describe(m)),
				)
				.child(
					Button::new(("remove-mirror", ix))
						.icon(Icon::new(IconName::Delete))
						.ghost()
						.xsmall()
						.tooltip(t!("mir.remove"))
						.on_click(cx.listener(move |this, _, _, cx| {
							this.draft.mirrors.retain(|m| m.id != remove);
							this.draft.normalize();
							cx.notify();
						})),
				)
		});
		let view = cx.entity().downgrade();
		let rclone_dir = self.rclone_dir.clone();
		v_flex()
			.gap_3()
			.child(hint(t!("mir.intro").into(), cx))
			.when(self.draft.mirrors.is_empty(), |d| {
				d.child(hint(t!("mir.none").into(), cx))
			})
			.children(rows)
			.child(
				h_flex().child(
					Button::new("add-mirror")
						.label(t!("mir.add_mirror"))
						.icon(Icon::new(IconName::Plus))
						.small()
						.outline()
						.on_click(move |_, window, cx| {
							let view = view.clone();
							MirrorForm::open(
								rclone_dir.clone(),
								move |mirror, _, cx| {
									let _ = view.update(cx, |this, cx| {
										let id = mirror.id.clone();
										this.draft.mirrors.push(mirror);
										if this.draft.active_mirror.is_none() {
											this.draft.active_mirror = Some(id);
										}
										cx.notify();
									});
								},
								window,
								cx,
							);
						}),
				),
			)
	}

	fn tools(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let binary = |id: &'static str, input: &Entity<InputState>, cx: &mut Context<Self>| {
			let (browse, clear) = (input.clone(), input.clone());
			h_flex()
				.gap_2()
				.child(div().flex_1().child(Input::new(input).small()))
				.child(
					Button::new(SharedString::from(format!("{id}-browse")))
						.label(t!("common.browse"))
						.small()
						.outline()
						.on_click(cx.listener(move |this, _, window, cx| {
							this.pick_path(browse.clone(), false, window, cx)
						})),
				)
				.child(
					Button::new(SharedString::from(format!("{id}-path")))
						.label(t!("set.use_path"))
						.small()
						.outline()
						.on_click(cx.listener(move |_, _, window, cx| {
							clear.update(cx, |input, cx| input.set_value("", window, cx));
						})),
				)
		};
		v_flex()
			.gap_4()
			.child(field(
				t!("set.adb_binary").into(),
				binary("adb", &self.adb_path, cx),
				cx,
			))
			.child(field(
				t!("set.rclone_binary").into(),
				binary("rclone", &self.rclone_path, cx),
				cx,
			))
			.child(field(
				t!("chk.button").into(),
				v_flex()
					.gap_1()
					.items_start()
					.child(
						Button::new("checkup")
							.label(t!("chk.button"))
							.small()
							.outline()
							.on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Checkup))),
					)
					.child(hint(t!("chk.hint").into(), cx)),
				cx,
			))
	}

	fn keys(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let rows = self.key_rows.get_or_insert_with(|| {
			self.keys
				.packages()
				.into_iter()
				.map(|package| {
					let fingerprint = match self.keys.load(&package) {
						Ok(Some(key)) => Ok(key.fingerprint()),
						Ok(None) => Err(String::new()),
						Err(e) => Err(e.to_string()),
					};
					KeyRow {
						package,
						fingerprint,
					}
				})
				.collect()
		});
		let list = rows.iter().enumerate().map(|(ix, row)| {
			let package = row.package.clone();
			let fingerprint = match &row.fingerprint {
				Ok(f) => div()
					.text_color(theme.muted_foreground)
					.child(format!("SHA-256 {}...", &f[..16.min(f.len())])),
				Err(e) => div().text_color(theme.danger).child(e.clone()),
			};
			h_flex()
				.gap_3()
				.py_1()
				.text_sm()
				.mono(cx)
				.child(
					div()
						.flex_1()
						.min_w_0()
						.truncate()
						.child(row.package.clone()),
				)
				.child(fingerprint)
				.child({
					let package = package.clone();
					Button::new(("export", ix))
						.label(t!("keys.export"))
						.xsmall()
						.outline()
						.on_click(cx.listener(move |this, _, window, cx| {
							this.export_key(package.clone(), window, cx)
						}))
				})
				.child(
					Button::new(("delete-key", ix))
						.icon(Icon::new(IconName::Delete))
						.xsmall()
						.ghost()
						.tooltip(t!("keys.delete"))
						.on_click(cx.listener(move |this, _, window, cx| {
							this.delete_key(package.clone(), window, cx)
						})),
				)
		});
		let empty = rows.is_empty();
		v_flex()
			.gap_3()
			.child(hint(t!("keys.intro").into(), cx))
			.when(empty, |d| d.child(hint(t!("keys.none").into(), cx)))
			.children(list)
			.child(
				h_flex().child(
					Button::new("import-key")
						.label(t!("keys.import"))
						.small()
						.outline()
						.on_click(cx.listener(|this, _, window, cx| this.import_key(window, cx))),
				),
			)
			.children(self.key_message.clone().map(|message| match message {
				Ok(text) => div().text_sm().child(text),
				Err(text) => div().text_sm().text_color(theme.danger).child(text),
			}))
	}

	fn import_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: true,
			directories: false,
			multiple: false,
			prompt: None,
		});
		cx.spawn_in(window, async move |this, cx| {
			let Ok(Ok(Some(paths))) = paths.await else {
				return;
			};
			let Some(file) = paths.into_iter().next() else {
				return;
			};
			let _ = this.update(cx, |this, cx| {
				this.key_message = Some(match this.keys.import(&file) {
					Ok(package) => {
						this.key_rows = None;
						cx.emit(SettingsEvent::KeysChanged);
						Ok(t!("keys.imported", package = package).into_owned())
					}
					Err(e) => Err(e.to_string()),
				});
				cx.notify();
			});
		})
		.detach();
	}

	fn delete_key(&mut self, package: String, window: &mut Window, cx: &mut Context<Self>) {
		let this = cx.entity().downgrade();
		window.open_alert_dialog(cx, move |alert, _, _| {
			let this = this.clone();
			let package = package.clone();
			alert
				.title(t!("keys.delete_title", package = package).into_owned())
				.description(t!("keys.delete_text").into_owned())
				.confirm()
				.ok_text(t!("common.delete").into_owned())
				.cancel_text(t!("common.cancel").into_owned())
				.ok_variant(ButtonVariant::Danger)
				.on_ok(move |_, _, cx| {
					let _ = this.update(cx, |this, cx| {
						this.key_message = Some(match this.keys.remove(&package) {
							Ok(()) => Ok(t!("keys.deleted", package = package).into_owned()),
							Err(e) => Err(e.to_string()),
						});
						this.key_rows = None;
						cx.emit(SettingsEvent::KeysChanged);
						cx.notify();
					});
					true
				})
		});
	}

	fn export_key(&mut self, package: String, window: &mut Window, cx: &mut Context<Self>) {
		let home = std::env::home_dir().unwrap_or_default();
		let dest = cx.prompt_for_new_path(&home, Some(&format!("{package}.pem")));
		cx.spawn_in(window, async move |this, cx| {
			let Ok(Ok(Some(dest))) = dest.await else {
				return;
			};
			let _ = this.update(cx, |this, cx| {
				this.key_message = Some(match this.keys.export(&package, &dest) {
					Ok(()) => Ok(t!("keys.exported", path = dest.display()).into_owned()),
					Err(e) => Err(e.to_string()),
				});
				cx.notify();
			});
		})
		.detach();
	}

	fn save_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let can_save = !self.saving && !self.draft.downloads_dir.as_os_str().is_empty();
		h_flex()
			.px_6()
			.py_3()
			.gap_3()
			.border_t_1()
			.border_color(theme.border)
			.bg(theme.muted)
			.child(
				div()
					.flex_1()
					.text_color(theme.muted_foreground)
					.child(t!("set.unsaved").into_owned()),
			)
			.child(
				Button::new("revert")
					.label(t!("set.revert"))
					.small()
					.outline()
					.disabled(self.saving)
					.on_click(cx.listener(|this, _, window, cx| this.revert(window, cx))),
			)
			.child(
				Button::new("save")
					.label(t!("set.save"))
					.small()
					.primary()
					.loading(self.saving)
					.disabled(!can_save)
					.on_click(cx.listener(|this, _, _, cx| this.save(cx))),
			)
	}
}

impl Render for SettingsView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let nav = v_flex()
			.w(px(220.))
			.flex_shrink_0()
			.h_full()
			.p_3()
			.gap_1()
			.border_r_1()
			.border_color(theme.border)
			.children(Category::ALL.into_iter().enumerate().map(|(ix, category)| {
				Button::new(("category", ix))
					.icon(category.icon())
					.label(category.label())
					.ghost()
					.w_full()
					.justify_start()
					.selected(self.category == category)
					.on_click(cx.listener(move |this, _, _, cx| {
						this.category = category;
						cx.notify();
					}))
			}));
		let body = match self.category {
			Category::Appearance => self.appearance(cx).into_any_element(),
			Category::Downloads => self.downloads(cx).into_any_element(),
			Category::Mirrors => self.mirrors(cx).into_any_element(),
			Category::Tools => self.tools(cx).into_any_element(),
			Category::Keys => self.keys(cx).into_any_element(),
		};
		h_flex().size_full().child(nav).child(
			v_flex()
				.flex_1()
				.min_w_0()
				.h_full()
				.child(
					div()
						.id("settings-body")
						.flex_1()
						.overflow_y_scroll()
						.child(
							v_flex()
								.max_w(px(820.))
								.px_6()
								.py_5()
								.gap_5()
								.child(div().text_xl().child(self.category.label()))
								.child(body),
						),
				)
				.when(self.is_dirty() || self.saving, |d| {
					d.child(self.save_bar(cx))
				}),
		)
	}
}

fn optional_path(text: &str) -> Option<PathBuf> {
	let trimmed = text.trim();
	(!trimmed.is_empty()).then(|| trimmed.into())
}

fn field(label: String, control: impl IntoElement, cx: &App) -> impl IntoElement {
	h_flex()
		.gap_4()
		.items_start()
		.child(
			div()
				.w(px(200.))
				.pt_1()
				.flex_shrink_0()
				.text_color(cx.theme().muted_foreground)
				.child(label),
		)
		.child(div().flex_1().min_w_0().child(control))
}

fn hint(text: String, cx: &App) -> impl IntoElement {
	div()
		.text_sm()
		.text_color(cx.theme().muted_foreground)
		.child(text)
}
