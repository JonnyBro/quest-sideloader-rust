//! Android SDK Platform-Tools (adb) downloaded from Google into the data folder.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rust_i18n::t;
use tokio::io::AsyncWriteExt;

/// Release notes and manual downloads.
pub const INFO_URL: &str = "https://developer.android.com/tools/releases/platform-tools";
/// Terms the user accepts by downloading the package.
pub const TERMS_URL: &str = "https://developer.android.com/studio/terms";
/// Meta's USB driver, which Windows needs to see the headset.
pub const QUEST_DRIVER_URL: &str =
	"https://developers.meta.com/horizon/downloads/package/oculus-adb-drivers/";

/// Google only publishes x86-64 builds for Linux and Windows; the macOS one is universal.
pub fn download_url() -> Option<&'static str> {
	match (std::env::consts::OS, std::env::consts::ARCH) {
		("windows", "x86_64" | "aarch64") => {
			Some("https://dl.google.com/android/repository/platform-tools-latest-windows.zip")
		}
		("linux", "x86_64") => {
			Some("https://dl.google.com/android/repository/platform-tools-latest-linux.zip")
		}
		("macos", _) => {
			Some("https://dl.google.com/android/repository/platform-tools-latest-darwin.zip")
		}
		_ => None,
	}
}

pub fn adb_path(dir: &Path) -> PathBuf {
	dir.join(if cfg!(windows) { "adb.exe" } else { "adb" })
}

/// Downloads the package and unpacks it into `dir` (replacing what was
/// there). Returns the path of the adb binary.
pub async fn install(
	dir: &Path,
	progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<PathBuf, String> {
	let url = download_url().ok_or_else(|| t!("pt.unsupported").into_owned())?;
	let parent = dir.parent().unwrap_or(dir).to_path_buf();
	tokio::fs::create_dir_all(&parent)
		.await
		.map_err(|e| e.to_string())?;
	let archive = parent.join("platform-tools.zip.part");
	download(url, &archive, progress).await?;

	let staging = parent.join("platform-tools.new");
	let (zip, out) = (archive.clone(), staging.clone());
	tokio::task::spawn_blocking(move || extract(&zip, &out))
		.await
		.map_err(|e| e.to_string())??;
	let _ = tokio::fs::remove_file(&archive).await;

	// The archive holds a single `platform-tools/` folder.
	let unpacked = staging.join("platform-tools");
	if !adb_path(&unpacked).is_file() {
		let _ = tokio::fs::remove_dir_all(&staging).await;
		return Err(t!("pt.no_adb").into_owned());
	}
	if dir.exists() {
		tokio::fs::remove_dir_all(dir)
			.await
			.map_err(|e| e.to_string())?;
	}
	tokio::fs::rename(&unpacked, dir)
		.await
		.map_err(|e| e.to_string())?;
	let _ = tokio::fs::remove_dir_all(&staging).await;
	tracing::info!("platform-tools installed to {}", dir.display());
	Ok(adb_path(dir))
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
	reqwest::Client::builder()
		.user_agent(concat!("quest-sideloader-rust/", env!("CARGO_PKG_VERSION")))
		.connect_timeout(Duration::from_secs(20))
		.read_timeout(Duration::from_secs(60))
		.build()
		.map_err(|e| e.to_string())
}

pub(crate) async fn download(
	url: &str,
	dest: &Path,
	progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<(), String> {
	let client = http_client()?;
	let mut response = client
		.get(url)
		.send()
		.await
		.map_err(|e| t!("err.cat_http", error = e).into_owned())?;
	if !response.status().is_success() {
		return Err(t!(
			"err.cat_status",
			status = response.status().as_u16(),
			url = url
		)
		.into_owned());
	}
	let total = response.content_length();
	let mut file = tokio::fs::File::create(dest)
		.await
		.map_err(|e| e.to_string())?;
	let mut done = 0;
	progress(done, total);
	while let Some(chunk) = response
		.chunk()
		.await
		.map_err(|e| t!("err.cat_http", error = e).into_owned())?
	{
		file.write_all(&chunk).await.map_err(|e| e.to_string())?;
		done += chunk.len() as u64;
		progress(done, total);
	}
	file.flush().await.map_err(|e| e.to_string())
}

fn extract(archive: &Path, out: &Path) -> Result<(), String> {
	let _ = std::fs::remove_dir_all(out);
	let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
	let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
		.map_err(|e| t!("err.corrupt", error = e).into_owned())?;
	for i in 0..zip.len() {
		let mut entry = zip
			.by_index(i)
			.map_err(|e| t!("err.corrupt", error = e).into_owned())?;
		let Some(relative) = entry.enclosed_name() else {
			return Err(t!("err.unsafe_path", entry = format!("{:?}", entry.name())).into_owned());
		};
		let path = out.join(relative);
		if entry.is_dir() {
			std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
			continue;
		}
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
		}
		let mut bytes = Vec::new();
		entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
		std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
		#[cfg(unix)]
		if let Some(mode) = entry.unix_mode() {
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777))
				.map_err(|e| e.to_string())?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn extracts_platform_tools_layout() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("pt.zip");
		{
			let file = std::fs::File::create(&archive).unwrap();
			let mut zip = zip::ZipWriter::new(file);
			let options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
			zip.add_directory("platform-tools/", options).unwrap();
			zip.start_file(
				format!(
					"platform-tools/{}",
					if cfg!(windows) { "adb.exe" } else { "adb" }
				),
				options,
			)
			.unwrap();
			std::io::Write::write_all(&mut zip, b"binary").unwrap();
			zip.finish().unwrap();
		}
		let out = dir.path().join("out");
		extract(&archive, &out).unwrap();
		let adb = adb_path(&out.join("platform-tools"));
		assert_eq!(std::fs::read(&adb).unwrap(), b"binary");
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			assert_eq!(
				std::fs::metadata(&adb).unwrap().permissions().mode() & 0o111,
				0o111
			);
		}
	}
}
