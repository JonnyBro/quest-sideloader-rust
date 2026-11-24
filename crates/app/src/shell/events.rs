use super::*;

impl Shell {
	pub(super) fn drain_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let events: Vec<Event> = self.engine.poll_events().collect();
		for event in events {
			self.handle_event(event, window, cx);
		}
		cx.notify();
	}

	pub(super) fn handle_event(
		&mut self,
		event: Event,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		match event {
			Event::SettingsSaved(settings) => {
				crate::apply_language(settings.language);
				crate::theme::apply(&settings, window, cx);
				let for_view = settings.clone();
				let announce = self
					.settings_view
					.update(cx, |s, cx| s.on_saved(for_view, window, cx));
				let folder = settings.downloads_dir.clone();
				let for_rename = folder.clone();
				self.downloads.update(cx, |d, cx| d.set_folder(folder, cx));
				self.rename
					.update(cx, |r, cx| r.set_downloads_dir(for_rename, cx));
				self.settings = settings;
				self.sync_catalog_settings(cx);
				window.refresh();
				if announce {
					self.notify(Notification::info(t!("app.settings_saved")), window, cx);
				}
			}
			Event::SettingsSaveFailed(text) => {
				self.settings_view.update(cx, |s, cx| s.save_failed(cx));
				self.notify(Notification::error(text), window, cx)
			}
			Event::AdbStatus(status) => {
				match &status {
					AdbStatus::Missing(_) if !self.adb_prompted => {
						self.adb_prompted = true;
						tools::open_adb(self.engine.clone(), window, cx);
					}
					AdbStatus::Running { .. } if std::mem::take(&mut self.reconnect) => {
						if let Some(address) = &self.last_wireless {
							self.engine.send(Command::Reconnect(address.clone()));
						}
					}
					_ => {}
				}
				self.adb = Some(status);
				self.update_installed(cx);
			}
			Event::RcloneMissing => tools::open_rclone(self.engine.clone(), window, cx),
			Event::ScriptReview {
				task,
				title,
				analysis,
			} => {
				self.reviews.push_back(PendingReview {
					task,
					title,
					analysis,
				});
				self.show_next_review(window, cx);
			}
			Event::DevicesChanged(devices) => {
				// mDNS serials (`adb-...`) can't be connected to by address.
				if let Some(device) = devices.iter().find(|d| {
					d.state == DeviceState::Online
						&& d.serial.contains(':')
						&& !d.serial.starts_with("adb-")
				}) {
					self.last_wireless = Some(device.serial.clone());
				}
				self.devices = devices;
				let online = |serial: &str| {
					self.devices
						.iter()
						.any(|d| d.serial == serial && d.state == DeviceState::Online)
				};
				if !self.selected.as_deref().is_some_and(online) {
					let first = self
						.devices
						.iter()
						.find(|d| d.state == DeviceState::Online)
						.map(|d| d.serial.clone());
					self.select(first, cx);
				}
				self.update_installed(cx);
			}
			Event::DeviceInfo { serial, info } => match info {
				Ok(info) => {
					if self.selected.as_deref() == Some(serial.as_str()) {
						let info = info.clone();
						self.headset
							.update(cx, |h, cx| h.set_info(&serial, Some(info), cx));
					}
					self.infos.insert(serial, info);
					self.update_installed(cx);
				}
				Err(e) => self.notify(Notification::error(e), window, cx),
			},
			Event::MdnsServices(result) => {
				let connected = self.device_serials();
				if let Some(dialog) = self.wifi.as_ref().and_then(WeakEntity::upgrade) {
					dialog.update(cx, |d, cx| d.set_found(result, connected, cx));
				} else {
					self.wifi = None;
				}
			}
			Event::DirListing {
				serial,
				path,
				result,
			} => {
				self.headset
					.update(cx, |h, cx| h.set_listing(&serial, &path, result, cx));
			}
			Event::Leftovers { serial, result } => {
				// Removing folders frees space; the status bar and Device page show it.
				self.engine.send(Command::RefreshDevice(serial.clone()));
				self.headset
					.update(cx, |h, cx| h.set_leftovers(&serial, result, cx));
			}
			Event::HeadsetDetails { serial, details } => {
				if let Ok((d, _)) = &details {
					self.details.insert(serial.clone(), d.clone());
				}
				self.headset
					.update(cx, |h, cx| h.set_details(&serial, details, cx));
			}
			Event::CatalogLoaded(catalog) => {
				let for_headset = catalog.clone();
				self.headset
					.update(cx, |h, cx| h.set_catalog(for_headset, cx));
				let names = catalog.as_deref().map(game_names).unwrap_or_default();
				self.settings_view
					.update(cx, |s, cx| s.set_game_names(names, cx));
				let for_downloads = catalog.clone();
				self.downloads
					.update(cx, |d, cx| d.set_catalog(for_downloads, cx));
				self.catalog.update(cx, |c, cx| c.set_catalog(catalog, cx));
			}
			Event::MirrorHealth(health) => {
				self.catalog.update(cx, |c, cx| c.set_health(health, cx));
			}
			Event::CatalogStatus(status) => {
				self.catalog.update(cx, |c, cx| c.set_status(status, cx));
			}
			Event::Backups(backups) => {
				self.headset.update(cx, |h, cx| h.set_backups(backups, cx));
			}
			Event::BackupsChanged => self.engine.send(Command::ListBackups),
			Event::AppUpdate(release) => {
				self.app_update = Some(release);
				cx.notify();
			}
			Event::RenameAnalysis { id, result } => {
				self.rename
					.update(cx, |r, cx| r.on_analysis(id, result, window, cx));
			}
			Event::Inspection { id, result } => {
				self.inspector
					.update(cx, |i, cx| i.on_result(id, result, cx));
			}
			Event::Shell { id, output } => {
				self.console.update(cx, |c, cx| c.on_output(id, output, cx));
			}
			Event::Downloads(releases) => {
				self.downloads
					.update(cx, |d, cx| d.set_releases(releases, cx));
			}
			Event::TaskUpdated(task) => self.on_task(task, window, cx),
			Event::Notice(text) => self.notify(Notification::info(text), window, cx),
			Event::Error(text) => self.notify(Notification::error(text), window, cx),
		}
	}

	pub(super) fn on_task(
		&mut self,
		task: TaskSnapshot,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		let was_finished = self
			.tasks
			.iter()
			.find(|t| t.id == task.id)
			.is_some_and(|t| t.state.is_finished());
		let finished = task.state.is_finished() && !was_finished;
		if finished {
			self.forget_review(task.id, window, cx);
			self.notify_finished(&task, window);
		}
		let for_downloads = task.clone();
		self.downloads
			.update(cx, |d, cx| d.on_task(for_downloads, cx));
		if finished && (task.state == TaskState::Done || task.kind == TaskKind::Download) {
			// Downloads add release folders, or leave a part behind when they
			// stop; installs may remove folders.
			self.downloads.update(cx, |d, cx| d.invalidate(cx));
			if let Some(serial) = &task.serial {
				// Installs and uninstalls change the app list.
				self.engine.send(Command::RefreshDevice(serial.clone()));
				self.engine.send(Command::RefreshHeadset(serial.clone()));
			}
			if task.kind == TaskKind::Rename {
				self.reload_copies(cx);
				self.settings_view.update(cx, |s, cx| s.keys_changed(cx));
			}
		}
		match self.tasks.iter_mut().find(|t| t.id == task.id) {
			Some(slot) => *slot = task,
			None => self.tasks.push(task),
		}
		self.update_title(window);
	}

	/// A system notification when a task ends while the window is in the background.
	/// Copies are the packages with a key, so this follows the key folder.
	pub(super) fn reload_copies(&mut self, cx: &mut Context<Self>) {
		self.copies = KeyStore::new(self.paths.signing_dir())
			.packages()
			.into_iter()
			.collect();
		let copies = self.copies.clone();
		self.headset
			.update(cx, |h, cx| h.set_copies_and_notify(copies, cx));
		self.sync_catalog_settings(cx);
	}

	pub(super) fn notify_finished(&self, task: &TaskSnapshot, window: &Window) {
		if !self.settings.notifications || window.is_window_active() {
			return;
		}
		match &task.state {
			TaskState::Done => crate::notify::send(task.title.clone(), task.stage.clone()),
			TaskState::Failed(e) => crate::notify::send(
				t!("app.task_failed", title = task.title).into_owned(),
				e.chars().take(200).collect(),
			),
			_ => {}
		}
	}

	/// The running task's progress goes in front of the title, for the taskbar.
	pub(super) fn update_title(&mut self, window: &mut Window) {
		let base = crate::base_title(self.paths.portable_root().is_some());
		let title = match self.active_tasks().next() {
			Some(task) => match task.progress {
				Some(p) => format!("{:.0}% {} - {base}", p * 100., task.title),
				None => format!("{} - {base}", task.title),
			},
			None => base.to_owned(),
		};
		if title != self.title {
			window.set_window_title(&title);
			self.title = title;
		}
	}

	pub(super) fn show_next_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		if self.review_open.is_some() {
			return;
		}
		let Some(next) = self.reviews.pop_front() else {
			return;
		};
		self.review_open = Some(next.task);
		let shell = cx.entity().downgrade();
		review::open(
			self.engine.clone(),
			next,
			move |window, cx| {
				let _ = shell.update(cx, |this, cx| {
					this.review_open = None;
					// The next one waits until this dialog is gone.
					cx.defer_in(window, |this, window, cx| this.show_next_review(window, cx));
				});
			},
			window,
			cx,
		);
	}

	/// The task was cancelled or failed elsewhere; its question is moot.
	pub(super) fn forget_review(
		&mut self,
		task: TaskId,
		window: &mut Window,
		cx: &mut Context<Self>,
	) {
		self.reviews.retain(|r| r.task != task);
		if self.review_open == Some(task) {
			self.review_open = None;
			window.close_dialog(cx);
			self.show_next_review(window, cx);
		}
	}
}
