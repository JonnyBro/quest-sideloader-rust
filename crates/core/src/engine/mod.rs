//! Background side of the app: the UI sends [`Command`]s, the engine works on a
//! tokio runtime in its own thread and reports back with [`Event`]s.

mod devices;
mod downloads;
mod install;
mod library;
mod task;

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
pub use task::{TaskId, TaskKind, TaskSnapshot, TaskState};

#[derive(Debug)]
pub enum Command {
	SaveSettings(Settings),
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
	/// Moves a queued download by `delta` places (negative is earlier).
	MoveDownload {
		task: TaskId,
		delta: isize,
	},
	/// Download the active mirror's metadata again.
	SyncCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbStatus {
	Starting,
	Running { version: u32 },
	Unavailable(String),
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
			self.sink.emit(Event::Error(
				"No mirror configured. Add one in Settings.".into(),
			));
			return;
		};
		let unfinished = |t: &&task::TaskEntry| {
			t.release.is_some() && t.abort.as_ref().is_none_or(|a| !a.is_finished())
		};
		if self.tasks.values().filter(unfinished).count() >= MAX_DOWNLOAD_JOBS {
			self.sink.emit(Event::Notice(format!(
				"The download queue is full ({MAX_DOWNLOAD_JOBS}); wait for a download to finish"
			)));
			return;
		}
		let active = self
			.tasks
			.values()
			.filter(unfinished)
			.any(|t| t.release.as_deref() == Some(game.release_name.as_str()));
		if active {
			self.sink.emit(Event::Notice(format!(
				"{} is already in the queue",
				game.name
			)));
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
			task.tracker
				.finish(TaskState::Cancelled, Some("Cancelled".into()), None);
		}
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
		};
		match command {
			Command::SaveSettings(settings) => state.save_settings(settings).await,
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
				Event::AdbStatus(AdbStatus::Unavailable(msg)) => Some(msg),
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
