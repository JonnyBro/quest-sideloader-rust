use std::collections::HashSet;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::{AdbStatus, Event, EventSink};
use crate::adb::{AdbServer, Device, DeviceState};

const MAX_BACKOFF: Duration = Duration::from_secs(30);

pub(super) fn spawn_tracker(server: AdbServer, sink: EventSink) -> JoinHandle<()> {
	tokio::spawn(track(server, sink))
}

/// Keeps a `track-devices` connection open and re-establishes it (restarting the
/// adb server if needed) whenever it drops.
async fn track(server: AdbServer, sink: EventSink) {
	let mut backoff = Duration::from_secs(1);
	loop {
		sink.emit(Event::AdbStatus(AdbStatus::Starting));
		let tracker = match server.ensure_running().await {
			Ok(version) => match server.track_devices().await {
				Ok(tracker) => {
					sink.emit(Event::AdbStatus(AdbStatus::Running { version }));
					Some(tracker)
				}
				Err(e) => {
					sink.emit(Event::AdbStatus(AdbStatus::Unavailable(e.to_string())));
					None
				}
			},
			Err(e) => {
				sink.emit(Event::AdbStatus(AdbStatus::Unavailable(e.to_string())));
				None
			}
		};

		let Some(mut tracker) = tracker else {
			sink.emit(Event::DevicesChanged(Vec::new()));
			tokio::time::sleep(backoff).await;
			backoff = (backoff * 2).min(MAX_BACKOFF);
			continue;
		};
		backoff = Duration::from_secs(1);

		let mut online = HashSet::new();
		loop {
			match tracker.next().await {
				Ok(devices) => {
					let now_online: HashSet<String> = devices
						.iter()
						.filter(|d| d.state == DeviceState::Online)
						.map(|d| d.serial.clone())
						.collect();
					for serial in now_online.difference(&online) {
						spawn_fetch_info(server.device(serial.clone()), sink.clone());
					}
					online = now_online;
					sink.emit(Event::DevicesChanged(devices));
				}
				Err(e) => {
					tracing::warn!("device tracking stopped: {e}");
					break;
				}
			}
		}
		sink.emit(Event::DevicesChanged(Vec::new()));
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
}

pub(super) fn spawn_fetch_info(device: Device, sink: EventSink) {
	tokio::spawn(async move {
		let info = device.info().await.map_err(|e| e.to_string());
		if let Err(e) = &info {
			tracing::warn!("failed to read info for {}: {e}", device.serial());
		}
		sink.emit(Event::DeviceInfo {
			serial: device.serial().to_owned(),
			info,
		});
	});
}

pub(super) fn spawn_connect(server: AdbServer, address: String, sink: EventSink) {
	tokio::spawn(async move {
		match server.connect_wireless(&address).await {
			Ok(message) => sink.emit(Event::Notice(message)),
			Err(e) => sink.emit(Event::Error(format!("connect {address}: {e}"))),
		}
	});
}

pub(super) fn spawn_disconnect(server: AdbServer, address: String, sink: EventSink) {
	tokio::spawn(async move {
		match server.disconnect_wireless(&address).await {
			Ok(message) => sink.emit(Event::Notice(message)),
			Err(e) => sink.emit(Event::Error(format!("disconnect {address}: {e}"))),
		}
	});
}
