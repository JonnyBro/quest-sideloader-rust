use super::*;

struct Pending {
	job: Job,
	tracker: Arc<Tracker>,
	review: oneshot::Receiver<ReviewDecision>,
}

/// Queue of jobs that haven't started transferring yet.
pub(in crate::engine) struct Queue {
	pending: VecDeque<Pending>,
	transferring: HashSet<TaskId>,
	slot_tx: mpsc::UnboundedSender<TaskId>,
}

impl Queue {
	pub fn new(slot_tx: mpsc::UnboundedSender<TaskId>) -> Self {
		Self {
			pending: VecDeque::new(),
			transferring: HashSet::new(),
			slot_tx,
		}
	}

	/// Adds a job and returns the handle the engine tracks it by.
	pub fn push(
		&mut self,
		job: Job,
		sink: EventSink,
	) -> (oneshot::Sender<ReviewDecision>, Arc<Tracker>) {
		let (review_tx, review_rx) = oneshot::channel();
		let tracker = Tracker::new(
			TaskSnapshot {
				id: job.id,
				kind: TaskKind::Download,
				serial: job.serial.clone(),
				title: job.game.name.clone(),
				stage: t!("task.queued").into_owned(),
				progress: None,
				detail: None,
				state: TaskState::Queued,
				warnings: Vec::new(),
				output: None,
				conflict: None,
			},
			sink,
		);
		self.pending.push_back(Pending {
			job,
			tracker: tracker.clone(),
			review: review_rx,
		});
		(review_tx, tracker)
	}

	/// Starts queued jobs while there are free slots. Returns the started
	/// jobs' abort handles so the engine can cancel them.
	pub fn start_ready(
		&mut self,
		limit: usize,
		deps: &Deps,
	) -> Vec<(TaskId, tokio::task::AbortHandle)> {
		let mut started = Vec::new();
		while self.transferring.len() < limit.max(1) {
			let Some(next) = self.pending.pop_front() else {
				break;
			};
			let id = next.job.id;
			self.transferring.insert(id);
			let slot = Slot {
				id,
				tx: self.slot_tx.clone(),
				freed: AtomicBool::new(false),
			};
			let deps = deps.clone();
			let handle = tokio::spawn(async move {
				let Pending {
					job,
					tracker,
					review,
				} = next;
				let result = run(&job, &deps, &tracker, &slot, review).await;
				slot.free();
				finish(&job, &deps, &tracker, result);
			});
			started.push((id, handle.abort_handle()));
		}
		started
	}

	/// A job finished transferring (or died); its slot is free again.
	pub fn slot_freed(&mut self, id: TaskId) {
		self.transferring.remove(&id);
	}

	/// Removes a job that hasn't started yet. Returns whether it was queued.
	pub fn remove_pending(&mut self, id: TaskId) -> bool {
		let before = self.pending.len();
		self.pending.retain(|p| p.job.id != id);
		self.pending.len() != before
	}

	/// Moves a queued job by `delta` places (negative is towards the front).
	pub fn move_by(&mut self, id: TaskId, delta: isize) {
		let Some(from) = self.pending.iter().position(|p| p.job.id == id) else {
			return;
		};
		let to = from
			.saturating_add_signed(delta)
			.min(self.pending.len() - 1);
		if let Some(item) = self.pending.remove(from) {
			self.pending.insert(to, item);
		}
		for (i, p) in self.pending.iter().enumerate() {
			p.tracker.update(
				|s| s.stage = t!("task.queued_pos", pos = i + 1).into_owned(),
				true,
			);
		}
	}

	pub fn abort_all(&mut self) {
		self.pending.clear();
	}
}

/// A transfer slot; tells the engine when it's free, even if the job is aborted.
pub(super) struct Slot {
	id: TaskId,
	tx: mpsc::UnboundedSender<TaskId>,
	freed: AtomicBool,
}

impl Slot {
	pub(super) fn free(&self) {
		if !self.freed.swap(true, Ordering::SeqCst) {
			let _ = self.tx.send(self.id);
		}
	}
}

impl Drop for Slot {
	fn drop(&mut self) {
		self.free();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn queue_order() {
		let (tx, _rx) = mpsc::unbounded_channel();
		let mut queue = Queue::new(tx);
		let (sink_tx, _sink_rx) = std::sync::mpsc::channel();
		let sink = EventSink {
			tx: sink_tx,
			notify: Arc::new(|| {}),
		};
		for id in 1..=3 {
			let mirror = Mirror {
				id: "m".into(),
				name: "m".into(),
				kind: crate::catalog::MirrorKind::Http {
					base_uri: "https://m/".into(),
				},
				password: "aGk=".into(),
			};
			let game = Game {
				name: format!("G{id}"),
				release_name: format!("G{id} v1"),
				package: format!("com.g{id}.x"),
				version_code: None,
				last_updated: String::new(),
				size_mb: None,
				downloads: None,
			};
			queue.push(
				Job {
					id,
					game,
					mirror,
					serial: None,
				},
				sink.clone(),
			);
		}
		queue.move_by(3, -5);
		let order: Vec<_> = queue.pending.iter().map(|p| p.job.id).collect();
		assert_eq!(order, [3, 1, 2]);
		queue.move_by(3, 1);
		assert!(queue.remove_pending(2));
		assert!(!queue.remove_pending(2));
		let order: Vec<_> = queue.pending.iter().map(|p| p.job.id).collect();
		assert_eq!(order, [1, 3]);
	}
}
