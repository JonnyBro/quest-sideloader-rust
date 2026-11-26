use rust_i18n::t;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Selectable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::catalog::{Mirror, MirrorKind, rclone_remotes};

use crate::theme::Mono;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
	VrpJson,
	Http,
	Rclone,
}

type OnAdd = Rc<dyn Fn(Mirror, &mut Window, &mut App)>;

/// `on_add` gets the mirror; it goes into the settings draft like any other
/// change.
pub struct MirrorForm {
	kind: Kind,
	rclone_dir: PathBuf,
	name: Entity<InputState>,
	json: Entity<TextareaState>,
	base_uri: Entity<InputState>,
	password: Entity<InputState>,
	rclone_file: Option<PathBuf>,
	remotes: Vec<String>,
	remote: String,
	error: Option<String>,
	on_add: OnAdd,
}

impl MirrorForm {
	pub fn open(
		rclone_dir: PathBuf,
		on_add: impl Fn(Mirror, &mut Window, &mut App) + 'static,
		window: &mut Window,
		cx: &mut App,
	) {
		let form = cx.new(|cx| Self {
			kind: Kind::VrpJson,
			rclone_dir,
			name: cx.new(|cx| InputState::new(window, cx).placeholder(t!("mir.default_name"))),
			json: cx.new(|cx| {
				TextareaState::new(window, cx)
					.rows(4)
					.placeholder(r#"{"baseUri":"https://.../","password":"..."}"#)
			}),
			base_uri: cx
				.new(|cx| InputState::new(window, cx).placeholder("https://example.org/files/")),
			password: cx.new(|cx| InputState::new(window, cx).masked(true)),
			rclone_file: None,
			remotes: Vec::new(),
			remote: String::new(),
			error: None,
			on_add: Rc::new(on_add),
		});
		window.open_dialog(cx, move |dialog, _, _| {
			dialog
				.title(t!("mir.add_mirror").trim_end_matches('.').to_owned())
				.w(px(620.))
				.child(form.clone())
		});
	}

	fn name_or(&self, fallback: &str, cx: &App) -> String {
		let name = self.name.read(cx).value().trim().to_owned();
		if name.is_empty() {
			fallback.to_owned()
		} else {
			name
		}
	}

	fn build(&self, cx: &App) -> Result<Mirror, String> {
		let password = self.password.read(cx).value().trim().to_owned();
		let mirror = match self.kind {
			Kind::VrpJson => Mirror::from_vrp_json(
				&self.name_or(&t!("mir.default_name"), cx),
				&self.json.read(cx).value(),
			)
			.map_err(|e| e.to_string())?,
			Kind::Http => Mirror {
				id: Mirror::new_id(),
				name: self.name_or(&t!("mir.default_name"), cx),
				kind: MirrorKind::Http {
					base_uri: self.base_uri.read(cx).value().trim().to_owned(),
				},
				password,
			},
			Kind::Rclone => {
				let source = self
					.rclone_file
					.as_ref()
					.ok_or_else(|| t!("mir.choose_config").into_owned())?;
				let id = Mirror::new_id();
				let mirror = Mirror {
					name: self.name_or(&self.remote, cx),
					kind: MirrorKind::Rclone {
						config_file: self.rclone_dir.join(format!("{id}.conf")),
						remote: self.remote.clone(),
					},
					id,
					password,
				};
				mirror.validate().map_err(|e| e.to_string())?;
				if let MirrorKind::Rclone { config_file, .. } = &mirror.kind {
					copy_private(source, config_file)
						.map_err(|e| t!("mir.copy_failed", error = e).into_owned())?;
				}
				return Ok(mirror);
			}
		};
		mirror.validate().map_err(|e| e.to_string())?;
		Ok(mirror)
	}

	fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		match self.build(cx) {
			Ok(mirror) => {
				window.close_dialog(cx);
				(self.on_add)(mirror, window, cx);
			}
			Err(e) => {
				self.error = Some(e);
				cx.notify();
			}
		}
	}

