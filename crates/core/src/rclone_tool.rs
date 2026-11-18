//! The official rclone build, downloaded from rclone.org into the data folder.

use std::io::Read;
use std::path::{Path, PathBuf};

use rust_i18n::t;
use sha2::{Digest, Sha256};

const BASE: &str = "https://downloads.rclone.org";
pub const INFO_URL: &str = "https://rclone.org/downloads/";

/// rclone's name for this platform in its release file names.
pub fn platform() -> Option<&'static str> {
	Some(match (std::env::consts::OS, std::env::consts::ARCH) {
		("windows", "x86_64") => "windows-amd64",
		("windows", "aarch64") => "windows-arm64",
		("linux", "x86_64") => "linux-amd64",
		("linux", "aarch64") => "linux-arm64",
		("linux", "arm") => "linux-arm-v7",
		("macos", "x86_64") => "osx-amd64",
		("macos", "aarch64") => "osx-arm64",
		_ => return None,
	})
}

pub fn binary_path(dir: &Path) -> PathBuf {
	dir.join(if cfg!(windows) {
		"rclone.exe"
	} else {
		"rclone"
	})
}

/// Downloads the current release, checks it against the published SHA-256
/// sums and puts the binary into `dir`. Returns the binary's path.
pub async fn install(
	dir: &Path,
	progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
) -> Result<PathBuf, String> {
	let platform = platform().ok_or_else(|| t!("rc.unsupported").into_owned())?;
	let client = crate::platform_tools::http_client()?;
	let version = get_text(&client, &format!("{BASE}/version.txt")).await?;
	let version = version
		.split_whitespace()
		.find(|w| w.starts_with('v'))
		.ok_or_else(|| t!("rc.bad_version").into_owned())?
		.to_owned();
	let file = format!("rclone-{version}-{platform}.zip");
	let sums = get_text(&client, &format!("{BASE}/{version}/SHA256SUMS")).await?;
	let expected =
		expected_sum(&sums, &file).ok_or_else(|| t!("rc.no_sum", file = file).into_owned())?;

	tokio::fs::create_dir_all(dir)
		.await
		.map_err(|e| e.to_string())?;
	let archive = dir.join("rclone.zip.part");
	crate::platform_tools::download(&format!("{BASE}/{version}/{file}"), &archive, progress)
		.await?;
	let bytes = tokio::fs::read(&archive).await.map_err(|e| e.to_string())?;
	let actual: String = Sha256::digest(&bytes)
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect();
	if actual != expected {
		let _ = tokio::fs::remove_file(&archive).await;
		return Err(t!("rc.bad_sum", file = file).into_owned());
	}

	let zip = archive.clone();
	let binary = tokio::task::spawn_blocking(move || extract_binary(&zip))
		.await
		.map_err(|e| e.to_string())??;
	let _ = tokio::fs::remove_file(&archive).await;
	let path = binary_path(dir);
	let tmp = path.with_extension("new");
	tokio::fs::write(&tmp, binary)
		.await
		.map_err(|e| e.to_string())?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
			.await
			.map_err(|e| e.to_string())?;
	}
	tokio::fs::rename(&tmp, &path)
		.await
		.map_err(|e| e.to_string())?;
	tracing::info!(%version, sha256 = %actual, "rclone installed to {}", path.display());
	Ok(path)
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
	let response = client
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
	response
		.text()
		.await
		.map_err(|e| t!("err.cat_http", error = e).into_owned())
}

/// The rclone executable inside a release zip (`rclone-v…/rclone`).
fn extract_binary(zip_path: &Path) -> Result<Vec<u8>, String> {
	let unreadable = |e: &dyn std::fmt::Display| t!("rc.zip_unreadable", error = e).into_owned();
	let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
	let mut archive =
		zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| unreadable(&e))?;
	for i in 0..archive.len() {
		let mut entry = archive.by_index(i).map_err(|e| unreadable(&e))?;
		let name = entry
			.name()
			.map_err(|e| unreadable(&e))?
			.rsplit('/')
			.next()
			.unwrap_or_default()
			.to_ascii_lowercase();
		if entry.is_file() && (name == "rclone" || name == "rclone.exe") {
			let mut bytes = Vec::new();
			entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
			return Ok(bytes);
		}
	}
	Err(t!("rc.zip_no_rclone").into_owned())
}

/// The sum for `file` from a `SHA256SUMS` listing (`<hex>  <name>` lines).
fn expected_sum(sums: &str, file: &str) -> Option<String> {
	sums.lines().find_map(|line| {
		let mut parts = line.split_whitespace();
		let (hash, name) = (parts.next()?, parts.next()?);
		(name.trim_start_matches('*') == file
			&& hash.len() == 64
			&& hash.bytes().all(|b| b.is_ascii_hexdigit()))
		.then(|| hash.to_ascii_lowercase())
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn finds_rclone_inside_zip() {
		use std::io::Write;
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("rclone.zip");
		let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
		let options = zip::write::SimpleFileOptions::default();
		writer
			.start_file("rclone-v1-linux-amd64/README.txt", options)
			.unwrap();
		writer.write_all(b"docs").unwrap();
		writer
			.start_file("rclone-v1-linux-amd64/rclone", options)
			.unwrap();
		writer.write_all(b"ELF...").unwrap();
		writer.finish().unwrap();
		assert_eq!(extract_binary(&path).unwrap(), b"ELF...");
	}

	#[test]
	fn finds_the_sum_in_a_signed_listing() {
		let hash = "a".repeat(64);
		let sums = format!(
			"-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA1\n\n{} rclone-v1.0.0-linux-arm64.zip\n{hash}  rclone-v1.0.0-linux-amd64.zip\n-----BEGIN PGP SIGNATURE-----\n",
			"b".repeat(64)
		);
		assert_eq!(
			expected_sum(&sums, "rclone-v1.0.0-linux-amd64.zip"),
			Some(hash)
		);
		assert_eq!(expected_sum(&sums, "rclone-v1.0.0-osx-amd64.zip"), None);
	}
}
