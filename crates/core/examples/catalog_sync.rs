//! Dev tool: `cargo run -p quest-sideloader-core --example catalog_sync -- <server-config.json>`.
//! Syncs a VRP-style mirror into a temporary folder and prints the game list.

use quest_sideloader_core::catalog::{Fetcher, Mirror, SyncStage, sync};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	let config = std::env::args_os()
		.nth(1)
		.ok_or("usage: catalog_sync <config.json>")?;
	let mirror = Mirror::from_vrp_json("cli", &std::fs::read_to_string(config)?)?;
	let root = std::env::temp_dir().join("quest-sideloader-catalog-sync");
	let catalog = sync(&Fetcher::new(None)?, &mirror, &root, &|stage| {
		if let SyncStage::Downloading { done, total } = stage {
			eprintln!("downloaded {done} of {total:?} bytes");
		}
	})
	.await?;
	println!(
		"{} games, cached in {}",
		catalog.games.len(),
		root.display()
	);
	for game in &catalog.games {
		println!(
			"{:<26} {:<28} v{:<6} thumb={} note={}",
			game.name,
			game.package,
			game.version_code.unwrap_or(0),
			catalog.thumbnail(&game.package).is_some(),
			catalog.note(&game.release_name).is_some(),
		);
	}
	Ok(())
}
