//! Builds the renamed copy of a release into a new folder next to the
//! downloads, leaving the original untouched.

use rust_i18n::t;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::analyze::{PLATFORM_LOADER, check_package, obb_name, planned_changes};
use super::arsc::ResourceTable;
use super::axml::Axml;
use super::keys::{KeyError, KeyStore};
use super::sign::{SignError, write_signed_apk};
use super::token::rename_value;
use super::verify::verify_v2;
use crate::adb::list_files;
use crate::sideload::run::{Reporter, Update};
use crate::sideload::{InstallPlan, SideloadError, Source};

#[derive(Debug, Clone)]
pub struct BuildRequest {
	/// A release folder or an APK.
	pub source: PathBuf,
	pub new_package: String,
	/// A replacement for the Platform SDK loader, chosen by the user.
	pub loader: Option<PathBuf>,
	pub dest_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
	#[error("{0}")]
	Invalid(String),
	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
	#[error(transparent)]
	Sideload(#[from] SideloadError),
	#[error(transparent)]
	Sign(#[from] SignError),
	#[error(transparent)]
	Key(#[from] KeyError),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> BuildError + '_ {
	move |source| BuildError::Io {
		path: path.to_path_buf(),
		source,
	}
}

pub fn output_dir(source: &Path, dest_dir: &Path, new_package: &str) -> PathBuf {
	let name = if source.is_dir() {
		source.file_name()
	} else {
		source.file_stem()
	}
	.map(|n| n.to_string_lossy().into_owned())
	.unwrap_or_else(|| "copy".into());
	dest_dir.join(format!("{name} ({new_package})"))
}

/// Builds the copy and returns its folder. Work happens in a hidden folder
/// that only gets its real name once everything is written and checked.
pub fn build(
	request: &BuildRequest,
	keys: &KeyStore,
	report: Reporter<'_>,
) -> Result<PathBuf, BuildError> {
	let plan = match Source::classify(&request.source)? {
		Source::Apk(apk) => InstallPlan::from_apk(&apk)?,
		Source::Folder(dir) => InstallPlan::from_folder(&dir)?,
		Source::Zip(_) => return Err(SideloadError::Unsupported(request.source.clone()).into()),
	};
	let meta = plan
		.apks
		.first()
		.ok_or_else(|| SideloadError::NothingToInstall(request.source.clone()))?;
	let old = meta.package.as_str();
	let new = request.new_package.trim();
	if let Some(problem) = check_package(new, old) {
		return Err(BuildError::Invalid(problem));
	}

	let out = output_dir(&request.source, &request.dest_dir, new);
	if out.exists() {
		return Err(BuildError::Invalid(
			t!("ren.exists", path = out.display()).into_owned(),
		));
	}
	let out_name = out.file_name().unwrap_or_default().to_string_lossy();
	let tmp = request.dest_dir.join(format!(".{out_name}.partial"));
	if tmp.exists() {
		std::fs::remove_dir_all(&tmp).map_err(io(&tmp))?;
	}
	std::fs::create_dir_all(&tmp).map_err(io(&tmp))?;
	let result = build_into(request, &plan, &meta.path, old, new, &tmp, keys, report)
		.and_then(|()| std::fs::rename(&tmp, &out).map_err(io(&out)));
	if result.is_err() {
		let _ = std::fs::remove_dir_all(&tmp);
	}
	result.map(|()| out)
}

#[allow(clippy::too_many_arguments)]
fn build_into(
	request: &BuildRequest,
	plan: &InstallPlan,
	apk: &Path,
	old: &str,
	new: &str,
	tmp: &Path,
	keys: &KeyStore,
	report: Reporter<'_>,
) -> Result<(), BuildError> {
	report(Update::Stage(t!("ren.stage_reading").into_owned()));
	let apk_err = |message: String| SideloadError::Apk {
		path: apk.to_path_buf(),
		message,
	};
	let file = std::fs::File::open(apk).map_err(io(apk))?;
	let mut zip =
		zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| apk_err(e.to_string()))?;
	let mut read = |name: &str| -> Result<Option<Vec<u8>>, SideloadError> {
		let mut entry = match zip.by_name(name) {
			Ok(entry) => entry,
			Err(zip::result::ZipError::FileNotFound) => return Ok(None),
			Err(e) => return Err(apk_err(e.to_string())),
		};
		let mut data = Vec::new();
		entry
			.read_to_end(&mut data)
			.map_err(|e| apk_err(e.to_string()))?;
		Ok(Some(data))
	};

	let mut lines = vec![
		"Quest Sideloader renamed copy".to_owned(),
		format!("Source: {}", request.source.display()),
		format!("Package: {old} -> {new}"),
		String::new(),
	];
	let mut replace = HashMap::new();

	let manifest =
		read("AndroidManifest.xml")?.ok_or_else(|| apk_err(t!("ren.no_manifest").into_owned()))?;
	let mut xml = Axml::parse(&manifest).map_err(|e| apk_err(e.to_string()))?;
	lines.push("Manifest:".into());
	for (attribute, change) in planned_changes(&xml, old).map_err(apk_err)? {
		let value = change.new_value(old, new);
		lines.push(format!(
			"  <{}> {}: {} -> {value}",
			change.element, change.attribute, change.value
		));
		xml.set_string(&attribute, &value);
	}
	replace.insert("AndroidManifest.xml".to_owned(), xml.write());

	if let Some(data) = read("resources.arsc")? {
		let mut table = ResourceTable::parse(&data).map_err(|e| apk_err(e.to_string()))?;
		let renamed: Vec<(u32, String)> = table
			.strings
			.iter()
			.enumerate()
			.filter_map(|(i, s)| Some((i as u32, rename_value(s, old, new)?)))
			.collect();
		if !renamed.is_empty() {
			lines.push("Resource strings:".into());
			for (index, value) in renamed {
				lines.push(format!(
					"  {} -> {value}",
					table.strings.get(index).unwrap_or_default()
				));
				table.strings.set(index, &value);
			}
			replace.insert("resources.arsc".to_owned(), table.write());
		}
	}

	if let Some(loader) = &request.loader {
		let original = read(PLATFORM_LOADER)?
			.ok_or_else(|| BuildError::Invalid(t!("ren.no_loader").into_owned()))?;
		let patched = std::fs::read(loader).map_err(io(loader))?;
		lines.push(String::new());
		lines.push(format!("Replaced {PLATFORM_LOADER}"));
		lines.push(format!(
			"  original SHA-256: {}",
			hex(&Sha256::digest(&original))
		));
		lines.push(format!(
			"  replacement SHA-256: {}",
			hex(&Sha256::digest(&patched))
		));
		lines.push(format!("  replacement file: {}", loader.display()));
		replace.insert(PLATFORM_LOADER.to_owned(), patched);
	}
	drop(read);
	drop(zip);

	report(Update::Stage(t!("ren.stage_key").into_owned()));
	let key = keys.load_or_create(new)?;
	lines.push(String::new());
	lines.push(format!(
		"Signing certificate SHA-256: {}",
		key.fingerprint()
	));

	report(Update::Stage(t!("ren.stage_signing").into_owned()));
	let apk_name = apk.file_name().unwrap_or_default();
	let out_apk = tmp.join(apk_name);
	write_signed_apk(apk, &out_apk, &replace, &key, &|done, total| {
		report(Update::Bytes { done, total })
	})?;
	report(Update::Stage(t!("ren.stage_checking").into_owned()));
	let certificate = verify_v2(&out_apk)
		.map_err(|e| BuildError::Invalid(t!("ren.bad_signature", error = e).into_owned()))?;
	if certificate != key.certificate() {
		return Err(BuildError::Invalid(
			t!("ren.bad_signature", error = "certificate").into_owned(),
		));
	}

	report(Update::Stage(t!("ren.stage_files").into_owned()));
	let is_folder = request.source.is_dir();
	if is_folder {
		copy_release(&plan.root, apk, old, new, tmp, &mut lines)?;
	} else if let Some(obb) = plan.obbs.iter().find(|o| o.package == old) {
		copy_obb(&obb.path, &tmp.join(new), old, new, &mut lines)?;
	}

	lines.push(String::new());
	let report_path = tmp.join("rename-report.txt");
	std::fs::write(&report_path, lines.join("\n") + "\n").map_err(io(&report_path))?;
	Ok(())
}

/// Everything else in the release folder: the package's OBB folder under the
/// new name, install.txt with the package replaced, the rest as it is.
fn copy_release(
	root: &Path,
	apk: &Path,
	old: &str,
	new: &str,
	tmp: &Path,
	lines: &mut Vec<String>,
) -> Result<(), BuildError> {
	let mut entries: Vec<_> = std::fs::read_dir(root)
		.map_err(io(root))?
		.collect::<Result<_, _>>()
		.map_err(io(root))?;
	entries.sort_by_key(|e| e.file_name());
	for entry in entries {
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		let file_type = entry.file_type().map_err(io(&path))?;
		if path == apk || file_type.is_symlink() {
			continue;
		}
		if file_type.is_dir() && name == old {
			copy_obb(&path, &tmp.join(new), old, new, lines)?;
		} else if file_type.is_dir() {
			copy_tree(&path, &tmp.join(&name))?;
		} else if name.eq_ignore_ascii_case("install.txt") {
			let text = std::fs::read_to_string(&path).map_err(io(&path))?;
			let renamed: Vec<String> = text
				.split('\n')
				.map(|line| rename_value(line, old, new).unwrap_or_else(|| line.to_owned()))
				.collect();
			let dest = tmp.join(&name);
			std::fs::write(&dest, renamed.join("\n")).map_err(io(&dest))?;
			lines.push(String::new());
			lines.push(format!("{name}: package replaced"));
		} else {
			link_or_copy(&path, &tmp.join(&name))?;
		}
	}
	Ok(())
}

fn copy_obb(
	from: &Path,
	to: &Path,
	old: &str,
	new: &str,
	lines: &mut Vec<String>,
) -> Result<(), BuildError> {
	lines.push(String::new());
	lines.push(format!("OBB folder: {old} -> {new}"));
	for file in list_files(from).map_err(io(from))? {
		let renamed = obb_name(&file.relative, old, new);
		if renamed != file.relative {
			lines.push(format!("  {} -> {renamed}", file.relative));
		}
		link_or_copy(&file.path, &to.join(&renamed))?;
	}
	Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), BuildError> {
	for file in list_files(from).map_err(io(from))? {
		link_or_copy(&file.path, &to.join(&file.relative))?;
	}
	Ok(())
}

/// OBBs can be gigabytes and never change, so the copy shares them with the
/// original when both are on the same drive.
fn link_or_copy(from: &Path, to: &Path) -> Result<(), BuildError> {
	if let Some(parent) = to.parent() {
		std::fs::create_dir_all(parent).map_err(io(parent))?;
	}
	if std::fs::hard_link(from, to).is_ok() {
		return Ok(());
	}
	std::fs::copy(from, to).map(|_| ()).map_err(io(to))
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rename::axml::tests::manifest;
	use crate::rename::keys::tests::test_key;
	use std::io::Write;

	fn release(dir: &Path) -> PathBuf {
		let root = dir.join("Game v1");
		std::fs::create_dir_all(root.join("com.game")).unwrap();
		std::fs::write(root.join("com.game/main.5.com.game.obb"), b"obb").unwrap();
		std::fs::write(root.join("com.game/extra.bin"), b"extra").unwrap();
		std::fs::write(
			root.join("install.txt"),
			"adb install game.apk\nadb push com.game /sdcard/Android/obb/\n",
		)
		.unwrap();
		std::fs::write(root.join("notes.txt"), b"hello").unwrap();

		let manifest = manifest(&[
			("manifest", 0, &[(false, "package", "com.game")]),
			("application", 1, &[(true, "name", ".App")]),
			("provider", 2, &[(true, "authorities", "com.game.files")]),
		]);
		let arsc =
			crate::rename::arsc::tests::table("com.game", &["com.game.files", "Hi"], &["app_name"]);
		let mut zip = zip::ZipWriter::new(std::fs::File::create(root.join("game.apk")).unwrap());
		let options = zip::write::SimpleFileOptions::default();
		zip.start_file("AndroidManifest.xml", options).unwrap();
		zip.write_all(&manifest).unwrap();
		zip.start_file("resources.arsc", options).unwrap();
		zip.write_all(&arsc).unwrap();
		zip.start_file(PLATFORM_LOADER, options).unwrap();
		zip.write_all(b"meta loader").unwrap();
		zip.finish().unwrap();
		root
	}

	#[test]
	fn builds_a_renamed_release() {
		let dir = tempfile::tempdir().unwrap();
		let root = release(dir.path());
		let keys = KeyStore::new(dir.path().join("signing"));
		let pem = dir.path().join("key.pem");
		std::fs::write(&pem, test_key().to_pem("com.copy").unwrap()).unwrap();
		keys.import(&pem).unwrap();
		let loader = dir.path().join("patched.so");
		std::fs::write(&loader, b"patched loader").unwrap();

		let request = BuildRequest {
			source: root.clone(),
			new_package: "com.copy".into(),
			loader: Some(loader),
			dest_dir: dir.path().to_path_buf(),
		};
		let out = build(&request, &keys, &|_| {}).unwrap();
		assert_eq!(out, dir.path().join("Game v1 (com.copy)"));
		assert_eq!(
			std::fs::read(out.join("com.copy/main.5.com.copy.obb")).unwrap(),
			b"obb"
		);
		assert_eq!(
			std::fs::read(out.join("com.copy/extra.bin")).unwrap(),
			b"extra"
		);
		assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"hello");
		assert_eq!(
			std::fs::read_to_string(out.join("install.txt")).unwrap(),
			"adb install game.apk\nadb push com.copy /sdcard/Android/obb/\n"
		);
		assert!(
			std::fs::read_to_string(out.join("rename-report.txt"))
				.unwrap()
				.contains("com.game -> com.copy")
		);
		assert_eq!(
			verify_v2(&out.join("game.apk")).unwrap(),
			test_key().certificate()
		);

		let meta = crate::sideload::read_apk_meta(&out.join("game.apk")).unwrap();
		assert_eq!(meta.package, "com.copy");
		let mut zip =
			zip::ZipArchive::new(std::fs::File::open(out.join("game.apk")).unwrap()).unwrap();
		let mut read = |name: &str| {
			let mut data = Vec::new();
			zip.by_name(name).unwrap().read_to_end(&mut data).unwrap();
			data
		};
		let xml = Axml::parse(&read("AndroidManifest.xml")).unwrap();
		let values: Vec<String> = xml
			.elements()
			.unwrap()
			.iter()
			.flat_map(|e| e.attributes.iter().filter_map(|a| a.value.clone()))
			.collect();
		assert_eq!(values, ["com.copy", "com.game.App", "com.copy.files"]);
		let table = ResourceTable::parse(&read("resources.arsc")).unwrap();
		assert_eq!(table.strings.get(0), Some("com.copy.files"));
		assert_eq!(read(PLATFORM_LOADER), b"patched loader");

		// The original is untouched and a second build doesn't overwrite.
		assert!(root.join("com.game/main.5.com.game.obb").exists());
		assert!(matches!(
			build(&request, &keys, &|_| {}),
			Err(BuildError::Invalid(_))
		));
	}
}
