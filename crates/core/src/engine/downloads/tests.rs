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
			conflict: None,
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
					let head_only = req.starts_with("HEAD ");
					let raw = req.split(' ').nth(1).unwrap_or("/");
					let path = percent_encoding::percent_decode_str(raw)
						.decode_utf8_lossy()
						.into_owned();
					let listing = path.ends_with('/');
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
					let kind = if listing {
						"text/html"
					} else {
						"application/octet-stream"
					};
					let response = match body {
						Some(body) => {
							let mut r = format!(
								"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {kind}\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\nConnection: close\r\n\r\n",
								body.len()
							)
							.into_bytes();
							if !head_only {
								r.extend(body);
							}
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

	/// The mirror is read through rclone, so these tests need it.
	fn have_rclone() -> bool {
		let found = std::process::Command::new("rclone")
			.arg("version")
			.output()
			.is_ok();
		if !found {
			eprintln!("rclone not installed, skipping");
		}
		found
	}

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
		if !have_rclone() {
			return;
		}
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
		if !have_rclone() {
			return;
		}
		let s = setup(1);
		s.engine.send(Command::Download {
			game: game(RELEASE),
			serial: None,
		});
		let done = wait_finished(&s.engine, None);
		assert_eq!(done.state, TaskState::Done, "{:?}", done);
		let output = done.output.expect("download-only keeps the files");
		assert_eq!(
			output,
			s.dir.path().join("downloads").join(RELEASE),
			"release sits in the downloads root"
		);
		assert!(
			output.join("install.txt").is_file(),
			"the archive's wrapping folder is unwrapped"
		);
		assert!(output.join("com.example.game/main.obb").is_file());
		assert!(
			output.join("extra.bin").is_file(),
			"nested archive unpacked"
		);
		assert!(!output.join("_data.7z").exists(), "nested archive removed");
		assert!(
			!s.dir
				.path()
				.join("downloads")
				.join(crate::downloaded::INCOMPLETE_DIR)
				.exists(),
			"temporary files removed after unpacking"
		);
		assert!(s.fake.recorded.lock().unwrap().pushed.is_empty());
	}

	#[test]
	fn queued_job_can_be_cancelled_and_missing_release_fails() {
		if !have_rclone() {
			return;
		}
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
