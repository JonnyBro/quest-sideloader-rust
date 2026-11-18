//! Background side of the app: the UI sends [`Command`]s, the engine works on a
//! tokio runtime in its own thread and reports back with [`Event`]s.

mod contribute;
mod devices;
mod downloads;
mod headset;
mod install;
mod library;
mod task;

use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use tokio::sync::mpsc;

use crate::adb::{AdbServer, DeviceEntry, DeviceInfo};
use crate::catalog::{Catalog, Game};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::settings::Settings;
use crate::sideload::script::ScriptAnalysis;

pub use install::ReviewDecision;
pub use library::CatalogStatus;
pub use task::{SignatureConflict, TaskId, TaskKind, TaskSnapshot, TaskState};

#[derive(Debug)]
pub enum Command {
	SaveSettings(Settings),
	/// Read the details and per-app storage the headset tab shows.
	RefreshHeadset(String),
	LaunchApp {
		serial: String,
		package: String,
	},
	StopApp {
		serial: String,
		package: String,
	},
	Uninstall {
		serial: String,
		app: AppInfo,
		back_up: bool,
		remove_obb: bool,
	},
	BackUpSaves {
		serial: String,
		app: AppInfo,
	},
	RestoreBackup {
		serial: String,
		backup: crate::saves::Backup,
	},
	DeleteBackup(crate::saves::Backup),
	ListBackups,
	/// Answered with [`Event::Downloads`].
	ListDownloads,
	/// Delete a release folder from the downloads folder by name.
	DeleteDownload(String),
	/// After a signature mismatch: back up saves, uninstall, install again, restore.
	ReinstallKeepingSaves {
		serial: String,
		app: AppInfo,
		source: PathBuf,
	},
	Reboot {
		serial: String,
		mode: crate::adb::RebootMode,
	},
	/// Turn on wireless ADB and connect to the headset over Wi-Fi.
	EnableWireless(String),
	Screenshot(String),
	SetTweak {
		serial: String,
		tweak: crate::adb::Tweak,
		value: String,
	},
	RefreshDevice(String),
	/// `host` or `host:port`.
	ConnectWireless(String),
	DisconnectWireless(String),
	/// Install an APK, a release folder or a ZIP on the headset.
	Install {
		serial: String,
		path: PathBuf,
	},
	ReviewScript {
		task: TaskId,
		decision: ReviewDecision,
	},
	CancelTask(TaskId),
	/// Download a release from the active mirror; with `serial`, install it
	/// on that headset afterwards.
	Download {
		game: Game,
		serial: Option<String>,
	},
	/// Upload an app from the headset to the active mirror. Only send this
	/// after the user agreed to share that app.
	Contribute {
		serial: String,
		package: String,
		name: String,
	},
	/// Stop offering this package for upload.
	IgnoreContribution(String),
	/// Moves a queued download by `delta` places (negative is earlier).
	MoveDownload {
		task: TaskId,
		delta: isize,
	},
	/// Download the active mirror's metadata again.
	SyncCatalog,
	/// Download adb from Google into the data folder and use it. Only send
	/// this after the user agreed to the Android SDK terms.
	InstallAdb,
	/// Download rclone from rclone.org into the data folder and use it.
	InstallRclone,
}

/// An app on the headset, as the UI knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
	pub package: String,
	pub name: String,
	pub version_code: Option<u64>,
}