	fn pick_file(&mut self, json: bool, window: &mut Window, cx: &mut Context<Self>) {
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
			let Some(path) = paths.into_iter().next() else {
				return;
			};
			let _ = this.update_in(cx, |this, window, cx| {
				if json {
					this.load_json(path, window, cx);
				} else {
					this.load_rclone_file(path);
				}
				cx.notify();
			});
		})
		.detach();
	}

	fn load_json(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
		match std::fs::read_to_string(&path) {
			Ok(text) => self
				.json
				.update(cx, |input, cx| input.set_value(text, window, cx)),
			Err(e) => {
				self.error =
					Some(t!("mir.cannot_read", path = path.display(), error = e).into_owned())
			}
		}
	}

	fn load_rclone_file(&mut self, path: PathBuf) {
		match std::fs::read_to_string(&path) {
			Ok(text) => {
				self.remotes = rclone_remotes(&text);
				self.remote = self.remotes.first().cloned().unwrap_or_default();
				self.error = self
					.remotes
					.is_empty()
					.then(|| t!("mir.no_remotes").into_owned());
				self.rclone_file = Some(path);
			}
			Err(e) => {
				self.error =
					Some(t!("mir.cannot_read", path = path.display(), error = e).into_owned())
			}
		}
	}
}

impl Render for MirrorForm {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let kind_button = |id: &'static str, kind: Kind, label: String| {
			Button::new(id)
				.label(label)
				.small()
				.outline()
				.selected(self.kind == kind)
				.on_click(cx.listener(move |this, _, _, cx| {
					this.kind = kind;
					this.error = None;
					cx.notify();
				}))
		};
		let row = |label: String, control: AnyElement| {
			h_flex()
				.gap_3()
				.items_start()
				.child(
					div()
						.w(px(120.))
						.pt_1()
						.flex_shrink_0()
						.text_color(theme.muted_foreground)
						.child(label),
				)
				.child(div().flex_1().min_w_0().child(control))
		};
		let mut fields = v_flex().gap_3().child(row(
			t!("mir.name").into(),
			Input::new(&self.name).small().into_any_element(),
		));
		match self.kind {
			Kind::VrpJson => {
				fields = fields.child(row(
					t!("mir.config").into(),
					v_flex()
						.gap_2()
						.child(Textarea::new(&self.json).mono(cx))
						.child(
							Button::new("load-json")
								.label(t!("mir.load_file"))
								.small()
								.outline()
								.on_click(cx.listener(|this, _, window, cx| {
									this.pick_file(true, window, cx)
								})),
						)
						.into_any_element(),
				));
			}
			Kind::Http => {
				fields = fields
					.child(row(
						t!("mir.base_uri").into(),
						Input::new(&self.base_uri).small().into_any_element(),
					))
					.child(row(
						t!("mir.password").into(),
						v_flex()
							.gap_1()
							.child(Input::new(&self.password).small().mask_toggle())
							.child(
								div()
									.text_sm()
									.text_color(theme.muted_foreground)
									.child(t!("mir.password_hint").into_owned()),
							)
							.into_any_element(),
					));
			}
			Kind::Rclone => {
				let file = self.rclone_file.as_ref().map_or_else(
					|| t!("mir.none_selected").into_owned(),
					|p| p.display().to_string(),
				);
				fields = fields.child(row(
					t!("mir.config_file").into(),
					h_flex()
						.gap_2()
						.child(
							div()
								.flex_1()
								.min_w_0()
								.truncate()
								.text_sm()
								.mono(cx)
								.text_color(theme.muted_foreground)
								.child(file),
						)
						.child(
							Button::new("choose-rclone")
								.label(t!("mir.choose"))
								.small()
								.outline()
								.on_click(cx.listener(|this, _, window, cx| {
									this.pick_file(false, window, cx)
								})),
						)
						.into_any_element(),
				));
				if !self.remotes.is_empty() {
					let remotes = self.remotes.clone();
					let current = self.remote.clone();
					let form = cx.entity().downgrade();
					fields = fields.child(row(
						t!("mir.remote").into(),
						Button::new("remote")
							.label(self.remote.clone())
							.small()
							.outline()
							.dropdown_caret(true)
							.dropdown_menu(move |mut menu: PopupMenu, _, _| {
								for remote in &remotes {
									let form = form.clone();
									let value = remote.clone();
									menu = menu.item(
										PopupMenuItem::new(remote.clone())
											.checked(*remote == current)
											.on_click(move |_, _, cx| {
												let _ = form.update(cx, |this, cx| {
													this.remote = value.clone();
													cx.notify();
												});
											}),
									);
								}
								menu
							})
							.into_any_element(),
					));
				}
				fields = fields.child(row(
					t!("mir.password").into(),
					v_flex()
						.gap_1()
						.child(Input::new(&self.password).small().mask_toggle())
						.child(
							div()
								.text_sm()
								.text_color(theme.muted_foreground)
								.child(t!("mir.archive_password_hint").into_owned()),
						)
						.into_any_element(),
				));
			}
		}
		v_flex()
			.gap_4()
			.child(
				h_flex()
					.gap_1()
					.child(kind_button(
						"kind-json",
						Kind::VrpJson,
						t!("mir.kind_json").into(),
					))
					.child(kind_button("kind-http", Kind::Http, "HTTP".into()))
					.child(kind_button(
						"kind-rclone",
						Kind::Rclone,
						t!("mir.kind_rclone").into(),
					)),
			)
			.child(fields)
			.children(
				self.error
					.clone()
					.map(|e| div().text_sm().text_color(theme.danger).child(e)),
			)
			.child(
				h_flex()
					.justify_end()
					.gap_2()
					.child(
						Button::new("cancel")
							.label(t!("common.cancel"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("add")
							.label(t!("mir.add"))
							.primary()
							.on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
					),
			)
	}
}

