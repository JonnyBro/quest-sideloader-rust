//! Downloading a release from the mirror, unpacking it and handing it to the
//! installer. Jobs wait in a queue; only `max_concurrent_downloads` of them
//! transfer at once, unpacking and installing don't hold a slot.

use rust_i18n::t;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use futures_util::stream::{self, StreamExt, TryStreamExt};

use tokio::sync::{mpsc, oneshot};

use super::EventSink;
use super::install::{self, DeviceLocks, InstallContext, ReviewDecision};
use super::task::{TaskEntry, TaskId, TaskKind, TaskSnapshot, TaskState, Tracker};
use crate::adb::AdbServer;
use crate::archive::{self, ArchiveError};
use crate::catalog::{
	CatalogError, FetchProgress, Fetcher, Game, Mirror, RemoteFile, release_file_path,
};
use crate::downloaded::INCOMPLETE_DIR;
use crate::format_bytes;
use crate::sideload::SideloadError;
use crate::sideload::run::Update;

mod queue;

pub(super) use queue::Queue;
use queue::Slot;

const MIB: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum JobError {
	#[error("{}", rust_i18n::t!("err.cancelled"))]
	Cancelled,
	#[error("{0}")]
	Failed(String),
}

impl From<SideloadError> for JobError {
	fn from(e: SideloadError) -> Self {
		match e {
			SideloadError::Cancelled => Self::Cancelled,
			other => Self::Failed(other.to_string()),
		}
	}
}

impl From<ArchiveError> for JobError {
	fn from(e: ArchiveError) -> Self {
		match e {
			ArchiveError::Cancelled => Self::Cancelled,
			other => Self::Failed(other.to_string()),
		}
	}
}

impl From<crate::catalog::CatalogError> for JobError {
	fn from(e: crate::catalog::CatalogError) -> Self {
		Self::Failed(e.to_string())
	}
}

fn io_failed(path: &Path) -> impl FnOnce(std::io::Error) -> JobError + '_ {
	move |e| JobError::Failed(format!("{}: {e}", path.display()))
}

/// What the job needs from the engine at start time.
#[derive(Clone)]
pub(super) struct Deps {
	pub fetcher: Fetcher,
	pub adb: AdbServer,
	pub locks: DeviceLocks,
	pub sink: EventSink,
	pub work_dir: PathBuf,
	pub downloads_dir: PathBuf,
	pub keep_downloads: bool,
	pub backups: Option<PathBuf>,
}

pub(super) struct Job {
	pub id: TaskId,
	pub game: Game,
	pub mirror: Mirror,
	pub serial: Option<String>,
}

/// Sets the flag when dropped, which stops a blocking extraction of an aborted job.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
	fn drop(&mut self) {
		self.0.store(true, Ordering::Relaxed);
	}
}

fn finish(job: &Job, deps: &Deps, tracker: &Tracker, result: Result<Option<PathBuf>, JobError>) {
	match result {
		Ok(output) => {
			let stage = if output.is_some() {
				t!("task.downloaded")
			} else {
				t!("task.installed")
			};
			tracker.finish(TaskState::Done, Some(stage.into()), output);
			if let Some(serial) = &job.serial {
				super::devices::spawn_fetch_info(
					deps.adb.device(serial.clone()),
					deps.sink.clone(),
				);
			}
		}
		Err(JobError::Cancelled) => tracker.finish(
			TaskState::Cancelled,
			Some(t!("task.cancelled").into_owned()),
			None,
		),
		Err(JobError::Failed(e)) => {
			tracing::error!("{}: {e}", job.game.release_name);
			tracker.finish(TaskState::Failed(e), None, None);
		}
	}
}

/// Folder name for a release; replaces characters Windows doesn't allow.
fn folder_name(release_name: &str) -> String {
	let cleaned: String = release_name
		.chars()
		.map(|c| {
			if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
				'_'
			} else {
				c
			}
		})
		.collect();
	let trimmed = cleaned.trim_end_matches(['.', ' ']).trim_start();
	if trimmed.is_empty() {
		"release".into()
	} else {
		trimmed.to_owned()
	}
}

