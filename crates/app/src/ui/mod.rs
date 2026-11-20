pub mod checkup;
pub mod console;
pub mod contribute;
pub mod devices;
pub mod downloads;
pub mod headset;
pub mod inspector;
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

#[derive(Clone, Copy)]
pub enum Tone {
	/// Installs, restores, builds: the step the user came for.
	Go,
	/// Deletes, uninstalls, powers off: hard or impossible to undo.
	Danger,
}

/// A button colored for what it does. White text on a dark fill reads the
/// same in the light and dark themes.
pub fn toned(text: impl Into<String>, tone: Tone) -> egui::Button<'static> {
	let fill = match tone {
		Tone::Go => egui::Color32::from_rgb(46, 125, 50),
		Tone::Danger => egui::Color32::from_rgb(183, 28, 28),
	};
	egui::Button::new(egui::RichText::new(text.into()).color(egui::Color32::WHITE)).fill(fill)
}

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

/// Unix seconds as `YYYY-MM-DD HH:MM` in the computer's time zone.
pub fn format_unix_time(secs: u64) -> String {
	chrono::DateTime::from_timestamp(secs as i64, 0)
		.map(|t| local(t.with_timezone(&chrono::Utc)))
		.unwrap_or_default()
}

fn local(time: chrono::DateTime<chrono::Utc>) -> String {
	time.with_timezone(&chrono::Local)
		.format("%Y-%m-%d %H:%M")
		.to_string()
}

/// A mirror's `2024-11-02 10:15 UTC` in the computer's time zone, or the
/// text as it is when it doesn't parse.
pub fn mirror_time(value: &str) -> String {
	let trimmed = value.trim().trim_end_matches("UTC").trim();
	chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M")
		.map(|t| local(t.and_utc()))
		.unwrap_or_else(|_| value.to_owned())
}
