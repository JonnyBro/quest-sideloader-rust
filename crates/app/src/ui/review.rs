use std::collections::VecDeque;

use quest_sideloader_core::sideload::script::{LineKind, ScriptAnalysis, ScriptLine, Verdict};
use quest_sideloader_core::{Command, ReviewDecision, TaskId};

struct PendingReview {
	task: TaskId,
	title: String,
	analysis: ScriptAnalysis,
}

/// Shows install.txt scripts that need a decision, one at a time.
pub struct ReviewDialog {
	queue: VecDeque<PendingReview>,
}

impl ReviewDialog {
	pub fn new() -> Self {
		Self {
			queue: VecDeque::new(),
		}
	}

	pub fn push(&mut self, task: TaskId, title: String, analysis: ScriptAnalysis) {
		self.queue.push_back(PendingReview {
			task,
			title,
			analysis,
		});
	}

	/// The task was cancelled elsewhere; its question is moot.
	pub fn forget(&mut self, task: TaskId) {
		self.queue.retain(|r| r.task != task);
	}

	pub fn show(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
		let Some(review) = self.queue.front() else {
			return;
		};
		let mut decision = None;

		let modal =
			egui::Modal::new(egui::Id::unique(("script_review", review.task))).show(ctx, |ui| {
				let screen = ctx.content_rect();
				ui.set_width((screen.width() - 120.0).clamp(420.0, 900.0));

				ui.heading(format!("Review install.txt: {}", review.title));
				ui.label(
					"This release comes with a script of adb commands. Some lines do more than \
				 install the game, so they won't run without your permission.",
				);
				if let Some(package) = &review.analysis.package {
					ui.horizontal(|ui| {
						ui.label("Game package:");
						ui.monospace(package);
					});
				}
				legend(ui);
				ui.add_space(6.0);

				egui::ScrollArea::both()
					.max_height((screen.height() - 300.0).max(160.0))
					.auto_shrink([false, true])
					.show(ui, |ui| {
						for line in &review.analysis.lines {
							script_line(ui, line);
						}
					});

				ui.add_space(8.0);
				let suspicious = count(&review.analysis, |v| matches!(v, Verdict::Suspicious(_)));
				let blocked = count(&review.analysis, |v| matches!(v, Verdict::Blocked(_)));
				ui.label(format!(
					"{suspicious} line(s) need permission, {blocked} line(s) will never run."
				));
				ui.horizontal(|ui| {
					if suspicious > 0
						&& ui
							.button("Run all allowed lines")
							.on_hover_text(
								"Runs safe and highlighted lines; blocked lines are still skipped",
							)
							.clicked()
					{
						decision = Some(ReviewDecision::RunAll);
					}
					if ui
						.button("Run safe lines only")
						.on_hover_text("The game may not work if a skipped line was needed")
						.clicked()
					{
						decision = Some(ReviewDecision::SafeOnly);
					}
					if ui.button("Cancel installation").clicked() {
						decision = Some(ReviewDecision::Cancel);
					}
				});
			});
		if modal.should_close() && decision.is_none() {
			decision = Some(ReviewDecision::Cancel);
		}

		if let Some(decision) = decision {
			let review = self.queue.pop_front().expect("front review exists");
			commands.push(Command::ReviewScript {
				task: review.task,
				decision,
			});
		}
	}
}

fn count(analysis: &ScriptAnalysis, pred: impl Fn(&Verdict) -> bool) -> usize {
	analysis
		.lines
		.iter()
		.filter(|l| l.verdict().is_some_and(&pred))
		.count()
}

fn suspicious_color(ui: &egui::Ui) -> egui::Color32 {
	if ui.visuals().dark_mode {
		egui::Color32::from_rgb(90, 70, 20)
	} else {
		egui::Color32::from_rgb(255, 236, 170)
	}
}

fn blocked_color(ui: &egui::Ui) -> egui::Color32 {
	if ui.visuals().dark_mode {
		egui::Color32::from_rgb(100, 30, 30)
	} else {
		egui::Color32::from_rgb(255, 205, 205)
	}
}

fn legend(ui: &mut egui::Ui) {
	ui.horizontal(|ui| {
		for (color, text) in [
			(suspicious_color(ui), "needs permission"),
			(blocked_color(ui), "blocked"),
		] {
			let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
			ui.painter().rect_filled(rect, 2.0, color);
			ui.label(text);
			ui.add_space(8.0);
		}
		ui.weak("grey lines are not adb commands and are skipped");
	});
}

fn script_line(ui: &mut egui::Ui, line: &ScriptLine) {
	let (fill, reason) = match &line.kind {
		LineKind::Command {
			verdict: Verdict::Suspicious(reason),
			..
		} => (Some(suspicious_color(ui)), Some(reason.as_str())),
		LineKind::Command {
			verdict: Verdict::Blocked(reason),
			..
		} => (Some(blocked_color(ui)), Some(reason.as_str())),
		_ => (None, None),
	};
	let ignored = matches!(line.kind, LineKind::Ignored(_));

	let frame = egui::Frame::new()
		.fill(fill.unwrap_or(egui::Color32::TRANSPARENT))
		.inner_margin(egui::Margin::symmetric(4, 2))
		.corner_radius(2.0);
	frame.show(ui, |ui| {
		ui.set_min_width(ui.available_width());
		ui.horizontal(|ui| {
			ui.add_sized(
				[32.0, 0.0],
				egui::Label::new(
					egui::RichText::new(line.number.to_string())
						.monospace()
						.weak(),
				),
			);
			let text = egui::RichText::new(if line.text.is_empty() {
				" "
			} else {
				&line.text
			})
			.monospace();
			ui.label(if ignored { text.weak() } else { text });
		});
		if let Some(reason) = reason {
			ui.horizontal(|ui| {
				ui.add_space(40.0);
				ui.label(egui::RichText::new(format!("⚠ {reason}")).small().strong());
			});
		}
	});
}
