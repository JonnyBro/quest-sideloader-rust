//! Dev tool: `cargo run -p quest-sideloader-core --example adb_probe`.

use quest_sideloader_core::adb::{AdbServer, DeviceState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let server = AdbServer::new(std::env::var_os("ADB").map(Into::into));
	println!(
		"adb server protocol version: {}",
		server.ensure_running().await?
	);
	let devices = server.devices().await?;
	println!("{} device(s)", devices.len());
	for entry in devices {
		println!(
			"{} {:?} {}",
			entry.serial,
			entry.state,
			entry.display_name()
		);
		if entry.state == DeviceState::Online {
			println!("{:#?}", server.device(&entry.serial).info().await?);
		}
	}
	Ok(())
}
