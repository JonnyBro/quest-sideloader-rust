use quest_sideloader_core::{Command, TaskId, TaskKind, TaskSnapshot, TaskState};

pub struct TasksView {
	tasks: Vec<TaskSnapshot>,
}

impl TasksView {
	pub fn new() -> Self {
		Self { tasks: Vec::new() }
	}

	pub fn on_update(&mut self, snapshot: TaskSnapshot) {
		match self.tasks.iter_mut().find(|t| t.id == snapshot.id) {
			// A late progress update must not resurrect a task that was cancelled.
			Some(existing) if existing.state.is_finished() => {}
			Some(existing) => *existing = snapshot,
			None => self.tasks.push(snapshot),
		}
	}

	fn active(&self) -> impl Iterator<Item = &TaskSnapshot> {
		self.tasks.iter().filter(|t| !t.state.is_finished())
	}

	/// One line for the bottom bar; returns true when clicked.
	pub fn summary(&self, ui: &mut egui::Ui) -> bool {
		let active: Vec<_> = self.active().collect();
		let Some(current) = active
			.iter()
			.find(|t| t.state == TaskState::Running)
			.or(active.first())
		else {
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
			if active.len() > 1 {
				ui.weak(format!("+{} more", active.len() - 1));
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
			ui.heading("Downloads and installations");
			if self.tasks.iter().any(|t| t.state.is_finished())
				&& ui.button("Clear finished").clicked()
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
				ui.weak(
					"Nothing here yet. Download games from the Library, or drop files onto the window.",
				);
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
	ui.horizontal(|ui| {
		ui.label(egui::RichText::new(&task.title).strong());
		ui.weak(match (task.kind, &task.serial) {
			(TaskKind::Download, Some(serial)) => format!("download and install · {serial}"),
			(TaskKind::Download, None) => "download only".to_owned(),
			(TaskKind::Install, Some(serial)) => format!("install · {serial}"),
			(TaskKind::Install, None) => "install".to_owned(),
		});
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if task.state.is_finished() {
				if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
					dismiss.push(task.id);
				}
				if let (Some(path), Some((serial, name))) = (&task.output, install_target)
					&& ui
						.small_button("Install")
						.on_hover_text(format!("Install on {name}"))
						.clicked()
				{
					commands.push(Command::Install {
						serial: serial.to_owned(),
						path: path.clone(),
					});
				}
				return;
			}
			if ui.small_button("Cancel").clicked() {
				commands.push(Command::CancelTask(task.id));
			}
			if let Some(position) = queued.iter().position(|id| *id == task.id) {
				if ui
					.add_enabled(position + 1 < queued.len(), egui::Button::new("⏷").small())
					.on_hover_text("Later")
					.clicked()
				{
					commands.push(Command::MoveDownload {
						task: task.id,
						delta: 1,
					});
				}
				if ui
					.add_enabled(position > 0, egui::Button::new("⏶").small())
					.on_hover_text("Sooner")
					.clicked()
				{
					commands.push(Command::MoveDownload {
						task: task.id,
						delta: -1,
					});
				}
			}
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
					ui.weak(path.display().to_string());
				}
			});
		}
		TaskState::Cancelled => {
			ui.weak("Cancelled");
		}
		TaskState::Failed(error) => {
			ui.colored_label(ui.visuals().error_fg_color, error);
		}
	}

	if !task.warnings.is_empty() {
		egui::CollapsingHeader::new(format!("{} warning(s)", task.warnings.len()))
			.id_salt(("task_warnings", task.id))
			.show(ui, |ui| {
				for warning in &task.warnings {
					ui.small(warning);
				}
			});
	}
}
