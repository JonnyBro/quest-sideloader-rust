use super::*;

impl State {
	pub(super) fn new_task_id(&mut self) -> TaskId {
		self.tasks
			.retain(|_, t| t.abort.as_ref().is_none_or(|a| !a.is_finished()));
		self.next_task += 1;
		self.next_task
	}

	pub(super) fn start_install(
		&mut self,
		serial: String,
		path: PathBuf,
		streaming: bool,
		remove_source: bool,
	) {
		let id = self.new_task_id();
		let ctx = install::InstallContext {
			id,
			device: self.adb.device(serial),
			work_dir: self.paths.cache_dir().join("sideload"),
			locks: self.device_locks.clone(),
			sink: self.sink.clone(),
			title_from_files: true,
			backups: self.update_backups(),
			streaming,
		};
		self.tasks
			.insert(id, install::spawn(ctx, path, remove_source));
	}

	pub(super) fn update_backups(&self) -> Option<PathBuf> {
		self.settings
			.backup_before_update
			.then(|| self.paths.backups_dir())
	}

	pub(super) fn download_deps(&self) -> downloads::Deps {
		downloads::Deps {
			fetcher: self.library.fetcher().clone(),
			adb: self.adb.clone(),
			locks: self.device_locks.clone(),
			sink: self.sink.clone(),
			work_dir: self.paths.cache_dir().join("sideload"),
			downloads_dir: self.settings.downloads_dir.clone(),
			keep_downloads: self.settings.keep_downloads,
			backups: self.update_backups(),
		}
	}

	pub(super) fn enqueue_download(&mut self, game: Game, serial: Option<String>, streaming: bool) {
		let Some(mirror) = self.settings.active_mirror().cloned() else {
			self.sink
				.emit(Event::Error(t!("lib.no_mirror").into_owned()));
			return;
		};
		let unfinished = |t: &&task::TaskEntry| {
			t.release.is_some() && t.abort.as_ref().is_none_or(|a| !a.is_finished())
		};
		if self.tasks.values().filter(unfinished).count() >= MAX_DOWNLOAD_JOBS {
			self.sink.emit(Event::Notice(
				t!("eng.queue_full", max = MAX_DOWNLOAD_JOBS).into_owned(),
			));
			return;
		}
		let active = self
			.tasks
			.values()
			.filter(unfinished)
			.any(|t| t.release.as_deref() == Some(game.release_name.as_str()));
		if active {
			self.sink.emit(Event::Notice(
				t!("eng.already_queued", name = game.name).into_owned(),
			));
			return;
		}
		let id = self.new_task_id();
		let release = game.release_name.clone();
		let job = downloads::Job {
			id,
			game,
			mirror,
			serial,
			streaming,
		};
		let (review, tracker) = self.queue.push(job, self.sink.clone());
		self.tasks
			.insert(id, downloads::entry(release, review, tracker));
		self.start_downloads();
	}

	pub(super) fn start_contribution(&mut self, serial: String, package: String, name: String) {
		// A renamed copy is re-signed by us; it must never reach the mirror.
		if crate::rename::KeyStore::new(self.paths.signing_dir()).has(&package) {
			self.sink.emit(Event::Error(
				t!("eng.copy_not_shared", package = package).into_owned(),
			));
			return;
		}
		let Some(target) = self.library.current().and_then(|c| c.upload_target()) else {
			self.sink
				.emit(Event::Error(t!("eng.no_uploads").into_owned()));
			return;
		};
		let id = self.new_task_id();
		let entry = contribute::spawn(contribute::Upload {
			id,
			device: self.adb.device(serial),
			package,
			name,
			target,
			work_dir: self
				.paths
				.cache_dir()
				.join("contribute")
				.join(format!("task-{id}")),
			fetcher: self.library.fetcher().clone(),
			lock: self.upload_lock.clone(),
			sink: self.sink.clone(),
		});
		self.tasks.insert(id, entry);
	}

	pub(super) fn start_downloads(&mut self) {
		let deps = self.download_deps();
		let limit = usize::from(self.settings.max_concurrent_downloads);
		for (id, abort) in self.queue.start_ready(limit, &deps) {
			if let Some(entry) = self.tasks.get_mut(&id) {
				entry.abort = Some(abort);
			}
		}
	}

	pub(super) fn review(&mut self, id: TaskId, decision: ReviewDecision) {
		if let Some(sender) = self.tasks.get_mut(&id).and_then(|t| t.review.take()) {
			let _ = sender.send(decision);
		}
	}

