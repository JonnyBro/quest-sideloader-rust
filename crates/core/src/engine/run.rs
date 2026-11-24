use super::*;

/// Runs a short headset command and reports it in the status bar.
pub(super) fn quick(
	sink: &EventSink,
	done: String,
	fut: impl std::future::Future<Output = std::result::Result<(), crate::adb::AdbError>>
	+ Send
	+ 'static,
) {
	let sink = sink.clone();
	tokio::spawn(async move {
		match fut.await {
			Ok(()) => sink.emit(Event::Notice(done)),
			Err(e) => sink.emit(Event::Error(e.to_string())),
		}
	});
}

pub(super) async fn run(
	paths: AppPaths,
	settings: Settings,
	adb_port: u16,
	mut commands: mpsc::UnboundedReceiver<Command>,
	sink: EventSink,
) {
	tracing::info!("engine started");
	let adb = AdbServer::with_port(settings.adb_path.clone(), adb_port);
	let tracker = devices::spawn_tracker(adb.clone(), sink.clone());
	let mut library = library::Library::new(
		settings.rclone_path.clone(),
		paths.catalog_dir(),
		crate::mirror_key::MirrorKeys::official(Some(paths.mirror_key_file())),
		sink.clone(),
	);
	library.set_bandwidth_limit(
		settings.download_limit.bytes_per_sec(),
		settings.max_concurrent_downloads,
	);
	library.activate(settings.active_mirror().cloned());
	let (slot_tx, mut slot_rx) = mpsc::unbounded_channel();
	let (tool_tx, mut tool_rx) = mpsc::unbounded_channel::<Tool>();
	let mut state = State {
		paths,
		settings,
		sink,
		adb_port,
		adb,
		tracker,
		tasks: HashMap::new(),
		next_task: 0,
		device_locks: install::DeviceLocks::default(),
		library,
		queue: downloads::Queue::new(slot_tx),
		upload_lock: Arc::default(),
		tool_installed: tool_tx,
		shells: HashMap::new(),
	};

	loop {
		let command = tokio::select! {
			command = commands.recv() => match command {
				Some(command) => command,
				None => break,
			},
			Some(id) = slot_rx.recv() => {
				state.queue.slot_freed(id);
				state.start_downloads();
				continue;
			}
			Some(tool) = tool_rx.recv() => {
				let mut settings = state.settings.clone();
				match &tool {
					Tool::Adb(path) => settings.adb_path = Some(path.clone()),
					Tool::Rclone(path) => settings.rclone_path = Some(path.clone()),
				}
				state.save_settings(settings).await;
				if matches!(tool, Tool::Rclone(_)) {
					// Syncs only if the catalog is missing or stale.
					state.library.activate(state.settings.active_mirror().cloned());
				}
				continue;
			}
		};
		match command {
			Command::SaveSettings(settings) => state.save_settings(settings).await,
			Command::InstallAdb => state.install_tool(Tool::Adb(PathBuf::new())),
			Command::InstallRclone => state.install_tool(Tool::Rclone(PathBuf::new())),
			Command::Shell {
				id,
				serial,
				command,
			} => {
				state.shells.retain(|_, shell| !shell.is_finished());
				let device = state.adb.device(serial);
				let shell = console::spawn(id, device, command, state.sink.clone());
				state.shells.insert(id, shell);
			}
			Command::Inspect { id, path } => {
				let sink = state.sink.clone();
				let keys = crate::rename::KeyStore::new(state.paths.signing_dir());
				tokio::task::spawn_blocking(move || {
					let result = crate::rename::inspect(&path, &keys)
						.map(Box::new)
						.map_err(|e| e.to_string());
					sink.emit(Event::Inspection { id, result });
				});
			}
			Command::AnalyzeRename { id, path } => {
				let sink = state.sink.clone();
				tokio::task::spawn_blocking(move || {
					let result = crate::rename::analyze(&path).map_err(|e| e.to_string());
					sink.emit(Event::RenameAnalysis { id, result });
				});
			}
			Command::BuildCopy {
				source,
				new_package,
				loader,
				debuggable,
				title,
			} => {
				let id = state.new_task_id();
				let request = crate::rename::BuildRequest {
					source,
					new_package: new_package.clone(),
					loader,
					debuggable,
					dest_dir: state.settings.downloads_dir.clone(),
				};
				let keys = crate::rename::KeyStore::new(state.paths.signing_dir());
				let title = t!("ren.task_title", name = title, package = new_package).into_owned();
				let entry = rename::spawn(id, request, keys, title, state.sink.clone());
				state.tasks.insert(id, entry);
			}
			Command::StopShell(id) => {
				if let Some(shell) = state.shells.remove(&id) {
					shell.abort();
					state.sink.emit(Event::Shell {
						id,
						output: ShellEvent::Failed(t!("eng.shell_stopped").into_owned()),
					});
				}
			}
			Command::RefreshDevice(serial) => {
				devices::spawn_fetch_info(state.adb.device(serial), state.sink.clone());
			}
			Command::DiscoverWireless => {
				let (adb, sink) = (state.adb.clone(), state.sink.clone());
				tokio::spawn(async move {
					let result = adb.mdns_services().await.map_err(|e| e.to_string());
					sink.emit(Event::MdnsServices(result));
				});
			}
			Command::ConnectWireless(address) => {
				devices::spawn_connect(state.adb.clone(), address, false, state.sink.clone());
			}
			Command::Reconnect(address) => {
				devices::spawn_connect(state.adb.clone(), address, true, state.sink.clone());
			}
			Command::DisconnectWireless(address) => {
				devices::spawn_disconnect(state.adb.clone(), address, state.sink.clone());
			}
			Command::Install { serial, path } => state.start_install(serial, path),
			Command::ReviewScript { task, decision } => state.review(task, decision),
			Command::CancelTask(task) => state.cancel(task),
			Command::Download { game, serial } => state.enqueue_download(game, serial),
			Command::MoveDownload { task, delta } => state.queue.move_by(task, delta),
			Command::FindLeftovers(serial) => {
				let (device, sink) = (state.adb.device(serial.clone()), state.sink.clone());
				tokio::spawn(async move {
					let result = device.leftovers().await.map_err(|e| e.to_string());
					sink.emit(Event::Leftovers { serial, result });
				});
			}
			Command::RemoveLeftovers { serial, packages } => {
				let (device, sink) = (state.adb.device(serial.clone()), state.sink.clone());
				tokio::spawn(async move {
					if let Err(e) = device.remove_leftovers(&packages).await {
						sink.emit(Event::Error(e.to_string()));
					} else {
						sink.emit(Event::Notice(
							t!("eng.leftovers_removed", n = packages.len()).into_owned(),
						));
					}
					let result = device.leftovers().await.map_err(|e| e.to_string());
					sink.emit(Event::Leftovers { serial, result });
				});
			}
			Command::RefreshHeadset(serial) => {
				let (device, sink) = (state.adb.device(serial.clone()), state.sink.clone());
				tokio::spawn(async move {
					let (details, usage) = tokio::join!(device.details(), device.app_usage());
					let details = match (details, usage) {
						(Ok(d), Ok(u)) => Ok((d, u)),
						(Err(e), _) | (_, Err(e)) => Err(e.to_string()),
					};
					sink.emit(Event::HeadsetDetails { serial, details });
				});
			}
			Command::LaunchApp { serial, package } => {
				let device = state.adb.device(serial);
				quick(
					&state.sink,
					t!("eng.started", package = package).into_owned(),
					async move { device.launch(&package).await },
				);
			}
			Command::StopApp { serial, package } => {
				let device = state.adb.device(serial);
				quick(
					&state.sink,
					t!("eng.stopped", package = package).into_owned(),
					async move { device.force_stop(&package).await },
				);
			}
			Command::Uninstall {
				serial,
				app,
				back_up,
				remove_obb,
			} => {
				let id = state.new_task_id();
				let entry = headset::spawn_uninstall(
					id,
					state.adb.device(serial),
					state.paths.backups_dir(),
					app.into(),
					headset::UninstallOptions {
						back_up,
						remove_obb,
					},
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::ListDir { serial, path } => {
				files::spawn_list(state.adb.device(serial), path, state.sink.clone());
			}
			Command::PullFiles {
				serial,
				paths,
				dest,
			} => {
				let id = state.new_task_id();
				let entry = files::spawn_pull(
					id,
					state.adb.device(serial),
					paths,
					dest,
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::PushFiles { serial, local, dir } => {
				let id = state.new_task_id();
				let entry =
					files::spawn_push(id, state.adb.device(serial), local, dir, state.sink.clone());
				state.tasks.insert(id, entry);
			}
			Command::MakeDir { serial, dir, name } => {
				files::spawn_make_dir(state.adb.device(serial), dir, name, state.sink.clone());
			}
			Command::RenameFile {
				serial,
				dir,
				from,
				to,
			} => {
				files::spawn_rename(state.adb.device(serial), dir, from, to, state.sink.clone());
			}
			Command::PasteFiles {
				serial,
				paths,
				dir,
				copy,
			} => {
				let id = state.new_task_id();
				let entry = files::spawn_paste(
					id,
					state.adb.device(serial),
					paths,
					dir,
					copy,
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::DeleteFiles { serial, paths, dir } => {
				files::spawn_delete(state.adb.device(serial), paths, dir, state.sink.clone());
			}
			Command::BackUpSaves { serial, app } => {
				let id = state.new_task_id();
				let entry = headset::spawn_backup(
					id,
					state.adb.device(serial),
					state.paths.backups_dir(),
					app.into(),
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::RestoreBackup {
				serial,
				backup,
				package,
			} => {
				let id = state.new_task_id();
				let entry = headset::spawn_restore(
					id,
					state.adb.device(serial),
					backup,
					package,
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::DeleteBackup(backup) => {
				let sink = state.sink.clone();
				tokio::task::spawn_blocking(move || match crate::saves::delete(&backup) {
					Ok(()) => sink.emit(Event::BackupsChanged),
					Err(e) => sink.emit(Event::Error(e.to_string())),
				});
			}
			Command::CheckAppUpdate => {
				let sink = state.sink.clone();
				tokio::spawn(async move {
					match crate::app_update::check().await {
						Ok(Some(release)) => {
							tracing::info!("version {} is out: {}", release.version, release.url);
							sink.emit(Event::AppUpdate(release));
						}
						Ok(None) => tracing::debug!("the app is up to date"),
						Err(e) => tracing::warn!("couldn't check for a new version: {e}"),
					}
				});
			}
			Command::ListBackups => {
				let (root, sink) = (state.paths.backups_dir(), state.sink.clone());
				tokio::task::spawn_blocking(move || {
					sink.emit(Event::Backups(crate::saves::list(&root)))
				});
			}
			Command::ListDownloads => {
				let (root, sink) = (state.settings.downloads_dir.clone(), state.sink.clone());
				tokio::task::spawn_blocking(move || {
					sink.emit(Event::Downloads(crate::downloaded::list(&root)))
				});
			}
			Command::DeleteDownload { name, incomplete } => {
				let (root, sink) = (state.settings.downloads_dir.clone(), state.sink.clone());
				tokio::task::spawn_blocking(move || {
					if let Err(e) = crate::downloaded::delete(&root, &name, incomplete) {
						sink.emit(Event::Error(
							t!("eng.delete_failed", name = name, error = e).into_owned(),
						));
					}
					sink.emit(Event::Downloads(crate::downloaded::list(&root)));
				});
			}
			Command::ReinstallKeepingSaves {
				serial,
				app,
				source,
				remove_source,
			} => {
				let id = state.new_task_id();
				let ctx = install::InstallContext {
					id,
					device: state.adb.device(serial),
					work_dir: state.paths.cache_dir().join("sideload"),
					locks: state.device_locks.clone(),
					sink: state.sink.clone(),
					title_from_files: false,
					backups: None,
				};
				let entry = headset::spawn_reinstall(
					ctx,
					state.paths.backups_dir(),
					app.into(),
					source,
					remove_source,
					state.sink.clone(),
				);
				state.tasks.insert(id, entry);
			}
			Command::Reboot { serial, mode } => {
				let device = state.adb.device(serial);
				quick(&state.sink, t!("eng.restarting").into_owned(), async move {
					device.reboot(mode).await
				});
			}
			Command::EnableWireless(serial) => {
				let (server, sink) = (state.adb.clone(), state.sink.clone());
				tokio::spawn(async move {
					let device = server.device(serial);
					let result = async {
						let ip = device.wifi_ip().await?.ok_or_else(|| {
							crate::adb::AdbError::Server(t!("eng.no_wifi").into_owned())
						})?;
						device.enable_tcpip(5555).await?;
						// adbd restarts in TCP mode before it accepts connections.
						tokio::time::sleep(std::time::Duration::from_secs(3)).await;
						server.connect_wireless(&format!("{ip}:5555")).await
					}
					.await;
					match result {
						Ok(message) => sink.emit(Event::Notice(message)),
						Err(e) => sink.emit(Event::Error(
							t!("eng.wireless_failed", error = e).into_owned(),
						)),
					}
				});
			}
			Command::SetMtp { serial, enabled } => {
				let device = state.adb.device(serial);
				let done = if enabled {
					t!("eng.mtp_on")
				} else {
					t!("eng.mtp_off")
				};
				quick(&state.sink, done.into_owned(), async move {
					device.set_mtp(enabled).await
				});
			}
			Command::SetAppSafety { serial, enabled } => {
				let (device, sink) = (state.adb.device(serial), state.sink.clone());
				tokio::spawn(async move {
					match device.set_app_safety(enabled).await {
						Ok(checks) => {
							let list = checks
								.iter()
								.map(|c| format!("{} = {}", c.name, c.value))
								.collect::<Vec<_>>()
								.join(", ");
							if checks.iter().all(|c| c.applied) {
								let done = if enabled {
									t!("eng.app_safety_on", checks = list)
								} else {
									t!("eng.app_safety_off", checks = list)
								};
								sink.emit(Event::Notice(done.into_owned()));
							} else {
								sink.emit(Event::Error(
									t!("eng.app_safety_partly", checks = list).into_owned(),
								));
							}
						}
						Err(e) => sink.emit(Event::Error(
							t!("eng.app_safety_failed", error = e).into_owned(),
						)),
					}
				});
			}
			Command::MirrorScreen { serial, eye } => {
				let (adb, sink) = (state.adb.clone(), state.sink.clone());
				tokio::spawn(async move {
					let title = format!("Quest Sideloader: {serial}");
					if let Err(e) = crate::scrcpy::run(&adb, &serial, &title, eye).await {
						sink.emit(Event::Error(
							t!("eng.scrcpy_failed", error = e).into_owned(),
						));
					}
				});
			}
			Command::Screenshot(serial) => {
				let (device, sink, dir) = (
					state.adb.device(serial.clone()),
					state.sink.clone(),
					state.paths.screenshots_dir(),
				);
				tokio::spawn(async move {
					let result = async {
						let png = device.screenshot().await.map_err(|e| e.to_string())?;
						let stamp = std::time::SystemTime::now()
							.duration_since(std::time::UNIX_EPOCH)
							.map_or(0, |d| d.as_secs());
						tokio::fs::create_dir_all(&dir)
							.await
							.map_err(|e| e.to_string())?;
						let path = dir.join(format!("quest-{stamp}.png"));
						tokio::fs::write(&path, png)
							.await
							.map_err(|e| e.to_string())?;
						Ok::<_, String>(path)
					}
					.await;
					match result {
						Ok(path) => sink.emit(Event::Notice(
							t!("eng.screenshot_saved", path = path.display()).into_owned(),
						)),
						Err(e) => sink.emit(Event::Error(
							t!("eng.screenshot_failed", error = e).into_owned(),
						)),
					}
				});
			}
			Command::SetTweak {
				serial,
				tweak,
				value,
			} => {
				let device = state.adb.device(serial);
				let done = if value.is_empty() {
					t!("eng.tweak_reset", property = tweak.property()).into_owned()
				} else {
					t!("eng.tweak_set", property = tweak.property(), value = value).into_owned()
				};
				quick(&state.sink, done, async move {
					device.set_tweak(tweak, &value).await
				});
			}
			Command::IgnoreContribution(package) => {
				let mut settings = state.settings.clone();
				if !settings.contribute_ignored.contains(&package) {
					settings.contribute_ignored.push(package);
					state.save_settings(settings).await;
				}
			}
			Command::HoldUpdates { package, hold } => {
				let mut settings = state.settings.clone();
				let held = settings.update_ignored.contains(&package);
				if hold && !held {
					settings.update_ignored.push(package);
					state.save_settings(settings).await;
				} else if !hold && held {
					settings.update_ignored.retain(|p| p != &package);
					state.save_settings(settings).await;
				}
			}
			Command::Contribute {
				serial,
				package,
				name,
			} => state.start_contribution(serial, package, name),
			Command::SyncCatalog => {
				let mirror = state.settings.active_mirror().cloned();
				state.library.sync(mirror);
			}
		}
	}
	state.tracker.abort();
	state.library.abort();
	state.queue.abort_all();
	for abort in state.tasks.values().filter_map(|t| t.abort.as_ref()) {
		abort.abort();
	}
	tracing::info!("engine stopped");
}
