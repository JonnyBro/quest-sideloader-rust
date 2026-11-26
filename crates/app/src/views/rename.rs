use rust_i18n::t;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Sizable, Theme, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::rename::{
	Analysis, ChangeKind, apk_name, check_package, obb_name, output_dir, rename_value,
	suggest_package,
};
use quest_sideloader_core::{Command, Engine};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::widgets::centered;

const LIST_WIDTH: f32 = 280.;

enum State {
	Analyzing,
	Ready(Box<Analysis>),
	Failed(String),
}

struct Item {
	id: u64,
	path: PathBuf,
	state: State,
	new_package: Entity<InputState>,
	use_loader: bool,
	debuggable: bool,
	/// A build was started; its progress is on the Downloads tab.
	started: bool,
	folded: Vec<&'static str>,
	_changes: Subscription,
}

impl Item {
	fn title(&self) -> String {
		match &self.state {
			State::Ready(a) => a.title.clone(),
			_ => self
				.path
				.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_else(|| self.path.display().to_string()),
		}
	}
}

pub struct RenameView {
	engine: Rc<Engine>,
	downloads_dir: PathBuf,
	items: Vec<Item>,
	selected: Option<u64>,
	next_id: u64,
	loader: Option<PathBuf>,
}

impl RenameView {
	pub fn new(engine: Rc<Engine>, downloads_dir: PathBuf, loader: Option<PathBuf>) -> Self {
		Self {
			engine,
			downloads_dir,
			items: Vec::new(),
			selected: None,
			next_id: 0,
			loader,
		}
	}

	pub fn loader(&self) -> Option<PathBuf> {
		self.loader.clone()
	}

