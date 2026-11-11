use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::script::Action;
use super::{InstallPlan, SideloadError, io_err};
use crate::adb::{Device, list_files};

const SHELL_TIMEOUT: Duration = Duration::from_secs(300);

pub enum Update {
	Stage(String),
	Bytes { done: u64, total: u64 },
	Warning(String),
}

pub type Reporter<'a> = &'a (dyn Fn(Update) + Send + Sync);

struct ByteCounter<'a> {
	done: AtomicU64,
	total: u64,
	report: Reporter<'a>,
}

impl ByteCounter<'_> {
	fn add(&self, n: u64) {
		let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
		(self.report)(Update::Bytes {
			done,
			total: self.total,
		});
	}
}

fn file_name(path: &Path) -> String {
	path.file_name().map_or_else(
		|| path.display().to_string(),
		|n| n.to_string_lossy().into_owned(),
	)
}

/// Installs every APK, then copies every OBB folder.
pub async fn run_default(
	device: &Device,
	plan: &InstallPlan,
	report: Reporter<'_>,
) -> Result<(), SideloadError> {
	let counter = ByteCounter {
		done: AtomicU64::new(0),
		total: plan.default_steps_bytes(),
		report,
	};
	let progress = |n| counter.add(n);
	for apk in &plan.apks {
		report(Update::Stage(format!(
			"Installing {}",
			file_name(&apk.path)
		)));
		device.install_apk(&apk.path, &progress).await?;
	}
	for obb in &plan.obbs {
		report(Update::Stage(format!("Copying OBB {}", obb.package)));
		device.push(&obb.path, &obb.remote_dir(), &progress).await?;
	}
	Ok(())
}

fn local_size(path: &Path) -> Result<u64, SideloadError> {
	let metadata = std::fs::metadata(path).map_err(io_err(path))?;
	if metadata.is_dir() {
		Ok(list_files(path)
			.map_err(io_err(path))?
			.iter()
			.map(|f| f.size)
			.sum())
	} else {
		Ok(metadata.len())
	}
}

/// Like `adb push`: pushing into an existing directory puts the source inside it.
/// Unlike adb, a folder pushed onto a same-named existing folder is merged into
/// it instead of nested (`obb/com.game/com.game`), which no game expects.
async fn push_target(device: &Device, local: &Path, remote: &str) -> Result<String, SideloadError> {
	let name = file_name(local);
	let same_name = remote.rsplit('/').next() == Some(name.as_str());
	if local.is_dir() && same_name {
		return Ok(remote.to_owned());
	}
	if device.is_dir(remote).await? {
		Ok(format!("{}/{name}", remote.trim_end_matches('/')))
	} else {
		Ok(remote.to_owned())
	}
}

/// Runs approved install.txt actions. Failed installs and pushes stop the
/// script; a failing shell command is only reported, as release scripts often
/// clean up paths that don't exist.
pub async fn run_script(
	device: &Device,
	actions: &[(usize, Action)],
	report: Reporter<'_>,
) -> Result<(), SideloadError> {
	let mut total = 0;
	for (line, action) in actions {
		if let Action::Install { apk: path } | Action::Push { local: path, .. } = action {
			if !path.exists() {
				return Err(SideloadError::Script {
					line: *line,
					message: format!("{} does not exist", path.display()),
				});
			}
			total += local_size(path)?;
		}
	}
	let counter = ByteCounter {
		done: AtomicU64::new(0),
		total,
		report,
	};
	let progress = |n| counter.add(n);
	let step_count = actions.len();

	for (step, (line, action)) in actions.iter().enumerate() {
		let prefix = format!("Step {}/{step_count}", step + 1);
		match action {
			Action::Install { apk } => {
				report(Update::Stage(format!(
					"{prefix}: installing {}",
					file_name(apk)
				)));
				device.install_apk(apk, &progress).await?;
			}
			Action::Push { local, remote } => {
				report(Update::Stage(format!(
					"{prefix}: copying {}",
					file_name(local)
				)));
				let target = push_target(device, local, remote).await?;
				device.push(local, &target, &progress).await?;
			}
			Action::Uninstall { package } => {
				report(Update::Stage(format!("{prefix}: uninstalling {package}")));
				let command = format!("pm uninstall {}", shell_words::quote(package));
				let out = device.shell_with_timeout(&command, SHELL_TIMEOUT).await?;
				if !out.success() {
					report(Update::Warning(format!(
						"line {line}: uninstall failed: {}",
						out.stdout.trim()
					)));
				}
			}
			Action::Shell { command } => {
				report(Update::Stage(format!("{prefix}: {command}")));
				let out = device.shell_with_timeout(command, SHELL_TIMEOUT).await?;
				if !out.success() {
					let detail = if out.stderr.trim().is_empty() {
						out.stdout.trim()
					} else {
						out.stderr.trim()
					};
					report(Update::Warning(format!(
						"line {line}: `{command}` failed: {detail}"
					)));
				}
			}
		}
	}
	Ok(())
}
