use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::adb::AdbServer;

const PROGRAM: &str = if cfg!(windows) {
	"scrcpy.exe"
} else {
	"scrcpy"
};

/// scrcpy from `PATH` (and Homebrew's folders on macOS), looked up once.
pub fn path() -> Option<&'static Path> {
	static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
	FOUND
		.get_or_init(|| {
			let program = crate::paths::default_tool(PROGRAM);
			if program.is_absolute() {
				return Some(program);
			}
			let dirs = std::env::var_os("PATH")?;
			std::env::split_paths(&dirs)
				.map(|dir| dir.join(PROGRAM))
				.find(|candidate| candidate.is_file())
		})
		.as_deref()
}

/// What part of the picture to show. The headset renders both eyes side by
/// side, left eye on the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eye {
	Both,
	Left,
	Right,
}

/// The `--crop` value (`width:height:x:y`) for one eye.
fn crop(eye: Eye, width: u32, height: u32) -> Option<String> {
	let half = width / 2;
	match eye {
		Eye::Both => None,
		Eye::Left => Some(format!("{half}:{height}:0:0")),
		Eye::Right => Some(format!("{half}:{height}:{half}:0")),
	}
}

/// Runs until the window closes. Output goes to the log; a failure returns
/// the last error line.
pub(crate) async fn run(
	adb: &AdbServer,
	serial: &str,
	title: &str,
	eye: Eye,
) -> Result<(), String> {
	let program = path().ok_or_else(|| format!("{PROGRAM} not found"))?;
	let crop = if eye == Eye::Both {
		None
	} else {
		let (width, height) = adb
			.device(serial)
			.display_size()
			.await
			.map_err(|e| e.to_string())?;
		crop(eye, width, height)
	};
	let mut command = tokio::process::Command::new(program);
	command
		.arg("--serial")
		.arg(serial)
		.arg("--window-title")
		.arg(title)
		.arg("--no-audio");
	if let Some(crop) = &crop {
		command.arg(format!("--crop={crop}"));
	}
	command
		// scrcpy would otherwise start its own adb server on 5037 and not
		// see the headset this app's server holds.
		.env("ADB", adb.adb_path())
		.env("ANDROID_ADB_SERVER_PORT", adb.port().to_string())
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped())
		.kill_on_drop(true);
	#[cfg(windows)]
	{
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		command.creation_flags(CREATE_NO_WINDOW);
	}
	tracing::info!(
		"starting {} for {serial} (adb {}, port {})",
		program.display(),
		adb.adb_path().display(),
		adb.port()
	);
	let mut child = command.spawn().map_err(|e| e.to_string())?;
	let stdout = child.stdout.take().map(log_lines);
	let stderr = child.stderr.take().map(log_lines);
	let status = child.wait().await.map_err(|e| e.to_string())?;
	let mut last_error = None;
	let mut last_line = None;
	for task in [stdout, stderr].into_iter().flatten() {
		if let Ok((error, line)) = task.await {
			last_error = last_error.or(error);
			last_line = last_line.or(line);
		}
	}
	if status.success() {
		tracing::info!("scrcpy closed");
		return Ok(());
	}
	tracing::error!("scrcpy exited with {status}");
	Err(last_error
		.or(last_line)
		.unwrap_or_else(|| status.to_string()))
}

/// Logs each line at the level scrcpy gave it. Returns the last `ERROR:`
/// line and the last line of any kind.
fn log_lines(
	pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> tokio::task::JoinHandle<(Option<String>, Option<String>)> {
	use tokio::io::AsyncBufReadExt;
	tokio::spawn(async move {
		let mut lines = tokio::io::BufReader::new(pipe).lines();
		let (mut error, mut last) = (None, None);
		while let Ok(Some(line)) = lines.next_line().await {
			let line = line.trim().to_owned();
			if line.is_empty() {
				continue;
			}
			let tag = line.strip_prefix("[server] ").unwrap_or(&line);
			if tag.starts_with("ERROR:") {
				tracing::error!("scrcpy: {line}");
				error = Some(line.clone());
			} else if tag.starts_with("WARN:") {
				tracing::warn!("scrcpy: {line}");
			} else {
				tracing::info!("scrcpy: {line}");
			}
			last = Some(line);
		}
		(error, last)
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn crops_one_eye() {
		assert_eq!(crop(Eye::Both, 3664, 1920), None);
		assert_eq!(
			crop(Eye::Left, 3664, 1920).as_deref(),
			Some("1832:1920:0:0")
		);
		assert_eq!(
			crop(Eye::Right, 3664, 1920).as_deref(),
			Some("1832:1920:1832:0")
		);
	}
}