	pub fn set_downloads_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
		self.downloads_dir = dir;
		cx.notify();
	}

	/// One already in the list is only selected.
	pub fn add(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
		if let Some(item) = self.items.iter().find(|i| i.path == path) {
			self.selected = Some(item.id);
			cx.notify();
			return;
		}
		self.next_id += 1;
		let id = self.next_id;
		self.engine.send(Command::AnalyzeRename {
			id,
			path: path.clone(),
		});
		let new_package = cx.new(|cx| InputState::new(window, cx));
		let changes = cx.subscribe_in(&new_package, window, |_, _, event, _, cx| {
			if matches!(event, InputEvent::Change) {
				cx.notify();
			}
		});
		self.items.push(Item {
			id,
			path,
			state: State::Analyzing,
			new_package,
			use_loader: false,
			debuggable: false,
			started: false,
			folded: Vec::new(),
			_changes: changes,
		});
		self.selected = Some(id);
		cx.notify();
	}

	pub fn on_analysis(
		&mut self,
		id: u64,
		result: Result<Analysis, String>,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
			return;
		};
		item.state = match result {
			Ok(analysis) => {
				if item.new_package.read(cx).value().is_empty() {
					let suggestion = suggest_package(&analysis.package);
					item.new_package
						.update(cx, |input, cx| input.set_value(suggestion, window, cx));
				}
				State::Ready(Box::new(analysis))
			}
			Err(e) => State::Failed(e),
		};
		cx.notify();
	}

	fn reanalyze(&mut self, id: u64, cx: &mut Context<Self>) {
		if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
			item.state = State::Analyzing;
			self.engine.send(Command::AnalyzeRename {
				id,
				path: item.path.clone(),
			});
			cx.notify();
		}
	}

	fn item_mut(&mut self, id: u64) -> Option<&mut Item> {
		self.items.iter_mut().find(|i| i.id == id)
	}

	fn pick(&mut self, directories: bool, window: &mut Window, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: !directories,
			directories,
			multiple: true,
			prompt: None,
		});
		cx.spawn_in(window, async move |this, cx| {
			if let Ok(Ok(Some(paths))) = paths.await {
				let _ = this.update_in(cx, |this, window, cx| {
					for path in paths {
						this.add(path, window, cx);
					}
				});
			}
		})
		.detach();
	}

	fn pick_loader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let paths = cx.prompt_for_paths(PathPromptOptions {
			files: true,
			directories: false,
			multiple: false,
			prompt: None,
		});
		cx.spawn_in(window, async move |this, cx| {
			if let Ok(Ok(Some(paths))) = paths.await
				&& let Some(file) = paths.into_iter().next()
			{
				let _ = this.update(cx, |this, cx| {
					this.loader = Some(file);
					cx.notify();
				});
			}
		})
		.detach();
	}

	fn list(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let rows = self.items.iter().map(|item| {
			let id = item.id;
			let selected = self.selected == Some(id);
			let status = match &item.state {
				State::Analyzing => h_flex()
					.gap_1()
					.child(Spinner::new().xsmall())
					.child(t!("rn.analyzing").into_owned())
					.into_any_element(),
				State::Ready(a) => div().mono(cx).child(a.package.clone()).into_any_element(),
				State::Failed(_) => div()
					.text_color(theme.danger)
					.child(t!("rn.failed").into_owned())
					.into_any_element(),
			};
			h_flex()
				.id(("rename-item", id as usize))
				.px_3()
				.py_2()
				.gap_2()
				.rounded(theme.radius)
				.cursor_pointer()
				.when(selected, |r| r.bg(theme.list_active))
				.hover(|s| s.bg(theme.list_hover))
				.on_click(cx.listener(move |this, _, _, cx| {
					this.selected = Some(id);
					cx.notify();
				}))
				.child(
					v_flex()
						.flex_1()
						.min_w_0()
						.child(div().truncate().child(item.title()))
						.child(
							div()
								.truncate()
								.text_sm()
								.text_color(theme.muted_foreground)
								.child(status),
						),
				)
				.child(
					Button::new(("remove", id as usize))
						.icon(Icon::new(IconName::Close))
						.ghost()
						.xsmall()
						.tooltip(t!("rn.remove"))
						.on_click(cx.listener(move |this, _, _, cx| {
							this.items.retain(|i| i.id != id);
							if this.selected == Some(id) {
								this.selected = this.items.first().map(|i| i.id);
							}
							cx.notify();
						})),
				)
		});
		v_flex()
			.id("rename-list")
			.w(px(LIST_WIDTH))
			.flex_shrink_0()
			.h_full()
			.p_2()
			.gap_1()
			.overflow_y_scroll()
			.border_r_1()
			.border_color(theme.border)
			.children(rows)
	}

	fn details(&self, item: &Item, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let id = item.id;
		let analyzing = matches!(item.state, State::Analyzing);
		let head = v_flex()
			.gap_1()
			.child(
				h_flex()
					.gap_2()
					.child(div().text_xl().min_w_0().truncate().child(item.title()))
					.child(
						Button::new("reanalyze")
							.icon(Icon::from(AppIcon::Refresh))
							.ghost()
							.small()
							.disabled(analyzing)
							.tooltip(t!("rn.reanalyze"))
							.on_click(cx.listener(move |this, _, _, cx| this.reanalyze(id, cx))),
					),
			)
			.child(
				div()
					.text_sm()
					.text_color(theme.muted_foreground)
					.mono(cx)
					.child(item.path.display().to_string()),
			);
		let analysis = match &item.state {
			State::Analyzing => {
				return v_flex()
					.gap_4()
					.child(head)
					.child(
						h_flex()
							.gap_2()
							.child(Spinner::new())
							.child(t!("rn.analyzing_long").into_owned()),
					)
					.into_any_element();
			}
			State::Failed(e) => {
				return v_flex()
					.gap_4()
					.child(head)
					.child(div().text_color(theme.danger).child(e.clone()))
					.into_any_element();
			}
			State::Ready(a) => a,
		};
		let old = analysis.package.as_str();
		let typed = item.new_package.read(cx).value().trim().to_owned();
		let problem = check_package(&typed, old);
		// Without a valid name the preview would show nonsense.
		let new = if problem.is_none() {
			typed.as_str()
		} else {
			old
		};

		let version = match (&analysis.version_name, analysis.version_code) {
			(Some(name), Some(code)) => format!("{name} ({code})"),
			(Some(name), None) => name.clone(),
			(None, Some(code)) => code.to_string(),
			(None, None) => "-".into(),
		};
		let input = item.new_package.clone();
		let suggestion = suggest_package(old);
		let summary = v_flex()
			.gap_2()
			.child(fact(
				t!("rn.package").into(),
				div().mono(cx).child(old.to_owned()),
				&theme,
			))
			.child(fact(t!("rn.version").into(), div().child(version), &theme))
			.child(fact(
				t!("rn.new_package").into(),
				h_flex()
					.gap_2()
					.child(
						div()
							.w(px(340.))
							.mono(cx)
							.child(Input::new(&item.new_package).small()),
					)
					.child(
						Button::new("suggest")
							.label(t!("rn.suggest"))
							.small()
							.outline()
							.on_click(move |_, window, cx| {
								let suggestion = suggestion.clone();
								input.update(cx, |input, cx| {
									input.set_value(suggestion, window, cx)
								});
							}),
					),
				&theme,
			))
			.children(
				problem
					.clone()
					.map(|p| div().text_color(theme.danger).child(p)),
			);

		let mut warnings = v_flex().gap_2().text_sm();
		if analysis.dex_count > 0 {
			warnings = warnings
				.child(
					div()
						.text_color(theme.warning)
						.child(t!("rn.dex_warning", n = analysis.dex_count).into_owned()),
				)
				.child(self.section(
					item,
					"dex",
					t!("rn.dex_examples").into(),
					v_flex().children(analysis.dex_strings.iter().map(|s| {
						div()
							.truncate()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(s.clone())
					})),
					cx,
				));
		}
		if analysis.firebase {
			warnings = warnings.child(
				div()
					.text_color(theme.warning)
					.child(t!("rn.firebase").into_owned()),
			);
		}
		if analysis.other_apks > 0 {
			warnings = warnings.child(
				div()
					.text_color(theme.warning)
					.child(t!("rn.other_apks", n = analysis.other_apks).into_owned()),
			);
		}
		if !analysis.jni_libraries.is_empty() {
			warnings = warnings.child(
				div()
					.text_color(theme.muted_foreground)
					.child(t!("rn.jni", n = analysis.jni_libraries.len()).into_owned()),
			);
		}
		if let Some(hash) = &analysis.platform_loader {
			warnings = warnings.child(
				v_flex()
					.text_color(theme.muted_foreground)
					.child(div().child(t!("rn.loader").into_owned()))
					.child(div().truncate().mono(cx).child(hash.clone())),
			);
		}

		let manifest: Vec<Change> = analysis
			.manifest
			.iter()
			.map(|c| Change {
				place: format!("<{}> {}", c.element, c.attribute),
				after: c.new_value(old, new),
				before: c.value.clone(),
				note: (c.kind == ChangeKind::ExpandClass).then(|| t!("rn.expanded").into_owned()),
			})
			.collect();
		let simple = |values: &[String], rename: &dyn Fn(&str) -> String| -> Vec<Change> {
			values
				.iter()
				.map(|v| Change {
					place: String::new(),
					before: v.clone(),
					after: rename(v),
					note: None,
				})
				.collect()
		};
		let rename_text = |v: &str| rename_value(v, old, new).unwrap_or_else(|| v.to_owned());
		let mut sections = v_flex().gap_3().child(self.section(
			item,
			"manifest",
			t!("rn.manifest", n = analysis.manifest.len()).into(),
			changes("manifest", &manifest, &theme, cx),
			cx,
		));
		if !analysis.resources.is_empty() {
			sections = sections.child(self.section(
				item,
				"resources",
				t!("rn.resources", n = analysis.resources.len()).into(),
				changes(
					"resources",
					&simple(&analysis.resources, &rename_text),
					&theme,
					cx,
				),
				cx,
			));
		}
		if analysis.resource_package {
			let change = Change {
				place: String::new(),
				before: old.to_owned(),
				after: new.to_owned(),
				note: None,
			};
			sections = sections.child(self.section(
				item,
				"resource-package",
				t!("rn.resource_package").into(),
				changes("resource-package", &[change], &theme, cx),
				cx,
			));
		}
		let source_apk = analysis
			.apk
			.file_name()
			.unwrap_or_default()
			.to_string_lossy()
			.into_owned();
		let copy_apk = apk_name(&source_apk, old, new);
		if copy_apk != source_apk {
			let change = Change {
				place: String::new(),
				before: source_apk,
				after: copy_apk,
				note: None,
			};
			sections = sections.child(self.section(
				item,
				"apk",
				t!("rn.apk_file").into(),
				changes("apk", &[change], &theme, cx),
				cx,
			));
		}
		if !analysis.obb_files.is_empty() {
			let obb = simple(&analysis.obb_files, &|f| obb_name(f, old, new));
			sections = sections.child(
				self.section(
					item,
					"obb",
					t!("rn.obb", n = analysis.obb_files.len()).into(),
					v_flex()
						.gap_1()
						.child(
							div()
								.text_sm()
								.mono(cx)
								.child(format!("Android/obb/{old}  ->  Android/obb/{new}")),
						)
						.child(changes("obb", &obb, &theme, cx)),
					cx,
				),
			);
		}
		if !analysis.script_lines.is_empty() {
			sections = sections.child(self.section(
				item,
				"script",
				t!("rn.script", n = analysis.script_lines.len()).into(),
				changes(
					"script",
					&simple(&analysis.script_lines, &rename_text),
					&theme,
					cx,
				),
				cx,
			));
		}

		let mut options = v_flex().gap_3();
		if analysis.platform_loader.is_some() {
			let loader = match &self.loader {
				Some(path) => div().mono(cx).truncate().child(path.display().to_string()),
				None => div()
					.text_color(theme.muted_foreground)
					.child(t!("rn.no_loader_file").into_owned()),
			};
			options = options.child(
				v_flex()
					.gap_1()
					.child(
						Checkbox::new("use-loader")
							.label(t!("rn.use_loader").into_owned())
							.checked(item.use_loader)
							.on_click(cx.listener(move |this, checked: &bool, _, cx| {
								if let Some(item) = this.item_mut(id) {
									item.use_loader = *checked;
								}
								cx.notify();
							})),
					)
					.child(
						h_flex()
							.pl_6()
							.gap_2()
							.text_sm()
							.child(div().flex_1().min_w_0().child(loader))
							.child(
								Button::new("pick-loader")
									.label(t!("rn.pick_loader"))
									.small()
									.outline()
									.on_click(cx.listener(|this, _, window, cx| {
										this.pick_loader(window, cx)
									})),
							),
					)
					.child(
						div()
							.pl_6()
							.text_sm()
							.text_color(theme.muted_foreground)
							.child(t!("rn.loader_hint").into_owned()),
					),
			);
		}
		options = options.child(if analysis.debuggable {
			div()
				.text_sm()
				.text_color(theme.muted_foreground)
				.child(t!("rn.already_debuggable").into_owned())
				.into_any_element()
		} else {
			v_flex()
				.gap_1()
				.child(
					Checkbox::new("debuggable")
						.label(t!("rn.debuggable").into_owned())
						.checked(item.debuggable)
						.on_click(cx.listener(move |this, checked: &bool, _, cx| {
							if let Some(item) = this.item_mut(id) {
								item.debuggable = *checked;
							}
							cx.notify();
						})),
				)
				.child(
					div()
						.pl_6()
						.text_sm()
						.text_color(theme.muted_foreground)
						.child(t!("rn.debuggable_hint").into_owned()),
				)
				.into_any_element()
		});

		let out = output_dir(&item.path, &self.downloads_dir, new);
		let loader_missing = item.use_loader && self.loader.is_none();
		let exists = out.exists();
		let can_build = problem.is_none() && !loader_missing && !exists;
		let block = if exists {
			Some(t!("rn.output_exists"))
		} else if loader_missing {
			Some(t!("rn.no_loader_file"))
		} else {
			None
		};
		let (new_owned, title) = (new.to_owned(), analysis.title.clone());
		let build = h_flex()
			.gap_3()
			.child(
				Button::new("build")
					.label(t!("rn.build"))
					.primary()
					.disabled(!can_build)
					.when_some(block, |b, tip| b.tooltip(tip))
					.on_click(cx.listener(move |this, _, _, cx| {
						let loader = this.loader.clone();
						let Some(item) = this.item_mut(id) else {
							return;
						};
						item.started = true;
						let command = Command::BuildCopy {
							source: item.path.clone(),
							new_package: new_owned.clone(),
							loader: loader.filter(|_| item.use_loader),
							debuggable: item.debuggable,
							title: title.clone(),
						};
						this.engine.send(command);
						cx.notify();
					})),
			)
			.when(item.started, |b| {
				b.child(
					div()
						.text_sm()
						.text_color(theme.muted_foreground)
						.child(t!("rn.started").into_owned()),
				)
			});

		v_flex()
			.gap_5()
			.child(head)
			.child(summary)
			.child(warnings)
			.child(sections)
			.child(
				v_flex()
					.gap_3()
					.pt_4()
					.border_t_1()
					.border_color(theme.border)
					.child(options)
					.child(
						div()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(t!("rn.output", path = out.display()).into_owned()),
					)
					.child(build),
			)
			.into_any_element()
	}

	fn section(
		&self,
		item: &Item,
		key: &'static str,
		title: String,
		body: impl IntoElement,
		cx: &mut Context<Self>,
	) -> impl IntoElement {
		let id = item.id;
		let open = !item.folded.contains(&key);
		v_flex()
			.gap_2()
			.child(
				h_flex()
					.id(SharedString::from(format!("section-{key}")))
					.gap_1()
					.cursor_pointer()
					.child(
						Icon::new(if open {
							IconName::ChevronDown
						} else {
							IconName::ChevronRight
						})
						.small(),
					)
					.child(div().child(title))
					.on_click(cx.listener(move |this, _, _, cx| {
						if let Some(item) = this.item_mut(id) {
							if let Some(pos) = item.folded.iter().position(|k| *k == key) {
								item.folded.remove(pos);
							} else {
								item.folded.push(key);
							}
						}
						cx.notify();
					})),
			)
			.when(open, |s| s.child(div().pl_5().child(body)))
	}
}

