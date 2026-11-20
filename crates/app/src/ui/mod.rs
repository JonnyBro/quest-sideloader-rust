pub mod console;
pub mod contribute;
pub mod devices;
pub mod downloads;
pub mod headset;
pub mod keys;
pub mod library;
pub mod logs;
pub mod mirrors;
pub mod rename;
pub mod review;
pub mod settings;
pub mod tasks;
pub mod tool_setup;
pub mod welcome;

pub use quest_sideloader_core::format_bytes;

pub fn open_folder(path: &std::path::Path) {
	let program = if cfg!(windows) {
		"explorer"
	} else if cfg!(target_os = "macos") {
		"open"
	} else {
		"xdg-open"
	};
	if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
		tracing::warn!("could not open {}: {e}", path.display());
	}
}

/// Unix seconds as `YYYY-MM-DD HH:MM` (UTC).
pub fn format_unix_time(secs: u64) -> String {
	let days = (secs / 86_400) as i64;
	let rem = secs % 86_400;
	// Howard Hinnant's days-to-civil algorithm.
	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let day = doy - (153 * mp + 2) / 5 + 1;
	let month = if mp < 10 { mp + 3 } else { mp - 9 };
	let year = yoe + era * 400 + i64::from(month <= 2);
	format!(
		"{year:04}-{month:02}-{day:02} {:02}:{:02}",
		rem / 3600,
		(rem % 3600) / 60
	)
}
