use rust_i18n::t;
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::adb::RemoteEntry;
use quest_sideloader_core::adb::browse::{STORAGE_ROOT, join, valid_name};
use quest_sideloader_core::{Command, Engine, format_bytes};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::widgets::{
	Confirm, centered, column_header, confirm, format_date, prompt_name, table_row,
};

const ROW_HEIGHT: f32 = 36.;
const SIZE_W: f32 = 96.;
const DATE_W: f32 = 150.;

pub struct FilesView {
	engine: Rc<Engine>,
	serial: Option<String>,
	path: String,
	/// `None` while the folder loads.
	entries: Option<Result<Vec<RemoteEntry>, String>>,
	requested: Option<(String, String)>,
	selected: HashSet<String>,
	clipboard: Option<Clipboard>,
}

struct Clipboard {
	paths: Vec<String>,
	copy: bool,
}

impl FilesView {
	pub fn new(engine: Rc<Engine>) -> Self {
		Self {
			engine,
			serial: None,
			path: STORAGE_ROOT.to_owned(),
			entries: None,
			requested: None,
			selected: HashSet::new(),
			clipboard: None,
		}
	}

	pub fn set_serial(&mut self, serial: Option<String>, cx: &mut Context<Self>) {
		if self.serial == serial {
			return;
		}
		self.serial = serial;
		self.path = STORAGE_ROOT.to_owned();
		self.entries = None;
		self.requested = None;
		self.selected.clear();
		self.clipboard = None;
		cx.notify();
	}

	pub fn ensure_loaded(&mut self) {
		let current = self.serial.clone().map(|s| (s, self.path.clone()));
		if self.entries.is_none() && self.requested != current {
			self.load();
		}
	}

	pub fn set_listing(
		&mut self,
		serial: &str,
		path: &str,
		result: Result<Vec<RemoteEntry>, String>,
		cx: &mut Context<Self>,
	) {
		if self.serial.as_deref() != Some(serial) || self.path != path {
			return;
		}
		if let Ok(entries) = &result {
			self.selected
				.retain(|name| entries.iter().any(|e| &e.name == name));
		}
		self.entries = Some(result);
		cx.notify();
	}

	fn load(&mut self) {
		if let Some(serial) = &self.serial {
			self.requested = Some((serial.clone(), self.path.clone()));
			self.engine.send(Command::ListDir {
				serial: serial.clone(),
				path: self.path.clone(),
			});
		}
	}

	fn open(&mut self, path: String, cx: &mut Context<Self>) {
		self.path = path;
		self.entries = None;
		self.selected.clear();
		self.load();
		cx.notify();
	}

	fn listed(&self) -> &[RemoteEntry] {
		match &self.entries {
			Some(Ok(entries)) => entries,
			_ => &[],
		}
	}

	fn selected_paths(&self) -> Vec<String> {
		self.listed()
			.iter()
			.filter(|e| self.selected.contains(&e.name))
			.map(|e| join(&self.path, &e.name))
			.collect()
	}