struct Change {
	place: String,
	before: String,
	after: String,
	note: Option<String>,
}

/// Unchanged rows are dimmed; long values are cut short and shown whole on
/// hover. `key` keeps element ids apart between the tables of one release.
fn changes(key: &str, rows: &[Change], theme: &Theme, cx: &App) -> AnyElement {
	let with_place = rows.iter().any(|r| !r.place.is_empty());
	let cell = |id: String, text: String, full: String| {
		div()
			.id(SharedString::from(id))
			.flex_1()
			.min_w_0()
			.truncate()
			.mono(cx)
			.tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
			.child(text)
	};
	v_flex()
		.text_sm()
		.children(rows.iter().enumerate().map(|(ix, row)| {
			let same = row.before == row.after;
			let after_full = match &row.note {
				Some(note) => format!("{}\n\n{note}", row.after),
				None => row.after.clone(),
			};
			let after_text = match &row.note {
				Some(_) => format!("{} *", row.after),
				None => row.after.clone(),
			};
			h_flex()
				.gap_3()
				.py_0p5()
				.border_b_1()
				.border_color(theme.border.opacity(0.5))
				.when(with_place, |r| {
					r.child(
						div()
							.w(px(200.))
							.flex_shrink_0()
							.truncate()
							.text_color(theme.muted_foreground)
							.child(row.place.clone()),
					)
				})
				.child(cell(
					format!("{key}-before-{ix}"),
					row.before.clone(),
					row.before.clone(),
				))
				.child(
					cell(format!("{key}-after-{ix}"), after_text, after_full)
						.when(same, |d| d.text_color(theme.muted_foreground))
						.when(row.note.is_some(), |d| d.text_color(theme.info)),
				)
		}))
		.into_any_element()
}