/// Files worth downloading: archive volumes and small sidecars, without
/// leftovers like macOS `._` files or unfinished uploads.
fn wanted(files: Vec<RemoteFile>) -> Vec<RemoteFile> {
	files
		.into_iter()
		.filter(|f| {
			let lower = f.name.to_ascii_lowercase();
			!f.name.starts_with("._")
				&& !lower.ends_with(".partial")
				&& !lower.ends_with(".tmp")
				&& !f.name.contains('/')
		})
		.collect()
}

fn first_volume(files: &[RemoteFile]) -> Option<&str> {
	let mut candidates: Vec<&str> = files
		.iter()
		.map(|f| f.name.as_str())
		.filter(|n| {
			let lower = n.to_ascii_lowercase();
			lower.ends_with(".7z.001") || lower.ends_with(".7z")
		})
		.collect();
	candidates.sort();
	candidates.into_iter().next()
}

fn check_space(dir: &Path, needed: u64) -> Result<(), JobError> {
	let available = fs4::available_space(dir).map_err(io_failed(dir))?;
	if available < needed {
		return Err(JobError::Failed(
			t!(
				"dl.no_space",
				dir = dir.display(),
				need = format_bytes(needed),
				have = format_bytes(available)
			)
			.into_owned(),
		));
	}
	Ok(())
}

/// Returns the kept folder for download-only jobs, `None` once installed.
async fn run(
	job: &Job,
	deps: &Deps,
	tracker: &Tracker,
	slot: &Slot,
	review: oneshot::Receiver<ReviewDecision>,
) -> Result<Option<PathBuf>, JobError> {
	let release = &job.game.release_name;
	let name = folder_name(release);
	let files_dir = deps.downloads_dir.join(&name);
	let work_dir = deps.downloads_dir.join(INCOMPLETE_DIR).join(&name);
	let archive_dir = work_dir.join("archive");
	let staging = work_dir.join("files");

	if files_dir.is_dir() {
		slot.free();
		tracing::debug!(path = %files_dir.display(), "release already downloaded");
	} else {
		tokio::fs::create_dir_all(&work_dir)
			.await
			.map_err(io_failed(&work_dir))?;
		download(job, deps, tracker, &archive_dir).await?;
		slot.free();
		unpack(job, tracker, &archive_dir, &staging).await?;
		let (staging, files_dir, work_dir) = (staging.clone(), files_dir.clone(), work_dir.clone());
		tokio::task::spawn_blocking(move || finalize(&staging, &files_dir, &work_dir))
			.await
			.map_err(|e| JobError::Failed(e.to_string()))??;
	}

	let Some(serial) = &job.serial else {
		return Ok(Some(files_dir));
	};
	let ctx = InstallContext {
		id: job.id,
		device: deps.adb.device(serial.clone()),
		work_dir: deps.work_dir.clone(),
		locks: deps.locks.clone(),
		sink: deps.sink.clone(),
		title_from_files: false,
		backups: deps.backups.clone(),
	};
	check_headset_space(&ctx, &files_dir).await?;
	let installed = install::install_from(&ctx, &files_dir, tracker, review).await;
	if installed.is_err() && !deps.keep_downloads {
		tracker.update(
			|s| {
				if let Some(conflict) = &mut s.conflict {
					conflict.remove_after = true;
				}
			},
			true,
		);
	}
	installed?;

	if deps.keep_downloads {
		return Ok(Some(files_dir));
	}
	// Removed before the task reports done, so a failure (on Windows, often a scanner holding a file open)
	// is visible instead of silently leaving gigabytes behind.
	let dir = files_dir.clone();
	let removed = tokio::task::spawn_blocking(move || remove_if_exists(&dir))
		.await
		.map_err(|e| JobError::Failed(e.to_string()))?;
	if let Err(e) = removed {
		tracker.report(Update::Warning(
			t!("dl.remove_failed", error = e).into_owned(),
		));
	}
	Ok(None)
}