	fn pull(&self, paths: Vec<String>, cx: &mut Context<Self>) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		if paths.is_empty() {
			return;
		}
		let dest = cx.prompt_for_paths(PathPromptOptions {
			files: false,
			directories: true,
			multiple: false,
			prompt: None,
		});
		let engine = self.engine.clone();
		cx.spawn(async move |_, _| {
			if let Ok(Ok(Some(mut dirs))) = dest.await
				&& let Some(dest) = dirs.pop()
			{
				engine.send(Command::PullFiles {
					serial,
					paths,
					dest,
				});
			}
		})
		.detach();
	}

	fn pick_and_push(&self, directories: bool, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: !directories,
			directories,
			multiple: true,
			prompt: None,
		});
		let this = cx.entity().downgrade();
		cx.spawn(async move |_, cx| {
			if let Ok(Ok(Some(paths))) = paths.await {
				let _ = this.update(cx, |this, _| this.push(paths));
			}
		})
		.detach();
	}

	fn push(&self, local: Vec<PathBuf>) {
		if let Some(serial) = self.serial.clone()
			&& !local.is_empty()
		{
			self.engine.send(Command::PushFiles {
				serial,
				local,
				dir: self.path.clone(),
			});
		}
	}

	fn delete(&self, window: &mut Window, cx: &mut App) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		let paths = self.selected_paths();
		let text = match paths.as_slice() {
			[] => return,
			[one] => t!("files.delete_one", name = one).into_owned(),
			_ => t!("files.delete_many", n = paths.len()).into_owned(),
		};
		confirm(
			&self.engine,
			Confirm {
				title: t!("files.delete_title").into_owned(),
				text,
				ok: t!("common.delete").into_owned(),
				danger: true,
			},
			Command::DeleteFiles {
				serial,
				paths,
				dir: self.path.clone(),
			},
			window,
			cx,
		);
	}

	fn pick_up(&mut self, copy: bool, cx: &mut Context<Self>) {
		let paths = self.selected_paths();
		if !paths.is_empty() {
			self.clipboard = Some(Clipboard { paths, copy });
			self.selected.clear();
			cx.notify();
		}
	}

	fn paste(&mut self, cx: &mut Context<Self>) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		let Some(clipboard) = self.clipboard.take() else {
			return;
		};
		self.engine.send(Command::PasteFiles {
			serial,
			paths: clipboard.paths.clone(),
			dir: self.path.clone(),
			copy: clipboard.copy,
		});
		// A copy can be pasted again elsewhere; moved files are gone from their place.
		if clipboard.copy {
			self.clipboard = Some(clipboard);
		}
		cx.notify();
	}

	fn new_folder(&self, window: &mut Window, cx: &mut App) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		let (engine, dir) = (self.engine.clone(), self.path.clone());
		prompt_name(
			t!("files.new_folder").into_owned(),
			"",
			t!("files.create").into_owned(),
			valid_name,
			move |name, _| {
				engine.send(Command::MakeDir {
					serial: serial.clone(),
					dir: dir.clone(),
					name,
				})
			},
			window,
			cx,
		);
	}

	fn rename(&self, window: &mut Window, cx: &mut App) {
		let Some(serial) = self.serial.clone() else {
			return;
		};
		if self.selected.len() != 1 {
			return;
		}
		let Some(from) = self.selected.iter().next().cloned() else {
			return;
		};
		let (engine, dir) = (self.engine.clone(), self.path.clone());
		let initial = from.clone();
		prompt_name(
			t!("files.rename_title", name = from).into_owned(),
			&initial,
			t!("files.rename").into_owned(),
			valid_name,
			move |to, _| {
				if to != from {
					engine.send(Command::RenameFile {
						serial: serial.clone(),
						dir: dir.clone(),
						from: from.clone(),
						to,
					});
				}
			},
			window,
			cx,
		);
	}

	fn breadcrumbs(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let relative = self.path.strip_prefix(STORAGE_ROOT).unwrap_or_default();
		let mut crumbs = vec![(STORAGE_ROOT.to_owned(), STORAGE_ROOT.to_owned())];
		let mut current = STORAGE_ROOT.to_owned();
		for part in relative.split('/').filter(|p| !p.is_empty()) {
			current = join(&current, part);
			crumbs.push((part.to_owned(), current.clone()));
		}
		let last = crumbs.len() - 1;
		let separator = theme.muted_foreground.opacity(0.5);
		h_flex()
			.flex_1()
			.min_w_0()
			.gap_1()
			.overflow_hidden()
			.mono(cx)
			.text_sm()
			.children(crumbs.into_iter().enumerate().map(|(ix, (label, path))| {
				h_flex()
					.gap_1()
					.flex_shrink_0()
					.when(ix > 0, |d| d.child(div().text_color(separator).child("/")))
					.child(
						Button::new(("crumb", ix))
							.label(label)
							.ghost()
							.xsmall()
							.disabled(ix == last)
							.on_click(
								cx.listener(move |this, _, _, cx| this.open(path.clone(), cx)),
							),
					)
			}))
	}

	fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let parent = (self.path != STORAGE_ROOT)
			.then(|| self.path.rsplit_once('/').map(|(p, _)| p.to_owned()))
			.flatten();
		let has_selection = !self.selected.is_empty();
		h_flex()
			.gap_2()
			.px_5()
			.py_3()
			.child(
				Button::new("up")
					.icon(Icon::new(IconName::ArrowUp))
					.ghost()
					.small()
					.tooltip(t!("files.up"))
					.disabled(parent.is_none())
					.on_click(cx.listener(move |this, _, _, cx| {
						if let Some(parent) = parent.clone() {
							this.open(parent, cx);
						}
					})),
			)
			.child(
				Button::new("reload")
					.icon(Icon::from(AppIcon::Refresh))
					.ghost()
					.small()
					.tooltip(t!("files.reload"))
					.loading(self.entries.is_none())
					.on_click(cx.listener(|this, _, _, cx| {
						let path = this.path.clone();
						this.open(path, cx);
					})),
			)
			.child(self.breadcrumbs(cx))
			.child(
				Button::new("copy")
					.icon(Icon::new(IconName::Copy))
					.ghost()
					.small()
					.tooltip(t!("files.copy_hint"))
					.disabled(!has_selection)
					.on_click(cx.listener(|this, _, _, cx| this.pick_up(true, cx))),
			)
			.child(
				Button::new("cut")
					.icon(Icon::from(AppIcon::Cut))
					.ghost()
					.small()
					.tooltip(t!("files.cut_hint"))
					.disabled(!has_selection)
					.on_click(cx.listener(|this, _, _, cx| this.pick_up(false, cx))),
			)
			.children(self.clipboard.as_ref().map(|clipboard| {
				let label = format!("{} ({})", t!("files.paste"), clipboard.paths.len());
				let tip = if clipboard.copy {
					t!("files.paste_copy_hint")
				} else {
					t!("files.paste_move_hint")
				};
				h_flex()
					.child(
						Button::new("paste")
							.icon(Icon::from(AppIcon::Paste))
							.label(label)
							.outline()
							.small()
							.tooltip(tip)
							.on_click(cx.listener(|this, _, _, cx| this.paste(cx))),
					)
					.child(
						Button::new("clear-clipboard")
							.icon(Icon::new(IconName::Close))
							.ghost()
							.xsmall()
							.tooltip(t!("files.clear_clipboard"))
							.on_click(cx.listener(|this, _, _, cx| {
								this.clipboard = None;
								cx.notify();
							})),
					)
			}))
			.child(
				Button::new("rename")
					.label(t!("files.rename"))
					.outline()
					.small()
					.disabled(self.selected.len() != 1)
					.on_click(cx.listener(|this, _, window, cx| this.rename(window, cx))),
			)
			.child(
				Button::new("delete")
					.icon(Icon::new(IconName::Delete))
					.label(t!("common.delete"))
					.outline()
					.small()
					.disabled(!has_selection)
					.on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
			)
			.child(
				Button::new("pull")
					.icon(Icon::from(AppIcon::Downloads))
					.label(t!("files.pull"))
					.outline()
					.small()
					.disabled(!has_selection)
					.tooltip(t!("files.pull_hint"))
					.on_click(cx.listener(|this, _, _, cx| {
						let paths = this.selected_paths();
						this.pull(paths, cx);
					})),
			)
			.child(
				Button::new("new-folder")
					.icon(Icon::new(IconName::FolderOpen))
					.label(t!("files.new_folder"))
					.outline()
					.small()
					.on_click(cx.listener(|this, _, window, cx| this.new_folder(window, cx))),
			)
			.child(
				Button::new("push-folder")
					.label(t!("files.push_folder"))
					.outline()
					.small()
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_push(true, cx))),
			)
			.child(
				Button::new("push-files")
					.icon(Icon::from(AppIcon::Upload))
					.label(t!("files.push_files"))
					.primary()
					.small()
					.tooltip(t!("files.push_hint"))
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_push(false, cx))),
			)
	}

	fn header_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let entries = self.listed();
		let all = !entries.is_empty() && self.selected.len() == entries.len();
		column_header(cx)
			.child(
				Checkbox::new("select-all")
					.checked(all)
					.disabled(entries.is_empty())
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.selected = if *checked {
							this.listed().iter().map(|e| e.name.clone()).collect()
						} else {
							HashSet::new()
						};
						cx.notify();
					})),
			)
			.child(
				div()
					.flex_1()
					.min_w_0()
					.child(t!("hs.col_name").into_owned()),
			)
			.child(
				div()
					.w(px(DATE_W))
					.flex_shrink_0()
					.child(t!("files.col_modified").into_owned()),
			)
			.child(
				div()
					.w(px(SIZE_W))
					.flex_shrink_0()
					.text_right()
					.child(t!("hs.col_size").into_owned()),
			)
	}

	fn render_rows(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let theme = cx.theme().clone();
		let entries = self.listed();
		range
			.filter_map(|ix| entries.get(ix).map(|e| (ix, e)))
			.map(|(ix, entry)| {
				let name = entry.name.clone();
				let path = join(&self.path, &entry.name);
				let is_dir = entry.is_dir;
				let icon = if is_dir {
					Icon::new(IconName::Folder).text_color(theme.primary)
				} else {
					Icon::from(AppIcon::File).text_color(theme.muted_foreground)
				};
				table_row(("file-row", ix), ROW_HEIGHT, &theme)
					.when(is_dir, |r| r.cursor_pointer())
					.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
						if is_dir {
							this.open(path.clone(), cx);
						} else if event.click_count() == 1 {
							if !this.selected.remove(&name) {
								this.selected.insert(name.clone());
							}
							cx.notify();
						}
					}))
					.child({
						let name = entry.name.clone();
						Checkbox::new(("file-check", ix))
							.checked(self.selected.contains(&entry.name))
							.on_click(cx.listener(move |this, checked: &bool, _, cx| {
								if *checked {
									this.selected.insert(name.clone());
								} else {
									this.selected.remove(&name);
								}
								cx.stop_propagation();
								cx.notify();
							}))
					})
					.child(
						h_flex()
							.flex_1()
							.min_w_0()
							.gap_2()
							.child(icon.small())
							.child(div().truncate().child(entry.name.clone())),
					)
					.child(
						div()
							.w(px(DATE_W))
							.flex_shrink_0()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(format_date(entry.modified)),
					)
					.child(
						div()
							.w(px(SIZE_W))
							.flex_shrink_0()
							.text_right()
							.text_sm()
							.mono(cx)
							.child(if is_dir {
								String::new()
							} else {
								format_bytes(entry.size)
							}),
					)
					.into_any_element()
			})
			.collect()
	}
}

impl Render for FilesView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let body = match &self.entries {
			None => centered(t!("files.loading").into_owned(), cx),
			Some(Err(e)) => centered(e.clone(), cx),
			Some(Ok(entries)) if entries.is_empty() => centered(t!("files.empty").into_owned(), cx),
			Some(Ok(entries)) => uniform_list(
				"headset-files",
				entries.len(),
				cx.processor(Self::render_rows),
			)
			.flex_1()
			.into_any_element(),
		};
		let drop_tint = cx.theme().primary.opacity(0.08);
		let drop_border = cx.theme().primary;
		v_flex()
			.id("files")
			.size_full()
			.drag_over::<ExternalPaths>(move |style, _, _, _| {
				style.bg(drop_tint).border_2().border_color(drop_border)
			})
			// Dropped files go into the open folder instead of being installed.
			.on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
				this.push(paths.paths().to_vec());
				cx.stop_propagation();
			}))
			.child(self.toolbar(cx))
			.child(self.header_row(cx))
			.child(body)
	}
}