fn fact(label: String, value: impl IntoElement, theme: &Theme) -> impl IntoElement {
	h_flex()
		.gap_4()
		.child(
			div()
				.w(px(160.))
				.flex_shrink_0()
				.text_color(theme.muted_foreground)
				.child(label),
		)
		.child(value)
}

impl Render for RenameView {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme().clone();
		let header = h_flex()
			.px_5()
			.pt_4()
			.pb_3()
			.gap_3()
			.border_b_1()
			.border_color(theme.border)
			.child(div().text_xl().child(t!("rn.heading").into_owned()))
			.child(div().flex_1())
			.child(
				Button::new("add-folder")
					.label(t!("rn.add_folder"))
					.small()
					.outline()
					.on_click(cx.listener(|this, _, window, cx| this.pick(true, window, cx))),
			)
			.child(
				Button::new("add-apk")
					.label(t!("rn.add_apk"))
					.small()
					.outline()
					.on_click(cx.listener(|this, _, window, cx| this.pick(false, window, cx))),
			);
		let body = if self.items.is_empty() {
			v_flex()
				.size_full()
				.items_center()
				.justify_center()
				.gap_1()
				.p_8()
				.child(div().text_lg().child(t!("rn.empty_title").into_owned()))
				.child(
					div()
						.text_color(theme.muted_foreground)
						.child(t!("rn.empty").into_owned()),
				)
				.into_any_element()
		} else {
			let details = match self
				.selected
				.and_then(|id| self.items.iter().find(|i| i.id == id))
			{
				Some(item) => div()
					.id("rename-details")
					.flex_1()
					.min_w_0()
					.h_full()
					.overflow_y_scroll()
					.child(div().max_w(px(1000.)).p_5().child(self.details(item, cx)))
					.into_any_element(),
				None => centered(t!("rn.select").into_owned(), cx),
			};
			h_flex()
				.size_full()
				.child(self.list(cx))
				.child(details)
				.into_any_element()
		};
		v_flex()
			.size_full()
			.child(header)
			.child(div().flex_1().min_h_0().child(body))
	}
}