async fn download(
	job: &Job,
	deps: &Deps,
	tracker: &Tracker,
	archive_dir: &Path,
) -> Result<(), JobError> {
	let release = &job.game.release_name;
	let listing = t!("dl.listing");
	tracker.stage(listing.clone());
	let files = wanted(
		with_retries(tracker, &listing, || {
			deps.fetcher.list_release(&job.mirror, release)
		})
		.await?,
	);
	if first_volume(&files).is_none() {
		return Err(JobError::Failed(
			t!("dl.no_archive", release = release).into_owned(),
		));
	}

	let listed: Option<u64> = files.iter().map(|f| f.size).sum();
	let estimate = listed.or_else(|| job.game.size_mb.map(|mb| (mb * MIB as f64) as u64));
	tokio::fs::create_dir_all(archive_dir)
		.await
		.map_err(io_failed(archive_dir))?;
	if let Some(size) = estimate {
		// Archives and their unpacked copy exist side by side for a while.
		check_space(archive_dir, size.saturating_mul(2) + 512 * MIB)?;
	}

	tracker.stage(t!("dl.downloading_n", n = files.len()));
	let concurrent = files.len().min(PARTS_AT_ONCE);
	// Bytes on disk per part; progress is their sum.
	let done: Vec<AtomicU64> = files.iter().map(|_| AtomicU64::new(0)).collect();
	let report = || {
		let sum = done.iter().map(|d| d.load(Ordering::Relaxed)).sum();
		tracker.bytes(sum, estimate);
	};
	let (done, report) = (&done, &report);
	let mut parts = Vec::with_capacity(files.len());
	for (i, file) in files.iter().enumerate() {
		parts.push(async move {
			let dest = archive_dir.join(&file.name);
			let existing = tokio::fs::metadata(&dest).await.ok().map(|m| m.len());
			let complete = match (existing, file.size) {
				(Some(have), Some(want)) => have == want,
				(Some(_), None) => true,
				(None, _) => false,
			};
			if !complete {
				let path = release_file_path(&job.mirror, release, &file.name);
				let stage = t!("dl.downloading_file", name = file.name);
				let progress: FetchProgress<'_> = &|bytes, _| {
					done[i].store(bytes, Ordering::Relaxed);
					report();
				};
				with_retries(tracker, &stage, || {
					deps.fetcher
						.download_file(&job.mirror, &path, &dest, progress, concurrent)
				})
				.await?;
			}
			let size = tokio::fs::metadata(&dest).await.map_or(0, |m| m.len());
			done[i].store(size, Ordering::Relaxed);
			report();
			Ok::<_, JobError>(())
		});
	}
	stream::iter(parts)
		.buffer_unordered(PARTS_AT_ONCE)
		.try_collect::<Vec<()>>()
		.await?;
	Ok(())
}

fn finalize(staging: &Path, files_dir: &Path, work_dir: &Path) -> Result<(), JobError> {
	let entries: Vec<_> = std::fs::read_dir(staging)
		.map_err(io_failed(staging))?
		.filter_map(|e| e.ok())
		.collect();
	let source = match entries.as_slice() {
		[only]
			if only.file_type().is_ok_and(|t| t.is_dir())
				&& !crate::sideload::is_package_name(&only.file_name().to_string_lossy()) =>
		{
			only.path()
		}
		_ => staging.to_path_buf(),
	};
	remove_if_exists(files_dir)?;
	std::fs::rename(&source, files_dir).map_err(io_failed(&source))?;
	remove_if_exists(work_dir)?;
	// Drop `.incomplete` itself once nothing else is in progress.
	if let Some(parent) = work_dir.parent() {
		let _ = std::fs::remove_dir(parent);
	}
	Ok(())
}

/// Pauses between attempts after a network hiccup; downloads resume from
/// the partial file, so a retry only fetches what's missing.
/// Release parts fetched at the same time, like other clients of these mirrors.
const PARTS_AT_ONCE: usize = 4;

const RETRY_DELAYS: [u64; 3] = [5, 15, 45];

fn retryable(e: &CatalogError) -> bool {
	match e {
		CatalogError::Http(_) => true,
		CatalogError::Status { status, .. } => *status == 429 || *status >= 500,
		_ => false,
	}
}

async fn with_retries<T, F, Fut>(
	tracker: &Tracker,
	stage: &str,
	mut attempt: F,
) -> Result<T, CatalogError>
where
	F: FnMut() -> Fut,
	Fut: Future<Output = Result<T, CatalogError>>,
{
	let mut delays = RETRY_DELAYS.iter();
	loop {
		match attempt().await {
			Err(e) if retryable(&e) => {
				let Some(&delay) = delays.next() else {
					return Err(e);
				};
				tracing::warn!("{stage}: {e}; retrying in {delay} s");
				tracker.stage(t!("dl.retrying", stage = stage, delay = delay));
				tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
				tracker.stage(stage.to_owned());
			}
			other => return other,
		}
	}
}

