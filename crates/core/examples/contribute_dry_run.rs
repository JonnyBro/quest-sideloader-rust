//! Dev tool: `cargo run -p quest-sideloader-core --example contribute_dry_run -- <package> <out dir>`.
//! Prepares an upload archive from the first connected headset without sending
//! it anywhere. Only reads from the headset.

use std::path::PathBuf;

use quest_sideloader_core::adb::{AdbServer, DeviceState};
use quest_sideloader_core::contribute::{archive_name, hwid, pack};
use quest_sideloader_core::sideload::read_apk_meta;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let mut args = std::env::args().skip(1);
	let package = args
		.next()
		.ok_or("usage: contribute_dry_run <package> <out dir>")?;
	let out: PathBuf = args.next().ok_or("missing <out dir>")?.into();

	let server = AdbServer::new(None);
	server.ensure_running().await?;
	let entry = server
		.devices()
		.await?
		.into_iter()
		.find(|d| d.state == DeviceState::Online)
		.ok_or("no online headset")?;
	let device = server.device(entry.serial.clone());

	let apk = device.apk_path(&package).await?;
	let obb_root = format!("/sdcard/Android/obb/{package}");
	let obb = device.list_files(&obb_root).await?;
	println!("APK: {apk}");
	println!("OBB files: {}", obb.len());

	let dir = out.join(&package);
	let mut pulls = vec![(apk, dir.join(format!("{package}.apk")))];
	for (remote, _) in &obb {
		let relative = remote
			.strip_prefix(&format!("{obb_root}/"))
			.ok_or("odd OBB path")?;
		pulls.push((remote.clone(), dir.join(&package).join(relative)));
	}
	device.pull(&pulls, &|_| {}).await?;

	let meta = read_apk_meta(&dir.join(format!("{package}.apk")))?;
	let id = hwid(device.serial());
	std::fs::write(dir.join("HWID.txt"), &id)?;
	let model = entry.display_name();
	let name = archive_name(
		meta.label.as_deref().unwrap_or(&package),
		meta.version_code.unwrap_or(0),
		&package,
		&id,
		&model,
	);
	let zip = out.join(&name);
	pack(&dir, &zip, &|_, _| {})?;
	println!(
		"Archive: {} ({} bytes)",
		zip.display(),
		std::fs::metadata(&zip)?.len()
	);
	Ok(())
}