/// The copy holds credentials, so keep it readable by this user only.
fn copy_private(from: &Path, to: &Path) -> std::io::Result<()> {
	if let Some(parent) = to.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let bytes = std::fs::read(from)?;
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	std::io::Write::write_all(&mut options.open(to)?, &bytes)
}

pub fn remove_unused_rclone_configs(mirrors: &[Mirror], rclone_dir: &Path) {
	let Ok(entries) = std::fs::read_dir(rclone_dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let used = mirrors.iter().any(
			|m| matches!(&m.kind, MirrorKind::Rclone { config_file, .. } if *config_file == path),
		);
		if !used && let Err(e) = std::fs::remove_file(&path) {
			tracing::warn!("could not remove {}: {e}", path.display());
		}
	}
}

/// "HTTP: host" or "rclone: remote", without credentials from the URL.
pub fn describe(mirror: &Mirror) -> String {
	match &mirror.kind {
		MirrorKind::Http { base_uri } => {
			let host = url_host(base_uri).unwrap_or_else(|| base_uri.clone());
			format!("HTTP: {host}")
		}
		MirrorKind::Rclone { remote, .. } => format!("rclone: {remote}"),
	}
}

fn url_host(uri: &str) -> Option<String> {
	let rest = uri.split_once("://")?.1;
	let host = rest.split(['/', '?']).next()?;
	Some(host.rsplit('@').next().unwrap_or(host).to_owned())
}

#[cfg(test)]
mod tests {
	use super::*;
	// The gpui glob import brings its own `test` attribute.
	use ::core::prelude::v1::test;

	#[test]
	fn hosts_hide_credentials() {
		assert_eq!(
			url_host("https://user:pw@example.org/a/b").as_deref(),
			Some("example.org")
		);
		assert_eq!(
			url_host("http://10.0.0.2:8080").as_deref(),
			Some("10.0.0.2:8080")
		);
		assert_eq!(url_host("nonsense"), None);
	}
}