impl From<AppInfo> for headset::AppRef {
	fn from(app: AppInfo) -> Self {
		Self {
			package: app.package,
			name: app.name,
			version_code: app.version_code,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbStatus {
	Starting,
	Running {
		version: u32,
	},
	Unavailable(String),
	/// The adb binary isn't there; it can be installed with [`Command::InstallAdb`].
	Missing(String),
}

#[derive(Debug)]
pub enum Event {
	SettingsSaved(Settings),
	SettingsSaveFailed(String),
	AdbStatus(AdbStatus),
	DevicesChanged(Vec<DeviceEntry>),
	DeviceInfo {
		serial: String,
		info: std::result::Result<DeviceInfo, String>,
	},
	TaskUpdated(TaskSnapshot),
	/// The active mirror's catalog; `None` when there is no mirror or it was never synced.
	CatalogLoaded(Option<Arc<Catalog>>),
	CatalogStatus(CatalogStatus),
	/// install.txt has lines that need the user's decision; answer with
	/// [`Command::ReviewScript`].
	ScriptReview {
		task: TaskId,
		title: String,
		analysis: ScriptAnalysis,
	},
	HeadsetDetails {
		serial: String,
		details: std::result::Result<
			(
				crate::adb::DeviceDetails,
				HashMap<String, crate::adb::AppUsage>,
			),
			String,
		>,
	},
	Backups(Vec<crate::saves::Backup>),
	/// Something changed the stored backups; ask again with [`Command::ListBackups`].
	BackupsChanged,
	/// A sync failed because there is no rclone; it can be installed with
	/// [`Command::InstallRclone`].
	RcloneMissing,
	/// Release folders in the downloads folder.
	Downloads(Vec<crate::downloaded::DownloadedRelease>),
	Notice(String),
	Error(String),
}

/// Called after every event so the UI can wake up and repaint.
pub type Notifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub(crate) struct EventSink {
	tx: std_mpsc::Sender<Event>,
	notify: Notifier,
}

impl EventSink {
	pub(crate) fn emit(&self, event: Event) {
		if self.tx.send(event).is_ok() {
			(self.notify)();
		}
	}
}

pub struct Engine {
	commands: Option<mpsc::UnboundedSender<Command>>,
	events: std_mpsc::Receiver<Event>,
	thread: Option<JoinHandle<()>>,
}

impl Engine {
	pub fn start(paths: AppPaths, settings: Settings, notify: Notifier) -> Result<Self> {
		Self::start_with_adb_port(paths, settings, notify, crate::adb::DEFAULT_PORT)
	}

	fn start_with_adb_port(
		paths: AppPaths,
		settings: Settings,
		notify: Notifier,
		adb_port: u16,
	) -> Result<Self> {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.thread_name("engine-worker")
			.build()
			.map_err(Error::Runtime)?;

		let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
		let (event_tx, event_rx) = std_mpsc::channel();
		let sink = EventSink {
			tx: event_tx,
			notify,
		};

		let thread = std::thread::Builder::new()
			.name("engine".into())
			.spawn(move || runtime.block_on(run(paths, settings, adb_port, cmd_rx, sink)))
			.map_err(Error::Runtime)?;

		Ok(Self {
			commands: Some(cmd_tx),
			events: event_rx,
			thread: Some(thread),
		})
	}

	pub fn send(&self, command: Command) {
		if let Some(tx) = &self.commands
			&& tx.send(command).is_err()
		{
			tracing::error!("engine is not running, command dropped");
		}
	}

	pub fn poll_events(&self) -> impl Iterator<Item = Event> + '_ {
		self.events.try_iter()
	}
}

impl Drop for Engine {
	fn drop(&mut self) {
		// Closing the channel ends the command loop, which lets the thread exit.
		self.commands.take();
		if let Some(thread) = self.thread.take()
			&& thread.join().is_err()
		{
			tracing::error!("engine thread panicked");
		}
	}
}

/// Mirrors forbid bulk downloading; a short queue keeps the app on the right side of that.
const MAX_DOWNLOAD_JOBS: usize = 10;

struct State {
	paths: AppPaths,
	settings: Settings,
	sink: EventSink,
	adb_port: u16,
	adb: AdbServer,
	tracker: tokio::task::JoinHandle<()>,
	tasks: HashMap<TaskId, task::TaskEntry>,
	next_task: TaskId,
	device_locks: install::DeviceLocks,
	library: library::Library,
	queue: downloads::Queue,
	upload_lock: Arc<tokio::sync::Mutex<()>>,
	/// Reports a tool that finished downloading.
	tool_installed: mpsc::UnboundedSender<Tool>,
}

impl State {
	fn new_task_id(&mut self) -> TaskId {
		self.tasks
			.retain(|_, t| t.abort.as_ref().is_none_or(|a| !a.is_finished()));
		self.next_task += 1;
		self.next_task
	}

	fn start_install(&mut self, serial: String, path: PathBuf) {
		let id = self.new_task_id();
		let ctx = install::InstallContext {
			id,
			device: self.adb.device(serial),
			work_dir: self.paths.cache_dir().join("sideload"),
			locks: self.device_locks.clone(),
			sink: self.sink.clone(),
			title_from_files: true,
		};
		self.tasks.insert(id, install::spawn(ctx, path));
	}

	fn download_deps(&self) -> downloads::Deps {
		downloads::Deps {
			fetcher: self.library.fetcher().clone(),
			adb: self.adb.clone(),
			locks: self.device_locks.clone(),
			sink: self.sink.clone(),
			work_dir: self.paths.cache_dir().join("sideload"),
			downloads_dir: self.settings.downloads_dir.clone(),
			keep_downloads: self.settings.keep_downloads,
		}
	}

	fn enqueue_download(&mut self, game: Game, serial: Option<String>) {
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
		};
		let (review, tracker) = self.queue.push(job, self.sink.clone());
		self.tasks
			.insert(id, downloads::entry(release, review, tracker));
		self.start_downloads();
	}

	fn start_contribution(&mut self, serial: String, package: String, name: String) {
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

	fn start_downloads(&mut self) {
		let deps = self.download_deps();
		let limit = usize::from(self.settings.max_concurrent_downloads);
		for (id, abort) in self.queue.start_ready(limit, &deps) {
			if let Some(entry) = self.tasks.get_mut(&id) {
				entry.abort = Some(abort);
			}
		}
	}

	fn review(&mut self, id: TaskId, decision: ReviewDecision) {
		if let Some(sender) = self.tasks.get_mut(&id).and_then(|t| t.review.take()) {
			let _ = sender.send(decision);
		}
	}

	fn cancel(&mut self, id: TaskId) {
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
	fn install_tool(&mut self, tool: Tool) {
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

	fn restart_tracker(&mut self) {
		self.tracker.abort();
		self.adb = AdbServer::with_port(self.settings.adb_path.clone(), self.adb_port);
		self.tracker = devices::spawn_tracker(self.adb.clone(), self.sink.clone());
	}

	async fn save_settings(&mut self, settings: Settings) {
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

/// A program the app can download for the user.
#[derive(Debug)]
enum Tool {
	Adb(PathBuf),
	Rclone(PathBuf),
}

/// Runs a short headset command and reports it in the status bar.
fn quick(
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

async fn run(
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
			Command::RefreshDevice(serial) => {
				devices::spawn_fetch_info(state.adb.device(serial), state.sink.clone());
			}
			Command::ConnectWireless(address) => {
				devices::spawn_connect(state.adb.clone(), address, state.sink.clone());
			}
			Command::DisconnectWireless(address) => {
				devices::spawn_disconnect(state.adb.clone(), address, state.sink.clone());
			}
			Command::Install { serial, path } => state.start_install(serial, path),
			Command::ReviewScript { task, decision } => state.review(task, decision),
			Command::CancelTask(task) => state.cancel(task),
			Command::Download { game, serial } => state.enqueue_download(game, serial),
			Command::MoveDownload { task, delta } => state.queue.move_by(task, delta),
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
			Command::RestoreBackup { serial, backup } => {
				let id = state.new_task_id();
				let entry = headset::spawn_restore(
					id,
					state.adb.device(serial),
					backup,
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
			Command::DeleteDownload(name) => {
				let (root, sink) = (state.settings.downloads_dir.clone(), state.sink.clone());
				tokio::task::spawn_blocking(move || {
					if let Err(e) = crate::downloaded::delete(&root, &name) {
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
			} => {
				let id = state.new_task_id();
				let ctx = install::InstallContext {
					id,
					device: state.adb.device(serial),
					work_dir: state.paths.cache_dir().join("sideload"),
					locks: state.device_locks.clone(),
					sink: state.sink.clone(),
					title_from_files: false,
				};
				let entry = headset::spawn_reinstall(
					ctx,
					state.paths.backups_dir(),
					app.into(),
					source,
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

#[cfg(test)]
mod tests {
	use std::time::{Duration, Instant};

	use super::*;

	#[test]
	fn save_settings_roundtrip_through_engine() {
		let dir = tempfile::tempdir().unwrap();
		let paths = AppPaths::under(dir.path());
		let mut initial = Settings::defaults_for(&paths);
		// A free port and a missing binary keep the test away from a real adb
		// server and from any headset connected to it.
		initial.adb_path = Some(dir.path().join("no-adb"));
		let free_port = std::net::TcpListener::bind("127.0.0.1:0")
			.unwrap()
			.local_addr()
			.unwrap()
			.port();
		let engine =
			Engine::start_with_adb_port(paths.clone(), initial.clone(), Arc::new(|| {}), free_port)
				.unwrap();

		let mut settings = initial;
		settings.max_concurrent_downloads = 3;
		engine.send(Command::SaveSettings(settings.clone()));

		let deadline = Instant::now() + Duration::from_secs(5);
		let saved = loop {
			assert!(Instant::now() < deadline, "no SettingsSaved event");
			if let Some(s) = engine.poll_events().find_map(|e| match e {
				Event::SettingsSaved(s) => Some(s),
				_ => None,
			}) {
				break s;
			}
			std::thread::sleep(Duration::from_millis(10));
		};
		assert_eq!(saved, settings);

		let unavailable = loop {
			assert!(Instant::now() < deadline, "no AdbStatus::Unavailable event");
			if let Some(msg) = engine.poll_events().find_map(|e| match e {
				Event::AdbStatus(AdbStatus::Missing(msg)) => Some(msg),
				_ => None,
			}) {
				break msg;
			}
			std::thread::sleep(Duration::from_millis(10));
		};
		assert!(unavailable.contains("not found"), "{unavailable}");
		assert_eq!(Settings::load(&paths).unwrap(), settings);
	}
}
