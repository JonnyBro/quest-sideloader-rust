//! Downloading a release from the mirror, unpacking it and handing it to the
//! installer. Jobs wait in a queue; only `max_concurrent_downloads` of them
//! transfer at once, unpacking and installing don't hold a slot.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{mpsc, oneshot};

use super::EventSink;
use super::install::{self, DeviceLocks, InstallContext, ReviewDecision};
use super::task::{TaskEntry, TaskId, TaskKind, TaskSnapshot, TaskState, Tracker};
use crate::adb::AdbServer;
use crate::archive::{self, ArchiveError};
use crate::catalog::{
	CatalogError, FetchProgress, Fetcher, Game, Mirror, RemoteFile, release_file_path,
};
use crate::format_bytes;
use crate::sideload::SideloadError;
use crate::sideload::run::Update;

const MIB: u64 = 1024 * 1024;
/// Written into the unpacked folder once extraction finished.
const COMPLETE_MARKER: &str = ".quest-sideloader-complete";

#[derive(Debug, thiserror::Error)]
enum JobError {
	#[error("cancelled")]
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
}

pub(super) struct Job {
	pub id: TaskId,
	pub game: Game,
	pub mirror: Mirror,
	pub serial: Option<String>,
}

struct Pending {
	job: Job,
	tracker: Arc<Tracker>,
	review: oneshot::Receiver<ReviewDecision>,
}

/// Queue of jobs that haven't started transferring yet.
pub(super) struct Queue {
	pending: VecDeque<Pending>,
	transferring: HashSet<TaskId>,
	slot_tx: mpsc::UnboundedSender<TaskId>,
}

impl Queue {
	pub fn new(slot_tx: mpsc::UnboundedSender<TaskId>) -> Self {
		Self {
			pending: VecDeque::new(),
			transferring: HashSet::new(),
			slot_tx,
		}
	}

	/// Adds a job and returns the handle the engine tracks it by.
	pub fn push(
		&mut self,
		job: Job,
		sink: EventSink,
	) -> (oneshot::Sender<ReviewDecision>, Arc<Tracker>) {
		let (review_tx, review_rx) = oneshot::channel();
		let tracker = Tracker::new(
			TaskSnapshot {
				id: job.id,
				kind: TaskKind::Download,
				serial: job.serial.clone(),
				title: job.game.name.clone(),
				stage: "Queued".into(),
				progress: None,
				detail: None,
				state: TaskState::Queued,
				warnings: Vec::new(),
				output: None,
			},
			sink,
		);
		self.pending.push_back(Pending {
			job,
			tracker: tracker.clone(),
			review: review_rx,
		});
		(review_tx, tracker)
	}

	/// Starts queued jobs while there are free slots. Returns the started
	/// jobs' abort handles so the engine can cancel them.
	pub fn start_ready(
		&mut self,
		limit: usize,
		deps: &Deps,
	) -> Vec<(TaskId, tokio::task::AbortHandle)> {
		let mut started = Vec::new();
		while self.transferring.len() < limit.max(1) {
			let Some(next) = self.pending.pop_front() else {
				break;
			};
			let id = next.job.id;
			self.transferring.insert(id);
			let slot = Slot {
				id,
				tx: self.slot_tx.clone(),
				freed: AtomicBool::new(false),
			};
			let deps = deps.clone();
			let handle = tokio::spawn(async move {
				let Pending {
					job,
					tracker,
					review,
				} = next;
				let result = run(&job, &deps, &tracker, &slot, review).await;
				slot.free();
				finish(&job, &deps, &tracker, result);
			});
			started.push((id, handle.abort_handle()));
		}
		started
	}

	/// A job finished transferring (or died); its slot is free again.
	pub fn slot_freed(&mut self, id: TaskId) {
		self.transferring.remove(&id);
	}

	/// Removes a job that hasn't started yet. Returns whether it was queued.
	pub fn remove_pending(&mut self, id: TaskId) -> bool {
		let before = self.pending.len();
		self.pending.retain(|p| p.job.id != id);
		self.pending.len() != before
	}

	/// Moves a queued job by `delta` places (negative is towards the front).
	pub fn move_by(&mut self, id: TaskId, delta: isize) {
		let Some(from) = self.pending.iter().position(|p| p.job.id == id) else {
			return;
		};
		let to = from
			.saturating_add_signed(delta)
			.min(self.pending.len() - 1);
		if let Some(item) = self.pending.remove(from) {
			self.pending.insert(to, item);
		}
		for (i, p) in self.pending.iter().enumerate() {
			p.tracker
				.update(|s| s.stage = format!("Queued · #{}", i + 1), true);
		}
	}

