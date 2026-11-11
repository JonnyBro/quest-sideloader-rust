//! Background side of the app: the UI sends [`Command`]s, the engine works on a
//! tokio runtime in its own thread and reports back with [`Event`]s.

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::settings::Settings;

#[derive(Debug)]
pub enum Command {
    SaveSettings(Settings),
}

#[derive(Debug)]
pub enum Event {
    SettingsSaved(Settings),
    Error(String),
}

/// Called after every event so the UI can wake up and repaint.
pub type Notifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct EventSink {
    tx: std_mpsc::Sender<Event>,
    notify: Notifier,
}

impl EventSink {
    fn emit(&self, event: Event) {
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
    pub fn start(paths: AppPaths, notify: Notifier) -> Result<Self> {
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
            .spawn(move || runtime.block_on(run(paths, cmd_rx, sink)))
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

async fn run(paths: AppPaths, mut commands: mpsc::UnboundedReceiver<Command>, sink: EventSink) {
    tracing::info!("engine started");
    while let Some(command) = commands.recv().await {
        match command {
            Command::SaveSettings(settings) => {
                let paths = paths.clone();
                let sink = sink.clone();
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        settings.save(&paths).map(|()| settings)
                    })
                    .await;
                    match result {
                        Ok(Ok(settings)) => sink.emit(Event::SettingsSaved(settings)),
                        Ok(Err(e)) => sink.emit(Event::Error(e.to_string())),
                        Err(e) => sink.emit(Event::Error(format!("settings task failed: {e}"))),
                    }
                });
            }
        }
    }
    tracing::info!("engine stopped");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn save_settings_roundtrip_through_engine() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(dir.path());
        let (wake_tx, wake_rx) = std_mpsc::channel();
        let notify: Notifier = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let engine = Engine::start(paths.clone(), notify).unwrap();

        let mut settings = Settings::defaults_for(&paths);
        settings.max_concurrent_downloads = 3;
        engine.send(Command::SaveSettings(settings.clone()));

        wake_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let events: Vec<_> = engine.poll_events().collect();
        assert!(matches!(&events[..], [Event::SettingsSaved(s)] if *s == settings));
        assert_eq!(Settings::load(&paths).unwrap(), settings);
    }
}
