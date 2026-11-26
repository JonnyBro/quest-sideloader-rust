use rust_i18n::t;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
	ActiveTheme, Disableable, Icon, IconName, Sizable, Theme, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use quest_sideloader_core::catalog::{Catalog, Game};
use quest_sideloader_core::downloaded::DownloadedRelease;
use quest_sideloader_core::{
	AppInfo, Command, Engine, TaskId, TaskKind, TaskSnapshot, TaskState, format_bytes,
};

use crate::icons::AppIcon;
use crate::theme::Mono;
use crate::widgets::{
	Confirm, column_header, confirm, content_width, fit_columns, format_date, open_folder,
	soft_tag, table_row,
};

const ROW_HEIGHT: f32 = 44.;
const COVER: (f32, f32) = (56., 31.5);
const NAME_MIN: f32 = 220.;
const SIZE_W: f32 = 100.;
const DATE_W: f32 = 150.;
const ACTIONS_W: f32 = 270.;

pub enum DownloadsEvent {
	Rename(PathBuf),
	Inspect(PathBuf),
}

pub struct DownloadsView {
	engine: Rc<Engine>,
	folder: PathBuf,
	/// Running, queued and finished tasks, active first.
	tasks: Vec<TaskSnapshot>,
	open_warnings: Vec<TaskId>,
	releases: Option<Vec<DownloadedRelease>>,
	/// Ask the engine for the release list next time the tab is shown.
	stale: bool,
	search: Entity<InputState>,
	target: Option<(String, String)>,
	catalog: Option<Arc<Catalog>>,
	/// Which release columns fit: size, date.
	cols: [bool; 2],
	_search_changes: Subscription,
}

impl EventEmitter<DownloadsEvent> for DownloadsView {}