	pub fn abort_all(&mut self) {
		self.pending.clear();
	}
}

/// A transfer slot; tells the engine when it's free, even if the job is aborted.
struct Slot {
	id: TaskId,
	tx: mpsc::UnboundedSender<TaskId>,
	freed: AtomicBool,
}

impl Slot {
	fn free(&self) {
		if !self.freed.swap(true, Ordering::SeqCst) {
			let _ = self.tx.send(self.id);
		}
	}
}

impl Drop for Slot {
	fn drop(&mut self) {
		self.free();
	}
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
				"Downloaded"
			} else {
				"Installed"
			};
			tracker.finish(TaskState::Done, Some(stage.into()), output);
			if let Some(serial) = &job.serial {
				super::devices::spawn_fetch_info(
					deps.adb.device(serial.clone()),
					deps.sink.clone(),
				);
			}
		}
		Err(JobError::Cancelled) => {
			tracker.finish(TaskState::Cancelled, Some("Cancelled".into()), None)
		}
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
		return Err(JobError::Failed(format!(
			"not enough free space in {}: need about {}, have {}",
			dir.display(),
			format_bytes(needed),
			format_bytes(available)
		)));
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
	let job_dir = deps.downloads_dir.join(folder_name(release));
	let archive_dir = job_dir.join("archive");
	let files_dir = job_dir.join("files");
	tokio::fs::create_dir_all(&job_dir)
		.await
		.map_err(io_failed(&job_dir))?;

	if !files_dir.join(COMPLETE_MARKER).is_file() {
		download(job, deps, tracker, &archive_dir).await?;
		slot.free();
		unpack(job, tracker, &archive_dir, &files_dir).await?;
	} else {
		slot.free();
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
	};
	check_headset_space(&ctx, &files_dir).await?;
	install::install_from(&ctx, &files_dir, tracker, review).await?;

	if deps.keep_downloads {
		return Ok(Some(files_dir));
	}
	// Removed before the task reports done, so a failure (on Windows, often a scanner holding a file open)
	// is visible instead of silently leaving gigabytes behind.
	let dir = job_dir.clone();
	let removed = tokio::task::spawn_blocking(move || remove_if_exists(&dir))
		.await
		.map_err(|e| JobError::Failed(e.to_string()))?;
	if let Err(e) = removed {
		tracker.report(Update::Warning(format!(
			"installed, but the downloaded files could not be removed: {e}"
		)));
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
	tracker.stage("Listing files");
	let files = wanted(
		with_retries(tracker, "Listing files", || {
			deps.fetcher.list_release(&job.mirror, release)
		})
		.await?,
	);
	if first_volume(&files).is_none() {
		return Err(JobError::Failed(format!(
			"the mirror has no archive for {release}"
		)));
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

	tracker.stage(format!("Downloading {} file(s)", files.len()));
	let mut done_before = 0u64;
	for file in &files {
		let dest = archive_dir.join(&file.name);
		let existing = tokio::fs::metadata(&dest).await.ok().map(|m| m.len());
		let complete = match (existing, file.size) {
			(Some(have), Some(want)) => have == want,
			(Some(_), None) => true,
			(None, _) => false,
		};
		if !complete {
			let path = release_file_path(&job.mirror, release, &file.name);
			let base = done_before;
			let total = estimate;
			let stage = format!("Downloading {}", file.name);
			let progress: FetchProgress<'_> = &|done, _| tracker.bytes(base + done, total);
			with_retries(tracker, &stage, || {
				deps.fetcher
					.download_file(&job.mirror, &path, &dest, progress)
			})
			.await?;
		}
		done_before += tokio::fs::metadata(&dest).await.map_or(0, |m| m.len());
		tracker.bytes(done_before, estimate);
	}
	Ok(())
}

/// Pauses between attempts after a network hiccup; downloads resume from
/// the partial file, so a retry only fetches what's missing.
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
				tracker.stage(format!(
					"{stage}: connection problem, retrying in {delay} s"
				));
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
	files_dir: &Path,
) -> Result<(), JobError> {
	tracker.stage("Unpacking");
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
			.ok_or_else(|| JobError::Failed("downloaded files contain no archive".into()))?
	};

	let cancel = Arc::new(AtomicBool::new(false));
	let _guard = CancelOnDrop(cancel.clone());
	let staging = files_dir.with_extension("partial");
	let files_dir = files_dir.to_path_buf();
	let archive_dir = archive_dir.to_path_buf();
	let progress = |done: u64, total: u64| tracker.bytes(done, Some(total));

	tokio::task::block_in_place(|| -> Result<(), JobError> {
		remove_if_exists(&staging)?;
		let volumes = archive::find_volumes(&first).map_err(io_failed(&first))?;
		archive::extract_7z(&volumes, &staging, Some(&password), &progress, &cancel)?;
		extract_nested(&staging, &password, &cancel)?;
		remove_if_exists(&files_dir)?;
		std::fs::rename(&staging, &files_dir).map_err(io_failed(&staging))?;
		std::fs::write(files_dir.join(COMPLETE_MARKER), b"").map_err(io_failed(&files_dir))?;
		remove_if_exists(&archive_dir)
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
		Ok(free) if free < size => Err(JobError::Failed(format!(
			"not enough space on the headset: need {}, have {}",
			format_bytes(size),
			format_bytes(free)
		))),
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
mod tests {
	use super::*;

	#[test]
	fn retry_only_transient_errors() {
		assert!(retryable(&CatalogError::Http("connection reset".into())));
		for (status, expected) in [(503, true), (429, true), (403, false), (404, false)] {
			let e = CatalogError::Status {
				status,
				url: String::new(),
			};
			assert_eq!(retryable(&e), expected, "{status}");
		}
		assert!(!retryable(&CatalogError::Rclone("x".into())));
	}

	#[tokio::test(start_paused = true)]
	async fn retries_then_gives_up() {
		let (sink_tx, _rx) = std::sync::mpsc::channel();
		let tracker = Tracker::new(
			TaskSnapshot {
				id: 1,
				kind: TaskKind::Download,
				serial: None,
				title: "t".into(),
				stage: String::new(),
				progress: None,
				detail: None,
				state: TaskState::Running,
				warnings: Vec::new(),
				output: None,
			},
			EventSink {
				tx: sink_tx,
				notify: Arc::new(|| {}),
			},
		);
		let calls = std::sync::atomic::AtomicUsize::new(0);
		let result: Result<(), _> = with_retries(&tracker, "x", || {
			calls.fetch_add(1, Ordering::Relaxed);
			async { Err(CatalogError::Http("down".into())) }
		})
		.await;
		assert!(result.is_err());
		assert_eq!(calls.load(Ordering::Relaxed), 1 + RETRY_DELAYS.len());

		calls.store(0, Ordering::Relaxed);
		let result: Result<(), _> = with_retries(&tracker, "x", || {
			calls.fetch_add(1, Ordering::Relaxed);
			async {
				Err(CatalogError::Status {
					status: 403,
					url: String::new(),
				})
			}
		})
		.await;
		assert!(result.is_err());
		assert_eq!(calls.load(Ordering::Relaxed), 1, "403 is not retried");
	}

	#[test]
	fn folder_names_are_portable() {
		assert_eq!(
			folder_name("Beat Saber v1.40+1.40"),
			"Beat Saber v1.40+1.40"
		);
		assert_eq!(folder_name("A: B / C?"), "A_ B _ C_");
		assert_eq!(folder_name("Trailing dot. "), "Trailing dot");
		assert_eq!(folder_name("..."), "release");
	}

	#[test]
	fn picks_files_and_first_volume() {
		let files = wanted(vec![
			RemoteFile {
				name: "h.7z.002".into(),
				size: Some(1),
			},
			RemoteFile {
				name: "._h.7z.001".into(),
				size: Some(1),
			},
			RemoteFile {
				name: "h.7z.001".into(),
				size: Some(1),
			},
			RemoteFile {
				name: "h.7z.003.partial".into(),
				size: Some(1),
			},
		]);
		let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
		assert_eq!(names, ["h.7z.002", "h.7z.001"]);
		assert_eq!(first_volume(&files), Some("h.7z.001"));
		assert_eq!(
			first_volume(&[RemoteFile {
				name: "notes.txt".into(),
				size: None
			}]),
			None
		);
	}

	#[test]
	fn queue_order() {
		let (tx, _rx) = mpsc::unbounded_channel();
		let mut queue = Queue::new(tx);
		let (sink_tx, _sink_rx) = std::sync::mpsc::channel();
		let sink = EventSink {
			tx: sink_tx,
			notify: Arc::new(|| {}),
		};
		for id in 1..=3 {
			let mirror = Mirror {
				id: "m".into(),
				name: "m".into(),
				kind: crate::catalog::MirrorKind::Http {
					base_uri: "https://m/".into(),
				},
				password: "aGk=".into(),
				headers: Vec::new(),
			};
			let game = Game {
				name: format!("G{id}"),
				release_name: format!("G{id} v1"),
				package: format!("com.g{id}.x"),
				version_code: None,
				last_updated: String::new(),
				size_mb: None,
				downloads: None,
			};
			queue.push(
				Job {
					id,
					game,
					mirror,
					serial: None,
				},
				sink.clone(),
			);
		}
		queue.move_by(3, -5);
		let order: Vec<_> = queue.pending.iter().map(|p| p.job.id).collect();
		assert_eq!(order, [3, 1, 2]);
		queue.move_by(3, 1);
		assert!(queue.remove_pending(2));
		assert!(!queue.remove_pending(2));
		let order: Vec<_> = queue.pending.iter().map(|p| p.job.id).collect();
		assert_eq!(order, [1, 3]);
	}

	mod end_to_end {
		use std::collections::HashMap;
		use std::time::{Duration, Instant};

		use tokio::io::{AsyncReadExt, AsyncWriteExt};

		use crate::adb::testing::FakeAdb;
		use crate::catalog::{Game, Mirror, MirrorKind, release_hash};
		use crate::engine::{Command, Engine, Event, TaskId, TaskState};
		use crate::paths::AppPaths;
		use crate::settings::Settings;

		/// Serves fixed paths; directory paths get an autoindex page of their children.
		async fn serve(files: HashMap<String, Vec<u8>>) -> String {
			let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
			let addr = listener.local_addr().unwrap();
			let files = std::sync::Arc::new(files);
			tokio::spawn(async move {
				while let Ok((mut s, _)) = listener.accept().await {
					let files = files.clone();
					tokio::spawn(async move {
						let mut req = Vec::new();
						let mut buf = [0u8; 4096];
						while !req.windows(4).any(|w| w == b"\r\n\r\n") {
							match s.read(&mut buf).await {
								Ok(0) | Err(_) => return,
								Ok(n) => req.extend_from_slice(&buf[..n]),
							}
						}
						let req = String::from_utf8_lossy(&req).into_owned();
						let path = req.split(' ').nth(1).unwrap_or("/").to_owned();
						let body = if let Some(body) = files.get(&path) {
							Some(body.clone())
						} else if path.ends_with('/') {
							let links: String = files
								.keys()
								.filter_map(|k| k.strip_prefix(&path))
								.map(|name| format!("<a href=\"{name}\">{name}</a>\n"))
								.collect();
							(!links.is_empty()).then(|| links.into_bytes())
						} else {
							None
						};
						let response = match body {
							Some(body) => {
								let mut r = format!(
									"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
									body.len()
								)
								.into_bytes();
								r.extend(body);
								r
							}
							None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
						};
						let _ = s.write_all(&response).await;
					});
				}
			});
			format!("http://{addr}/")
		}

		const RELEASE: &str = "Demo Game v1 +1";

		/// The release as a mirror stores it: an encrypted, split 7z with a
		/// nested unencrypted `_data.7z` next to install.txt.
		fn release_files(dir: &std::path::Path) -> HashMap<String, Vec<u8>> {
			let nested_src = dir.join("nested");
			std::fs::create_dir_all(&nested_src).unwrap();
			std::fs::write(nested_src.join("extra.bin"), b"nested payload").unwrap();
			let nested = dir.join("_data.7z");
			sevenz_rust2::compress_to_path(&nested_src, &nested).unwrap();

			let src = dir.join("src");
			let game = src.join(RELEASE);
			std::fs::create_dir_all(game.join("com.example.game")).unwrap();
			std::fs::write(game.join("com.example.game/main.obb"), vec![9u8; 300_000]).unwrap();
			std::fs::copy(&nested, game.join("_data.7z")).unwrap();
			std::fs::write(
				game.join("install.txt"),
				"adb push com.example.game /sdcard/Android/obb/\n\
				 adb push extra.bin /sdcard/Android/data/com.example.game/files/extra.bin\n",
			)
			.unwrap();
			let archive = dir.join("release.7z");
			sevenz_rust2::compress_to_path_encrypted(&src, &archive, "hunter2".into()).unwrap();

			let hash = release_hash(RELEASE);
			let bytes = std::fs::read(&archive).unwrap();
			bytes
				.chunks(bytes.len() / 3 + 1)
				.enumerate()
				.map(|(i, chunk)| (format!("/{hash}/{hash}.7z.{:03}", i + 1), chunk.to_vec()))
				.collect()
		}

		struct Setup {
			dir: tempfile::TempDir,
			fake: FakeAdb,
			engine: Engine,
			_rt: tokio::runtime::Runtime,
		}

		fn setup(max_downloads: u8) -> Setup {
			let rt = tokio::runtime::Runtime::new().unwrap();
			let dir = tempfile::tempdir().unwrap();
			let base = rt.block_on(serve(release_files(dir.path())));
			let fake = rt.block_on(FakeAdb::start());
			let paths = AppPaths::under(dir.path().join("app"));
			let mut settings = Settings::defaults_for(&paths);
			settings.downloads_dir = dir.path().join("downloads");
			settings.max_concurrent_downloads = max_downloads;
			settings.mirrors.push(Mirror {
				id: "m1".into(),
				name: "test".into(),
				kind: MirrorKind::Http { base_uri: base },
				password: "aHVudGVyMg==".into(),
				headers: Vec::new(),
			});
			settings.normalize();
			let engine =
				Engine::start_with_adb_port(paths, settings, std::sync::Arc::new(|| {}), fake.port)
					.unwrap();
			Setup {
				dir,
				fake,
				engine,
				_rt: rt,
			}
		}

		fn game(release: &str) -> Game {
			Game {
				name: "Demo Game".into(),
				release_name: release.into(),
				package: "com.example.game".into(),
				version_code: Some(1),
				last_updated: String::new(),
				size_mb: Some(1.0),
				downloads: None,
			}
		}

		fn wait_finished(engine: &Engine, id: Option<TaskId>) -> crate::engine::TaskSnapshot {
			let deadline = Instant::now() + Duration::from_secs(20);
			loop {
				assert!(Instant::now() < deadline, "task did not finish");
				if let Some(s) = engine.poll_events().find_map(|e| match e {
					Event::TaskUpdated(s)
						if s.state.is_finished() && id.is_none_or(|id| s.id == id) =>
					{
						Some(s)
					}
					_ => None,
				}) {
					return s;
				}
				std::thread::sleep(Duration::from_millis(20));
			}
		}

		#[test]
		fn downloads_unpacks_and_installs() {
			let s = setup(2);
			s.engine.send(Command::Download {
				game: game(RELEASE),
				serial: Some("SER1".into()),
			});
			let done = wait_finished(&s.engine, None);
			assert_eq!(done.state, TaskState::Done, "{:?}", done);
			assert_eq!(done.title, "Demo Game");
			assert_eq!(done.output, None);

			let rec = s.fake.recorded.lock().unwrap();
			let mut pushed: Vec<_> = rec
				.pushed
				.iter()
				.map(|(p, d)| (p.as_str(), d.len()))
				.collect();
			pushed.sort();
			assert_eq!(
				pushed,
				[
					("/sdcard/Android/data/com.example.game/files/extra.bin", 14),
					("/sdcard/Android/obb/com.example.game/main.obb", 300_000),
				]
			);
			assert!(
				!s.dir.path().join("downloads").join(RELEASE).exists(),
				"files are removed after installing when keep_downloads is off"
			);
		}

		#[test]
		fn download_only_keeps_unpacked_files() {
			let s = setup(1);
			s.engine.send(Command::Download {
				game: game(RELEASE),
				serial: None,
			});
			let done = wait_finished(&s.engine, None);
			assert_eq!(done.state, TaskState::Done, "{:?}", done);
			let output = done.output.expect("download-only keeps the files");
			assert!(output.join(RELEASE).join("install.txt").is_file());
			assert!(
				output.join(RELEASE).join("extra.bin").is_file(),
				"nested archive unpacked"
			);
			assert!(
				!output.join(RELEASE).join("_data.7z").exists(),
				"nested archive removed"
			);
			assert!(
				!output.with_file_name("archive").exists(),
				"volumes removed after unpacking"
			);
			assert!(s.fake.recorded.lock().unwrap().pushed.is_empty());
		}

		#[test]
		fn queued_job_can_be_cancelled_and_missing_release_fails() {
			let s = setup(1);
			s.engine.send(Command::Download {
				game: game("Not On Mirror v1"),
				serial: None,
			});
			s.engine.send(Command::Download {
				game: game(RELEASE),
				serial: None,
			});
			// With one slot the second job waits; cancel it before it starts.
			s.engine.send(Command::CancelTask(2));
			let mut finished = HashMap::new();
			let deadline = Instant::now() + Duration::from_secs(20);
			while finished.len() < 2 {
				assert!(
					Instant::now() < deadline,
					"tasks did not finish: {finished:?}"
				);
				for event in s.engine.poll_events() {
					if let Event::TaskUpdated(t) = event
						&& t.state.is_finished()
					{
						finished.insert(t.id, t.state);
					}
				}
				std::thread::sleep(Duration::from_millis(20));
			}
			assert!(matches!(finished[&1], TaskState::Failed(_)), "{finished:?}");
			assert_eq!(finished[&2], TaskState::Cancelled);
		}
	}
}
