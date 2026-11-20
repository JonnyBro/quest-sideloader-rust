use quest_sideloader_core::{Command, TaskId, TaskKind, TaskSnapshot, TaskState};
use rust_i18n::t;

pub struct TasksView {
	tasks: Vec<TaskSnapshot>,
}

impl TasksView {
	pub fn new() -> Self {
		Self { tasks: Vec::new() }
	}

	/// Returns true when this update is the one that finished the task.
	pub fn on_update(&mut self, snapshot: TaskSnapshot) -> bool {
		let finishing = snapshot.state.is_finished();
		match self.tasks.iter_mut().find(|t| t.id == snapshot.id) {
			// A late progress update must not resurrect a task that was cancelled.
			Some(existing) if existing.state.is_finished() => false,
			Some(existing) => {
				*existing = snapshot;
				finishing
			}
			None => {
				self.tasks.push(snapshot);
				finishing
			}
		}
	}

	fn active(&self) -> impl Iterator<Item = &TaskSnapshot> {
		self.tasks.iter().filter(|t| !t.state.is_finished())
	}

	/// The running task, or the first queued one, and how many are active.
	fn current(&self) -> Option<(&TaskSnapshot, usize)> {
		let active: Vec<_> = self.active().collect();
		let current = active
			.iter()
			.find(|t| t.state == TaskState::Running)
			.or(active.first())?;
		Some((current, active.len()))
	}

	/// Goes in front of the window title: the current task's progress, or
	/// the number of active tasks when it has none.
	pub fn title_prefix(&self) -> Option<String> {
		let (current, active) = self.current()?;
		Some(match current.progress {
			Some(p) => format!("{:.0}%", p * 100.0),
			None => t!("tasks.title_active", n = active).into_owned(),
		})
	}

	/// One line for the bottom bar; returns true when clicked.
	pub fn summary(&self, ui: &mut egui::Ui) -> bool {
		let Some((current, active)) = self.current() else {
			return false;
		};
		let mut clicked = false;
		ui.horizontal(|ui| {
			if let Some(p) = current.progress {
				ui.add(
					egui::ProgressBar::new(p)
						.desired_width(120.0)
						.show_percentage(),
				);
			} else {
				ui.spinner();
			}
			let text = format!("{}: {}", current.title, current.stage);
			clicked |= ui.link(text).clicked();
			if active > 1 {
				ui.weak(t!("tasks.more", n = active - 1));
			}
		});
		clicked
	}

	/// `install_target` is where a downloaded release can be installed now.
	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		install_target: Option<(&str, &str)>,
		commands: &mut Vec<Command>,
	) {
		let mut dismiss: Vec<TaskId> = Vec::new();
		ui.horizontal(|ui| {
			ui.heading(t!("tasks.heading"));
			if self.tasks.iter().any(|t| t.state.is_finished())
				&& ui.button(t!("tasks.clear")).clicked()
			{
				dismiss.extend(
					self.tasks
						.iter()
						.filter(|t| t.state.is_finished())
						.map(|t| t.id),
				);
			}
		});
		ui.separator();
		if self.tasks.is_empty() {
			ui.centered_and_justified(|ui| {
				ui.weak(t!("tasks.empty"));
			});
			return;
		}

		let queued: Vec<TaskId> = self
			.tasks
			.iter()
			.filter(|t| t.state == TaskState::Queued)
			.map(|t| t.id)
			.collect();
		egui::ScrollArea::vertical()
			.auto_shrink([false, false])
			.show(ui, |ui| {
				// Active first, newest finished below them.
				let mut ordered: Vec<&TaskSnapshot> = self.active().collect();
				ordered.extend(self.tasks.iter().rev().filter(|t| t.state.is_finished()));
				for task in ordered {
					task_row(ui, task, &queued, install_target, commands, &mut dismiss);
					ui.separator();
				}
			});
		self.tasks.retain(|t| !dismiss.contains(&t.id));
	}
}

