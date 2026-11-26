//! Background side of the app: the UI sends [`Command`]s, the engine works on a
//! tokio runtime in its own thread and reports back with [`Event`]s.

mod console;
mod contribute;
mod devices;
mod downloads;
mod files;
mod headset;
mod install;
mod library;
mod messages;
mod rename;
mod run;
mod state;
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

pub use console::ShellEvent;
pub use install::ReviewDecision;
pub use library::CatalogStatus;
pub use messages::{AdbStatus, AppInfo, Command, Event};
pub use task::{BrokenStream, SignatureConflict, TaskId, TaskKind, TaskSnapshot, TaskState};

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
			.spawn(move || runtime.block_on(run::run(paths, settings, adb_port, cmd_rx, sink)))
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

/// Mirrors forbid bulk downloading, so the queue stays short.
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
	tool_installed: mpsc::UnboundedSender<Tool>,
	shells: HashMap<u64, tokio::task::AbortHandle>,
}

impl State {}

#[derive(Debug)]
enum Tool {
	Adb(PathBuf),
	Rclone(PathBuf),
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