async fn unpack(
	job: &Job,
	tracker: &Tracker,
	archive_dir: &Path,
	staging: &Path,
) -> Result<(), JobError> {
	tracker.stage(t!("dl.unpacking"));
	let password = job.mirror.archive_password()?;
	let first = {
		let entries = std::fs::read_dir(archive_dir).map_err(io_failed(archive_dir))?;
		let files: Vec<RemoteFile> = entries
			.filter_map(|e| e.ok())
			.map(|e| RemoteFile {
				name: e.file_name().to_string_lossy().into_owned(),
				size: None,
			})
			.collect();
		first_volume(&files)
			.map(|n| archive_dir.join(n))
			.ok_or_else(|| JobError::Failed(t!("dl.no_archive_local").into_owned()))?
	};

	let cancel = Arc::new(AtomicBool::new(false));
	let _guard = CancelOnDrop(cancel.clone());
	let progress = |done: u64, total: u64| tracker.bytes(done, Some(total));

	tokio::task::block_in_place(|| -> Result<(), JobError> {
		remove_if_exists(staging)?;
		let volumes = archive::find_volumes(&first).map_err(io_failed(&first))?;
		archive::extract_7z(&volumes, staging, Some(&password), &progress, &cancel)?;
		extract_nested(staging, &password, &cancel)?;
		remove_if_exists(archive_dir)
	})
}

/// Some releases ship extra `.7z` archives next to install.txt (for example
/// `_data.7z`) that must be unpacked in place before the script runs.
fn extract_nested(root: &Path, password: &str, cancel: &AtomicBool) -> Result<(), JobError> {
	let mut dirs = vec![root.to_path_buf()];
	if let Ok(entries) = std::fs::read_dir(root) {
		dirs.extend(
			entries
				.filter_map(|e| e.ok())
				.map(|e| e.path())
				.filter(|p| p.is_dir()),
		);
	}
	for dir in dirs {
		let Ok(entries) = std::fs::read_dir(&dir) else {
			continue;
		};
		for path in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
			let is_archive = path.is_file()
				&& path
					.extension()
					.is_some_and(|e| e.eq_ignore_ascii_case("7z"));
			if !is_archive {
				continue;
			}
			let volumes = [path.clone()];
			let no_op = |_: u64, _: u64| {};
			match archive::extract_7z(&volumes, &dir, None, &no_op, cancel) {
				Err(ArchiveError::WrongPassword) => {
					archive::extract_7z(&volumes, &dir, Some(password), &no_op, cancel)?;
				}
				other => other?,
			}
			std::fs::remove_file(&path).map_err(io_failed(&path))?;
		}
	}
	Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), JobError> {
	match std::fs::remove_dir_all(path) {
		Ok(()) => Ok(()),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(e) => Err(io_failed(path)(e)),
	}
}

/// Refuses to start when the unpacked release clearly won't fit on the headset.
async fn check_headset_space(ctx: &InstallContext, files_dir: &Path) -> Result<(), JobError> {
	let dir = files_dir.to_path_buf();
	let size: u64 = tokio::task::spawn_blocking(move || {
		crate::adb::list_files(&dir).map(|files| files.iter().map(|f| f.size).sum())
	})
	.await
	.map_err(|e| JobError::Failed(e.to_string()))?
	.map_err(io_failed(files_dir))?;
	match ctx.device.free_space("/sdcard").await {
		Ok(free) if free < size => Err(JobError::Failed(
			t!(
				"dl.no_space_headset",
				need = format_bytes(size),
				have = format_bytes(free)
			)
			.into_owned(),
		)),
		Ok(_) => Ok(()),
		Err(e) => {
			tracing::warn!("could not check headset storage: {e}");
			Ok(())
		}
	}
}

pub(super) fn entry(
	release: String,
	review: oneshot::Sender<ReviewDecision>,
	tracker: Arc<Tracker>,
) -> TaskEntry {
	TaskEntry {
		abort: None,
		review: Some(review),
		tracker,
		release: Some(release),
	}
}

#[cfg(test)]
mod tests;