	pub(super) fn cancel(&mut self, id: TaskId) {
		let Some(task) = self.tasks.remove(&id) else {
			return;
		};
		let queued = self.queue.remove_pending(id);
		let running = task.abort.as_ref().is_some_and(|a| !a.is_finished());
		if let Some(abort) = &task.abort {
			abort.abort();
		}
		if queued || running {
			task.tracker.finish(
				TaskState::Cancelled,
				Some(t!("task.cancelled").into_owned()),
				None,
			);
		}
	}

	/// Only the kind of `tool` matters here; its path is filled in once the
	/// download finishes.
	pub(super) fn install_tool(&mut self, tool: Tool) {
		let id = self.new_task_id();
		let (kind, title, stage) = match tool {
			Tool::Adb(_) => (TaskKind::AdbSetup, t!("pt.title"), t!("pt.downloading")),
			Tool::Rclone(_) => (TaskKind::RcloneSetup, t!("rc.title"), t!("rc.downloading")),
		};
		let tracker = task::Tracker::new(
			TaskSnapshot {
				id,
				kind,
				serial: None,
				title: title.into_owned(),
				stage: stage.into_owned(),
				progress: None,
				detail: None,
				state: TaskState::Running,
				warnings: Vec::new(),
				output: None,
				conflict: None,
				broken_stream: None,
			},
			self.sink.clone(),
		);
		let (paths, done, t) = (
			self.paths.clone(),
			self.tool_installed.clone(),
			tracker.clone(),
		);
		let handle = tokio::spawn(async move {
			let progress = |n, total| t.bytes(n, total);
			let result = match tool {
				Tool::Adb(_) => {
					crate::platform_tools::install(&paths.platform_tools_dir(), &progress)
						.await
						.map(Tool::Adb)
				}
				Tool::Rclone(_) => crate::rclone_tool::install(&paths.rclone_bin_dir(), &progress)
					.await
					.map(Tool::Rclone),
			};
			match result {
				Ok(tool) => {
					let stage = match tool {
						Tool::Adb(_) => t!("pt.done"),
						Tool::Rclone(_) => t!("rc.done"),
					};
					t.finish(TaskState::Done, Some(stage.into_owned()), None);
					let _ = done.send(tool);
				}
				Err(e) => {
					tracing::error!("tool install failed: {e}");
					t.finish(TaskState::Failed(e), None, None);
				}
			}
		});
		self.tasks.insert(
			id,
			task::TaskEntry {
				abort: Some(handle.abort_handle()),
				review: None,
				tracker,
				release: None,
			},
		);
	}

	pub(super) fn restart_tracker(&mut self) {
		self.tracker.abort();
		self.adb = AdbServer::with_port(self.settings.adb_path.clone(), self.adb_port);
		self.tracker = devices::spawn_tracker(self.adb.clone(), self.sink.clone());
	}

	pub(super) async fn save_settings(&mut self, settings: Settings) {
		let paths = self.paths.clone();
		let to_save = settings.clone();
		let result = tokio::task::spawn_blocking(move || to_save.save(&paths)).await;
		match result {
			Ok(Ok(())) => {
				let adb_changed = settings.adb_path != self.settings.adb_path;
				let rclone_changed = settings.rclone_path != self.settings.rclone_path;
				let limit_changed = settings.download_limit != self.settings.download_limit
					|| settings.max_concurrent_downloads != self.settings.max_concurrent_downloads;
				let mirror_changed = settings.active_mirror() != self.settings.active_mirror();
				self.settings = settings.clone();
				self.sink.emit(Event::SettingsSaved(settings));
				if adb_changed {
					self.restart_tracker();
				}
				if limit_changed {
					self.library.set_bandwidth_limit(
						self.settings.download_limit.bytes_per_sec(),
						self.settings.max_concurrent_downloads,
					);
				}
				if rclone_changed {
					self.library
						.set_rclone_path(self.settings.rclone_path.clone());
				}
				self.start_downloads();
				if mirror_changed {
					self.library
						.activate(self.settings.active_mirror().cloned());
				}
			}
			Ok(Err(e)) => self.sink.emit(Event::SettingsSaveFailed(e.to_string())),
			Err(e) => self.sink.emit(Event::SettingsSaveFailed(format!(
				"settings task failed: {e}"
			))),
		}
	}
}