fn task_row(
	ui: &mut egui::Ui,
	task: &TaskSnapshot,
	queued: &[TaskId],
	install_target: Option<(&str, &str)>,
	commands: &mut Vec<Command>,
	dismiss: &mut Vec<TaskId>,
) {
	// The buttons go in first, so a long title is cut short instead of
	// pushing them off the row.
	ui.horizontal(|ui| {
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			task_buttons(ui, task, queued, install_target, commands, dismiss);
			ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
				ui.add(egui::Label::new(egui::RichText::new(&task.title).strong()).truncate());
				let kind = match (task.kind, &task.serial) {
					(TaskKind::Download, Some(serial)) => {
						t!("tasks.kind_download_install", serial = serial)
					}
					(TaskKind::Download, None) => t!("tasks.kind_download"),
					(TaskKind::Install, Some(serial)) => {
						t!("tasks.kind_install_on", serial = serial)
					}
					(TaskKind::Install, None) => t!("tasks.kind_install"),
					(TaskKind::Upload, Some(serial)) => t!("tasks.kind_upload_on", serial = serial),
					(TaskKind::Upload, None) => t!("tasks.kind_upload"),
					(TaskKind::Backup, _) => t!("tasks.kind_backup"),
					(TaskKind::Restore, _) => t!("tasks.kind_restore"),
					(TaskKind::Uninstall, _) => t!("tasks.kind_uninstall"),
					(TaskKind::AdbSetup, _) => t!("tasks.kind_adb"),
					(TaskKind::RcloneSetup, _) => t!("tasks.kind_rclone"),
					(TaskKind::Rename, _) => t!("tasks.kind_rename"),
				};
				ui.add(egui::Label::new(egui::RichText::new(kind).weak()).truncate());
			});
		});
	});

	match &task.state {
		TaskState::Queued => {
			ui.weak(&task.stage);
		}
		TaskState::Running | TaskState::AwaitingReview => {
			ui.horizontal(|ui| {
				match task.progress {
					Some(fraction) => {
						ui.add(
							egui::ProgressBar::new(fraction)
								.desired_width(220.0)
								.show_percentage(),
						);
					}
					None => {
						ui.spinner();
					}
				}
				ui.label(&task.stage);
				if let Some(detail) = &task.detail {
					ui.weak(detail);
				}
			});
		}
		TaskState::Done => {
			ui.horizontal(|ui| {
				ui.colored_label(egui::Color32::from_rgb(90, 180, 110), &task.stage);
				if let Some(path) = &task.output {
					ui.add(
						egui::Label::new(egui::RichText::new(path.display().to_string()).weak())
							.truncate(),
					);
				}
			});
		}
		TaskState::Cancelled => {
			ui.weak(t!("tasks.cancelled"));
		}
		TaskState::Failed(error) => {
			ui.colored_label(ui.visuals().error_fg_color, error);
			if let (Some(conflict), Some(serial)) = (&task.conflict, &task.serial)
				&& ui
					.button(t!("tasks.reinstall"))
					.on_hover_text(t!("tasks.reinstall_hint"))
					.clicked()
			{
				commands.push(Command::ReinstallKeepingSaves {
					serial: serial.clone(),
					app: quest_sideloader_core::AppInfo {
						package: conflict.package.clone(),
						name: task.title.clone(),
						version_code: None,
					},
					source: conflict.source.clone(),
				});
			}
		}
	}

	if !task.warnings.is_empty() {
		egui::CollapsingHeader::new(t!("tasks.warnings", n = task.warnings.len()))
			.id_salt(("task_warnings", task.id))
			.show(ui, |ui| {
				for warning in &task.warnings {
					ui.small(warning);
				}
			});
	}
}

fn task_buttons(
	ui: &mut egui::Ui,
	task: &TaskSnapshot,
	queued: &[TaskId],
	install_target: Option<(&str, &str)>,
	commands: &mut Vec<Command>,
	dismiss: &mut Vec<TaskId>,
) {
	if task.state.is_finished() {
		if ui
			.small_button("✕")
			.on_hover_text(t!("tasks.dismiss"))
			.clicked()
		{
			dismiss.push(task.id);
		}
		if let (Some(path), Some((serial, name))) = (&task.output, install_target)
			&& ui
				.small_button(t!("common.install"))
				.on_hover_text(t!("common.install_on", name = name))
				.clicked()
		{
			commands.push(Command::Install {
				serial: serial.to_owned(),
				path: path.clone(),
			});
		}
		return;
	}
	if ui.small_button(t!("common.cancel")).clicked() {
		commands.push(Command::CancelTask(task.id));
	}
	if let Some(position) = queued.iter().position(|id| *id == task.id) {
		if ui
			.add_enabled(position + 1 < queued.len(), egui::Button::new("⏷").small())
			.on_hover_text(t!("tasks.later"))
			.clicked()
		{
			commands.push(Command::MoveDownload {
				task: task.id,
				delta: 1,
			});
		}
		if ui
			.add_enabled(position > 0, egui::Button::new("⏶").small())
			.on_hover_text(t!("tasks.sooner"))
			.clicked()
		{
			commands.push(Command::MoveDownload {
				task: task.id,
				delta: -1,
			});
		}
	}
}
