//! Dev tool: `cargo run -p quest-sideloader-core --example apk_info -- <file.apk|folder>`.
//! Prints what would be installed; never talks to a device.

use std::path::PathBuf;

use quest_sideloader_core::sideload::{InstallPlan, Source, script};

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let path: PathBuf = std::env::args_os()
		.nth(1)
		.ok_or("usage: apk_info <path>")?
		.into();
	let plan = match Source::classify(&path)? {
		Source::Apk(p) => InstallPlan::from_apk(&p)?,
		Source::Folder(p) => InstallPlan::from_folder(&p)?,
		Source::Zip(_) => return Err("zip: extract it first".into()),
	};
	println!("{plan:#?}");
	if let Some(script_path) = &plan.script {
		let text = std::fs::read_to_string(script_path)?;
		let analysis = script::analyze(&text, &plan.root, plan.package());
		println!("needs review: {}", analysis.needs_review());
		for line in &analysis.lines {
			println!("{:>3} {:?} | {}", line.number, line.verdict(), line.text);
		}
	}
	Ok(())
}
