//! The application menu entry and icon in the user's ~/.local/share.

use std::path::{Path, PathBuf};

const DESKTOP: &str = include_str!("../assets/app-icon/quest-sideloader.desktop");
const ICON: &str = include_str!("../assets/app-icon/quest-sideloader.svg");

struct Files {
	applications: PathBuf,
	desktop: PathBuf,
	icon: PathBuf,
}

fn files() -> Option<Files> {
	let data = directories::BaseDirs::new()?.data_dir().to_path_buf();
	let applications = data.join("applications");
	Some(Files {
		desktop: applications.join("quest-sideloader.desktop"),
		applications,
		icon: data.join("icons/hicolor/scalable/apps/quest-sideloader.svg"),
	})
}

/// Linux reports a replaced binary as "path (deleted)" until the next start.
fn current_exe() -> std::io::Result<PathBuf> {
	let exe = std::env::current_exe()?;
	Ok(
		match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
			Some(path) => PathBuf::from(path),
			None => exe,
		},
	)
}

/// The entry runs this binary by its full path: a release archive is unpacked
/// anywhere, rarely into PATH.
fn desktop_entry(exe: &Path) -> String {
	let exe = exe.display().to_string();
	let quoted = if exe.contains([' ', '"', '\\', '$', '`']) {
		format!(
			"\"{}\"",
			exe.replace('\\', "\\\\")
				.replace('"', "\\\"")
				.replace('$', "\\$")
				.replace('`', "\\`")
		)
	} else {
		exe
	};
	DESKTOP.replace("Exec=quest-sideloader\n", &format!("Exec={quoted}\n"))
}

pub fn is_installed() -> bool {
	files().is_some_and(|f| f.desktop.exists())
}

pub fn install() -> Result<(), String> {
	let files = files().ok_or("no home folder")?;
	let exe = current_exe().map_err(|e| e.to_string())?;
	for (path, text) in [
		(&files.desktop, desktop_entry(&exe)),
		(&files.icon, ICON.to_owned()),
	] {
		if let Some(dir) = path.parent() {
			std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
		}
		std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
	}
	refresh_cache(&files);
	Ok(())
}

pub fn uninstall() -> Result<(), String> {
	let files = files().ok_or("no home folder")?;
	for path in [&files.desktop, &files.icon] {
		match std::fs::remove_file(path) {
			Ok(()) => {}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => return Err(format!("{}: {e}", path.display())),
		}
	}
	refresh_cache(&files);
	Ok(())
}

/// Keeps an installed entry pointing at this binary after the archive moved,
/// and its icon current after an update.
pub fn refresh_if_installed() {
	let Some(files) = files() else {
		return;
	};
	let Ok(current) = std::fs::read_to_string(&files.desktop) else {
		return;
	};
	let Ok(exe) = current_exe() else {
		return;
	};
	let up_to_date = current == desktop_entry(&exe)
		&& std::fs::read_to_string(&files.icon).is_ok_and(|t| t == ICON);
	if !up_to_date && let Err(e) = install() {
		tracing::warn!("could not update the menu entry: {e}");
	}
}

/// The database is optional: without the tool, desktops pick the entry up on
/// their next scan.
fn refresh_cache(files: &Files) {
	let status = std::process::Command::new("update-desktop-database")
		.arg(&files.applications)
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status();
	if let Err(e) = status {
		tracing::debug!("update-desktop-database: {e}");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exec_uses_full_path() {
		let entry = desktop_entry(Path::new("/opt/qs/quest-sideloader"));
		assert!(entry.contains("\nExec=/opt/qs/quest-sideloader\n"));
		let entry = desktop_entry(Path::new("/home/me/My Apps/quest-sideloader"));
		assert!(entry.contains("\nExec=\"/home/me/My Apps/quest-sideloader\"\n"));
	}
}