impl DownloadsView {
	pub fn new(
		engine: Rc<Engine>,
		folder: PathBuf,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("common.search")));
		let changes = cx.subscribe_in(&search, window, |_, _, event, _, cx| {
			if matches!(event, InputEvent::Change) {
				cx.notify();
			}
		});
		Self {
			engine,
			folder,
			tasks: Vec::new(),
			open_warnings: Vec::new(),
			releases: None,
			stale: true,
			search,
			target: None,
			catalog: None,
			cols: [true; 2],
			_search_changes: changes,
		}
	}

	pub fn on_task(&mut self, snapshot: TaskSnapshot, cx: &mut Context<Self>) {
		match self.tasks.iter_mut().find(|t| t.id == snapshot.id) {
			// A late progress update must not bring back a task that was cancelled.
			Some(existing) if existing.state.is_finished() => {}
			Some(existing) => *existing = snapshot,
			None => self.tasks.push(snapshot),
		}
		cx.notify();
	}

	pub fn set_releases(&mut self, releases: Vec<DownloadedRelease>, cx: &mut Context<Self>) {
		self.releases = Some(releases);
		cx.notify();
	}

	pub fn invalidate(&mut self, cx: &mut Context<Self>) {
		self.stale = true;
		cx.notify();
	}

	pub fn set_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
		if self.folder != folder {
			self.folder = folder;
			self.releases = None;
			self.stale = true;
			cx.notify();
		}
	}

	pub fn set_target(&mut self, target: Option<(String, String)>, cx: &mut Context<Self>) {
		self.target = target;
		cx.notify();
	}

	pub fn set_catalog(&mut self, catalog: Option<Arc<Catalog>>, cx: &mut Context<Self>) {
		self.catalog = catalog;
		cx.notify();
	}

	fn cover(&self, release: &str) -> Option<PathBuf> {
		let catalog = self.catalog.as_ref()?;
		let game = catalog.games.iter().find(|g| g.release_name == release)?;
		catalog.thumbnail(&game.package)
	}

	fn catalog_game(&self, release: &str) -> Option<&Game> {
		self.catalog
			.as_ref()?
			.games
			.iter()
			.find(|g| g.release_name == release)
	}

	/// An unfinished download that is running or queued shows in the queue,
	/// so the list leaves it out.
	fn downloading(&self, release: &str) -> bool {
		self.catalog_game(release).is_some_and(|game| {
			self.tasks.iter().any(|t| {
				t.kind == TaskKind::Download && !t.state.is_finished() && t.title == game.name
			})
		})
	}

	fn dismiss_finished(&mut self, cx: &mut Context<Self>) {
		self.tasks.retain(|t| !t.state.is_finished());
		cx.notify();
	}

	fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let summary = self.releases.as_ref().map(|releases| {
			let total: u64 = releases.iter().map(|r| r.size).sum();
			t!("dl.summary", n = releases.len(), size = format_bytes(total)).into_owned()
		});
		let folder = self.folder.clone();
		h_flex()
			.px_5()
			.pt_4()
			.pb_2()
			.gap_3()
			.child(div().text_xl().child(t!("tab.downloads").into_owned()))
			.children(summary.map(|s| div().text_color(theme.muted_foreground).child(s)))
			.child(div().flex_1())
			.child(
				Button::new("refresh")
					.icon(Icon::from(AppIcon::Refresh))
					.ghost()
					.small()
					.tooltip(t!("common.refresh"))
					.on_click(
						cx.listener(|this, _, _, _| this.engine.send(Command::ListDownloads)),
					),
			)
			.child(
				Button::new("open-folder")
					.label(t!("common.open_folder"))
					.icon(Icon::new(IconName::FolderOpen))
					.small()
					.outline()
					.on_click(move |_, _, _| open_folder(&folder)),
			)
	}

	fn queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
		if self.tasks.is_empty() {
			return None;
		}
		let theme = cx.theme().clone();
		let queued: Vec<TaskId> = self
			.tasks
			.iter()
			.filter(|t| t.state == TaskState::Queued)
			.map(|t| t.id)
			.collect();
		// Active first, newest finished below them.
		let mut ordered: Vec<&TaskSnapshot> = self
			.tasks
			.iter()
			.filter(|t| !t.state.is_finished())
			.collect();
		ordered.extend(self.tasks.iter().rev().filter(|t| t.state.is_finished()));
		let any_finished = self.tasks.iter().any(|t| t.state.is_finished());
		let rows: Vec<AnyElement> = ordered
			.into_iter()
			.enumerate()
			.map(|(ix, task)| {
				self.task_row(ix, task, &queued, &theme, cx)
					.into_any_element()
			})
			.collect();
		Some(
			v_flex()
				.px_5()
				.pb_3()
				.gap_2()
				.child(
					h_flex()
						.gap_3()
						.child(
							div()
								.text_color(theme.muted_foreground)
								.child(t!("dl.queue").into_owned()),
						)
						.child(div().flex_1())
						.when(any_finished, |h| {
							h.child(
								Button::new("clear-finished")
									.label(t!("tasks.clear"))
									.outline()
									.xsmall()
									.on_click(
										cx.listener(|this, _, _, cx| this.dismiss_finished(cx)),
									),
							)
						}),
				)
				.children(rows)
				.into_any_element(),
		)
	}

	fn task_row(
		&self,
		ix: usize,
		task: &TaskSnapshot,
		queued: &[TaskId],
		theme: &Theme,
		cx: &mut Context<Self>,
	) -> impl IntoElement {
		let kind = task_kind(task);
		let id = task.id;
		let state = match &task.state {
			TaskState::Queued => div()
				.text_sm()
				.text_color(theme.muted_foreground)
				.child(task.stage.clone())
				.into_any_element(),
			TaskState::Running | TaskState::AwaitingReview => h_flex()
				.gap_3()
				.text_sm()
				.child(match task.progress {
					Some(p) => h_flex()
						.gap_2()
						.child(
							Progress::new(("task-progress", ix))
								.value(p * 100.)
								.w(px(240.)),
						)
						.child(div().mono(cx).child(format!("{:.0}%", p * 100.)))
						.into_any_element(),
					None => Spinner::new().small().into_any_element(),
				})
				.child(div().child(task.stage.clone()))
				.children(
					task.detail
						.clone()
						.map(|d| div().text_color(theme.muted_foreground).mono(cx).child(d)),
				)
				.into_any_element(),
			TaskState::Done => h_flex()
				.gap_3()
				.text_sm()
				.child(div().text_color(theme.success).child(task.stage.clone()))
				.children(task.output.as_ref().map(|path| {
					div()
						.min_w_0()
						.truncate()
						.text_color(theme.muted_foreground)
						.mono(cx)
						.child(path.display().to_string())
				}))
				.into_any_element(),
			TaskState::Cancelled => div()
				.text_sm()
				.text_color(theme.muted_foreground)
				.child(t!("tasks.cancelled").into_owned())
				.into_any_element(),
			TaskState::Failed(error) => div()
				.text_sm()
				.text_color(theme.danger)
				.child(error.clone())
				.into_any_element(),
		};

		let mut buttons = h_flex().gap_1().flex_shrink_0();
		if task.state.is_finished() {
			if let (Some(path), Some((serial, name))) = (&task.output, &self.target) {
				let (path, serial) = (path.clone(), serial.clone());
				buttons = buttons.child(
					Button::new(("install-output", ix))
						.label(t!("common.install"))
						.primary()
						.xsmall()
						.tooltip(t!("common.install_on", name = name))
						.on_click(cx.listener(move |this, _, _, _| {
							this.engine.send(Command::Install {
								serial: serial.clone(),
								path: path.clone(),
								streaming: true,
							})
						})),
				);
			}
			if let (TaskState::Failed(_), Some(conflict), Some(serial)) =
				(&task.state, &task.conflict, &task.serial)
			{
				let (conflict, serial, name) =
					(conflict.clone(), serial.clone(), task.title.clone());
				buttons = buttons.child(
					Button::new(("reinstall", ix))
						.label(t!("tasks.reinstall"))
						.primary()
						.xsmall()
						.tooltip(t!("tasks.reinstall_hint"))
						.on_click(cx.listener(move |this, _, _, _| {
							this.engine.send(Command::ReinstallKeepingSaves {
								serial: serial.clone(),
								app: AppInfo {
									package: conflict.package.clone(),
									name: name.clone(),
									version_code: None,
								},
								source: conflict.source.clone(),
								remove_source: conflict.remove_after,
								streaming: conflict.streaming,
							})
						})),
				);
			}
			if let (TaskState::Failed(_), Some(broken), Some(serial)) =
				(&task.state, &task.broken_stream, &task.serial)
			{
				let (broken, serial) = (broken.clone(), serial.clone());
				buttons = buttons.child(
					Button::new(("no-stream", ix))
						.label(t!("lib.install_no_stream"))
						.primary()
						.xsmall()
						.tooltip(t!("lib.install_no_stream_tip"))
						.on_click(cx.listener(move |this, _, _, _| {
							this.engine.send(Command::RetryWithoutStreaming {
								serial: serial.clone(),
								source: broken.source.clone(),
								remove_source: broken.remove_after,
							})
						})),
				);
			}
			buttons = buttons.child(
				Button::new(("dismiss", ix))
					.icon(Icon::new(IconName::Close))
					.ghost()
					.xsmall()
					.tooltip(t!("tasks.dismiss"))
					.on_click(cx.listener(move |this, _, _, cx| {
						this.tasks.retain(|t| t.id != id);
						cx.notify();
					})),
			);
		} else {
			if let Some(position) = queued.iter().position(|q| *q == id) {
				let move_by = |delta: isize| {
					cx.listener(move |this: &mut Self, _, _, _| {
						this.engine.send(Command::MoveDownload { task: id, delta })
					})
				};
				buttons = buttons
					.child(
						Button::new(("sooner", ix))
							.icon(Icon::new(IconName::ArrowUp))
							.ghost()
							.xsmall()
							.tooltip(t!("tasks.sooner"))
							.disabled(position == 0)
							.on_click(move_by(-1)),
					)
					.child(
						Button::new(("later", ix))
							.icon(Icon::new(IconName::ArrowDown))
							.ghost()
							.xsmall()
							.tooltip(t!("tasks.later"))
							.disabled(position + 1 >= queued.len())
							.on_click(move_by(1)),
					);
			}
			buttons = buttons.child(
				Button::new(("cancel", ix))
					.label(t!("common.cancel"))
					.outline()
					.xsmall()
					.on_click(
						cx.listener(move |this, _, _, _| this.engine.send(Command::CancelTask(id))),
					),
			);
		}

		let warnings_open = self.open_warnings.contains(&id);
		let warnings = (!task.warnings.is_empty()).then(|| {
			v_flex()
				.gap_1()
				.child(
					Button::new(("warnings", ix))
						.label(t!("tasks.warnings", n = task.warnings.len()))
						.icon(Icon::new(if warnings_open {
							IconName::ChevronDown
						} else {
							IconName::ChevronRight
						}))
						.ghost()
						.xsmall()
						.on_click(cx.listener(move |this, _, _, cx| {
							if let Some(pos) = this.open_warnings.iter().position(|w| *w == id) {
								this.open_warnings.remove(pos);
							} else {
								this.open_warnings.push(id);
							}
							cx.notify();
						})),
				)
				.when(warnings_open, |d| {
					d.children(task.warnings.iter().map(|w| {
						div()
							.pl_4()
							.text_sm()
							.text_color(theme.warning)
							.child(w.clone())
					}))
				})
		});

		v_flex()
			.p_3()
			.gap_2()
			.rounded(theme.radius_lg)
			.border_1()
			.border_color(theme.border)
			.bg(theme.muted)
			.child(
				h_flex()
					.gap_3()
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(task.title.clone()))
							.child(
								div()
									.truncate()
									.text_sm()
									.text_color(theme.muted_foreground)
									.child(kind),
							),
					)
					.child(buttons),
			)
			.child(state)
			.children(warnings)
	}

	fn releases(&self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let Some(releases) = &self.releases else {
			return h_flex()
				.p_8()
				.justify_center()
				.child(Spinner::new())
				.into_any_element();
		};
		if releases.is_empty() {
			return v_flex()
				.p_8()
				.items_center()
				.gap_1()
				.child(div().text_lg().child(t!("dl.empty_title").into_owned()))
				.child(
					div()
						.text_color(theme.muted_foreground)
						.child(t!("dl.empty").into_owned()),
				)
				.into_any_element();
		}
		let needle = self.search.read(cx).value().trim().to_lowercase();
		let c = self.cols;
		let header = column_header(cx)
			.child(div().w(px(COVER.0)).flex_shrink_0())
			.child(
				div()
					.flex_1()
					.min_w(px(NAME_MIN))
					.child(t!("dl.col_release").into_owned()),
			)
			.when(c[0], |h| {
				h.child(
					div()
						.w(px(SIZE_W))
						.flex_shrink_0()
						.text_right()
						.child(t!("dl.col_size").into_owned()),
				)
			})
			.when(c[1], |h| {
				h.child(
					div()
						.w(px(DATE_W))
						.flex_shrink_0()
						.text_right()
						.child(t!("dl.col_date").into_owned()),
				)
			})
			.child(div().w(px(ACTIONS_W)).flex_shrink_0());
		let rows = releases
			.iter()
			.filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
			.filter(|r| !r.incomplete || !self.downloading(&r.name))
			.enumerate()
			.map(|(ix, release)| {
				let cover = match self.cover(&release.name) {
					Some(path) => img(path)
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.object_fit(ObjectFit::Cover)
						.into_any_element(),
					None => div()
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.bg(theme.muted)
						.into_any_element(),
				};
				let resume = release.incomplete.then(|| {
					let game = self.catalog_game(&release.name).cloned();
					Button::new(("resume", ix))
						.label(t!("dl.resume"))
						.primary()
						.xsmall()
						.disabled(game.is_none())
						.tooltip(if game.is_some() {
							t!("dl.resume_hint")
						} else {
							t!("dl.resume_unavailable")
						})
						.on_click(cx.listener(move |this, _, _, _| {
							if let Some(game) = &game {
								this.engine.send(Command::Download {
									game: game.clone(),
									serial: None,
									streaming: true,
								});
							}
						}))
				});
				let install = {
					let path = release.path.clone();
					let target = self.target.clone();
					Button::new(("install", ix))
						.label(t!("common.install"))
						.primary()
						.xsmall()
						.disabled(target.is_none())
						.tooltip(match &target {
							Some((_, name)) => t!("common.install_on", name = name),
							None => t!("dl.connect_first"),
						})
						.on_click(cx.listener(move |this, _, _, _| {
							if let Some((serial, _)) = &target {
								this.engine.send(Command::Install {
									serial: serial.clone(),
									path: path.clone(),
									streaming: true,
								});
							}
						}))
				};
				let copy = {
					let path = release.path.clone();
					Button::new(("copy", ix))
						.icon(Icon::from(AppIcon::Rename))
						.ghost()
						.xsmall()
						.tooltip(t!("dl.copy_hint"))
						.on_click(cx.listener(move |_, _, _, cx| {
							cx.emit(DownloadsEvent::Rename(path.clone()))
						}))
				};
				let inspect = {
					let path = release.path.clone();
					Button::new(("inspect", ix))
						.icon(Icon::from(AppIcon::Inspector))
						.ghost()
						.xsmall()
						.tooltip(t!("dl.details"))
						.on_click(cx.listener(move |_, _, _, cx| {
							cx.emit(DownloadsEvent::Inspect(path.clone()))
						}))
				};
				let open = {
					let path = release.path.clone();
					Button::new(("open", ix))
						.icon(Icon::new(IconName::FolderOpen))
						.ghost()
						.xsmall()
						.tooltip(t!("common.open"))
						.on_click(move |_, _, _| open_folder(&path))
				};
				let delete = {
					let engine = self.engine.clone();
					let (name, size) = (release.name.clone(), release.size);
					let incomplete = release.incomplete;
					Button::new(("delete", ix))
						.icon(Icon::new(IconName::Delete))
						.ghost()
						.xsmall()
						.tooltip(t!("common.delete"))
						.on_click(move |_, window, cx| {
							confirm(
								&engine,
								Confirm {
									title: t!("dl.delete_title").into_owned(),
									text: if incomplete {
										t!(
											"dl.delete_partial_text",
											name = name,
											size = format_bytes(size)
										)
									} else {
										t!("dl.delete_text", name = name, size = format_bytes(size))
									}
									.into_owned(),
									ok: t!("common.delete").into_owned(),
									danger: true,
								},
								Command::DeleteDownload {
									name: name.clone(),
									incomplete,
								},
								window,
								cx,
							);
						})
				};
				table_row(("release", ix), ROW_HEIGHT, &theme)
					.child(cover)
					.child(
						h_flex()
							.flex_1()
							.min_w(px(NAME_MIN))
							.gap_2()
							.child(div().min_w_0().truncate().child(release.name.clone()))
							.when(release.incomplete, |d| {
								d.child(
									soft_tag(theme.warning, t!("dl.incomplete").into_owned())
										.flex_shrink_0(),
								)
							}),
					)
					.when(c[0], |r| {
						r.child(
							div()
								.w(px(SIZE_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.mono(cx)
								.child(format_bytes(release.size)),
						)
					})
					.when(c[1], |r| {
						r.child(
							div()
								.w(px(DATE_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.mono(cx)
								.child(release.modified.map(format_date).unwrap_or_default()),
						)
					})
					.child(
						h_flex()
							.w(px(ACTIONS_W))
							.flex_shrink_0()
							.gap_1()
							.justify_end()
							.map(|actions| match resume {
								Some(resume) => actions.child(resume),
								None => actions.child(install).child(copy).child(inspect),
							})
							.child(open)
							.child(delete),
					)
			});
		v_flex().child(header).children(rows).into_any_element()
	}
}

fn task_kind(task: &TaskSnapshot) -> String {
	match (task.kind, &task.serial) {
		(TaskKind::Download, Some(serial)) => t!("tasks.kind_download_install", serial = serial),
		(TaskKind::Download, None) => t!("tasks.kind_download"),
		(TaskKind::Install, Some(serial)) => t!("tasks.kind_install_on", serial = serial),
		(TaskKind::Install, None) => t!("tasks.kind_install"),
		(TaskKind::Upload, Some(serial)) => t!("tasks.kind_upload_on", serial = serial),
		(TaskKind::Upload, None) => t!("tasks.kind_upload"),
		(TaskKind::Backup, _) => t!("tasks.kind_backup"),
		(TaskKind::Restore, _) => t!("tasks.kind_restore"),
		(TaskKind::Uninstall, _) => t!("tasks.kind_uninstall"),
		(TaskKind::AdbSetup, _) => t!("tasks.kind_adb"),
		(TaskKind::RcloneSetup, _) => t!("tasks.kind_rclone"),
		(TaskKind::Rename, _) => t!("tasks.kind_rename"),
		(TaskKind::PullFiles, _) => t!("tasks.kind_pull_files"),
		(TaskKind::PushFiles, _) => t!("tasks.kind_push_files"),
		(TaskKind::PasteFiles, _) => t!("tasks.kind_paste_files"),
	}
	.into_owned()
}

impl Render for DownloadsView {
	fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		if std::mem::take(&mut self.stale) {
			self.engine.send(Command::ListDownloads);
		}
		let fit = fit_columns(
			content_width(window, 0.),
			40. + COVER.0 + 16. + NAME_MIN + 16. + ACTIONS_W,
			&[(SIZE_W, 1), (DATE_W, 0)],
		);
		self.cols = [fit[0], fit[1]];
		let queue = self.queue(cx);
		let releases = self.releases(cx);
		let theme = cx.theme().clone();
		v_flex().size_full().child(self.header(cx)).child(
			div().id("downloads").flex_1().overflow_y_scroll().child(
				v_flex()
					.children(queue)
					.child(
						h_flex()
							.px_5()
							.py_2()
							.gap_3()
							.child(
								div()
									.text_color(theme.muted_foreground)
									.child(t!("dl.on_disk").into_owned()),
							)
							.child(
								div()
									.min_w_0()
									.truncate()
									.text_sm()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(self.folder.display().to_string()),
							)
							.child(div().flex_1())
							.child(
								Input::new(&self.search)
									.prefix(Icon::new(IconName::Search).small())
									.cleanable(true)
									.small()
									.w(px(240.)),
							),
					)
					.child(releases),
			),
		)
	}
}
