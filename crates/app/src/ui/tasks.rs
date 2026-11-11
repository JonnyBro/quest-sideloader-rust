use quest_sideloader_core::{Command, TaskId, TaskSnapshot, TaskState};

pub struct TasksView {
	tasks: Vec<TaskSnapshot>,
}

impl TasksView {
	pub fn new() -> Self {
		Self { tasks: Vec::new() }
	}

	pub fn is_empty(&self) -> bool {
		self.tasks.is_empty()
	}

	pub fn on_update(&mut self, snapshot: TaskSnapshot) {
		match self.tasks.iter_mut().find(|t| t.id == snapshot.id) {
			// A late progress update must not resurrect a task that was cancelled.
			Some(existing) if existing.state.is_finished() => {}
			Some(existing) => *existing = snapshot,
			None => self.tasks.push(snapshot),
		}
	}

	pub fn show(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
		let mut dismiss: Vec<TaskId> = Vec::new();
		ui.horizontal(|ui| {
			ui.strong("Installations");
			if self.tasks.iter().any(|t| t.state.is_finished())
				&& ui.small_button("Clear finished").clicked()
			{
				dismiss.extend(
					self.tasks
						.iter()
						.filter(|t| t.state.is_finished())
						.map(|t| t.id),
				);
			}
		});

		egui::ScrollArea::vertical()
			.max_height(180.0)
			.auto_shrink([false, true])
			.show(ui, |ui| {
				for task in self.tasks.iter().rev() {
					task_row(ui, task, commands, &mut dismiss);
					ui.separator();
				}
			});
		self.tasks.retain(|t| !dismiss.contains(&t.id));
	}
}

fn task_row(
	ui: &mut egui::Ui,
	task: &TaskSnapshot,
	commands: &mut Vec<Command>,
	dismiss: &mut Vec<TaskId>,
) {
	ui.horizontal(|ui| {
		ui.label(egui::RichText::new(&task.title).strong());
		ui.weak(&task.serial);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if task.state.is_finished() {
				if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
					dismiss.push(task.id);
				}
			} else if ui.small_button("Cancel").clicked() {
				commands.push(Command::CancelTask(task.id));
			}
		});
	});

	match &task.state {
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
			});
		}
		TaskState::Done => {
			ui.colored_label(egui::Color32::from_rgb(90, 180, 110), "Installed");
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
