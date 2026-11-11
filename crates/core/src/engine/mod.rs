//! Background side of the app: the UI sends [`Command`]s, the engine works on a
//! tokio runtime in its own thread and reports back with [`Event`]s.

mod devices;

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use tokio::sync::mpsc;

use crate::adb::{AdbServer, DeviceEntry, DeviceInfo};
use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::settings::Settings;

#[derive(Debug)]
pub enum Command {
	SaveSettings(Settings),
	RefreshDevice(String),
	/// `host` or `host:port`.
	ConnectWireless(String),
	DisconnectWireless(String),
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

struct State {
	paths: AppPaths,
	settings: Settings,
	sink: EventSink,
	adb_port: u16,
	adb: AdbServer,
	tracker: tokio::task::JoinHandle<()>,
}

impl State {
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
				self.settings = settings.clone();
				self.sink.emit(Event::SettingsSaved(settings));
				if adb_changed {
					self.restart_tracker();
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
	let mut state = State {
		paths,
		settings,
		sink,
		adb_port,
		adb,
		tracker,
	};

	while let Some(command) = commands.recv().await {
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
		}
	}
	state.tracker.abort();
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
