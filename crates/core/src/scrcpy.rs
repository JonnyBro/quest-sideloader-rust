//! Screen mirroring through scrcpy, when it's installed.

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

/// Runs scrcpy for `serial` until its window closes. On failure returns the
/// last thing scrcpy printed.
pub(crate) async fn run(adb: &AdbServer, serial: &str, title: &str) -> Result<(), String> {
	let program = path().ok_or_else(|| format!("{PROGRAM} not found"))?;
	let mut command = tokio::process::Command::new(program);
	command
		.arg("--serial")
		.arg(serial)
		.arg("--window-title")
		.arg(title)
		// scrcpy would otherwise start its own adb server on 5037 and not
		// see the headset this app's server holds.
		.env("ADB", adb.adb_path())
		.env("ANDROID_ADB_SERVER_PORT", adb.port().to_string())
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::piped())
		.kill_on_drop(true);
	#[cfg(windows)]
	{
		const CREATE_NO_WINDOW: u32 = 0x0800_0000;
		command.creation_flags(CREATE_NO_WINDOW);
	}
	let output = command.output().await.map_err(|e| e.to_string())?;
	if output.status.success() {
		return Ok(());
	}
	let stderr = String::from_utf8_lossy(&output.stderr);
	Err(stderr
		.lines()
		.rev()
		.map(str::trim)
		.find(|line| line.starts_with("ERROR:"))
		.or_else(|| stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()))
		.unwrap_or("exited with an error")
		.to_owned())
}
