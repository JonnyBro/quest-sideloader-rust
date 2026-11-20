use rust_i18n::t;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::arsc::ResourceTable;
use super::axml::{Attribute, Axml};
use super::dex;
use super::token::{expand_class, mentions, rename_value};
use crate::adb::list_files;
use crate::sideload::{InstallPlan, SideloadError, Source, is_package_name};

pub const PLATFORM_LOADER: &str = "lib/arm64-v8a/libovrplatformloader.so";
/// Enough to show what kind of strings they are without flooding the screen.
const MAX_EXAMPLES: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
	pub title: String,
	pub root: PathBuf,
	pub apk: PathBuf,
	pub package: String,
	pub version_code: Option<u64>,
	pub version_name: Option<String>,
	pub manifest: Vec<ManifestChange>,
	pub resources: Vec<String>,
	/// Code strings that mention the package; these stay as they are.
	pub dex_strings: Vec<String>,
	pub dex_count: usize,
	/// Native libraries with JNI functions named after the package.
	pub jni_libraries: Vec<String>,
	pub firebase: bool,
	pub platform_loader: Option<String>,
	pub obb_files: Vec<String>,
	pub script_lines: Vec<String>,
	pub other_apks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
	/// The application id, an authority, permission or action.
	Identity,
	/// A class name relative to the package, spelled out so it keeps working.
	ExpandClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestChange {
	pub element: String,
	pub attribute: String,
	pub value: String,
	pub kind: ChangeKind,
}

impl ManifestChange {
	pub fn new_value(&self, old: &str, new: &str) -> String {
		match self.kind {
			ChangeKind::Identity => {
				rename_value(&self.value, old, new).unwrap_or_else(|| self.value.clone())
			}
			ChangeKind::ExpandClass => {
				expand_class(&self.value, old).unwrap_or_else(|| self.value.clone())
			}
		}
	}
}

const COMPONENTS: [&str; 7] = [
	"application",
	"activity",
	"activity-alias",
	"service",
	"receiver",
	"provider",
	"instrumentation",
];
/// Attributes that always hold a class name, on any element.
const CLASS_ATTRIBUTES: [&str; 5] = [
	"targetActivity",
	"parentActivityName",
	"backupAgent",
	"appComponentFactory",
	"zygotePreloadName",
];
const PARENT_ACTIVITY: [&str; 2] = [
	"android.support.PARENT_ACTIVITY",
	"androidx.core.PARENT_ACTIVITY",
];
const ATTR_NAME: u32 = 0x0101_0003;
const ATTR_VALUE: u32 = 0x0101_0024;

pub fn manifest_changes(xml: &Axml, package: &str) -> Result<Vec<ManifestChange>, String> {
	Ok(planned_changes(xml, package)?
		.into_iter()
		.map(|(_, change)| change)
		.collect())
}

pub(super) fn planned_changes(
	xml: &Axml,
	package: &str,
) -> Result<Vec<(Attribute, ManifestChange)>, String> {
	let mut changes = Vec::new();
	let elements = xml.elements().map_err(|e| e.to_string())?;
	for element in &elements {
		// Names of other apps the game talks to.
		if element.name == "queries" || element.ancestors.iter().any(|a| a == "queries") {
			continue;
		}
		let is_component = COMPONENTS.contains(&element.name.as_str());
		let parent_activity = element.name == "meta-data"
			&& element.attributes.iter().any(|a| {
				a.is_android("name", ATTR_NAME)
					&& a.value
						.as_deref()
						.is_some_and(|v| PARENT_ACTIVITY.contains(&v))
			});
		for attribute in &element.attributes {
			let Some(value) = &attribute.value else {
				continue;
			};
			let kind = if element.name == "manifest" && attribute.name == "package" {
				(value == package).then_some(ChangeKind::Identity)
			} else if is_class(attribute, is_component, parent_activity) {
				expand_class(value, package).map(|_| ChangeKind::ExpandClass)
			} else if attribute.name == "targetPackage" {
				None
			} else {
				mentions(value, package).then_some(ChangeKind::Identity)
			};
			if let Some(kind) = kind {
				changes.push((
					attribute.clone(),
					ManifestChange {
						element: element.name.clone(),
						attribute: attribute.name.clone(),
						value: value.clone(),
						kind,
					},
				));
			}
		}
	}
	Ok(changes)
}

fn is_class(attribute: &Attribute, is_component: bool, parent_activity: bool) -> bool {
	(is_component && attribute.is_android("name", ATTR_NAME))
		|| (parent_activity && attribute.is_android("value", ATTR_VALUE))
		|| CLASS_ATTRIBUTES.contains(&attribute.name.as_str())
}

/// Reads the release (a folder or an APK) and works out what renaming it
/// would change. Slow for big games: every native library is read.
pub fn analyze(path: &Path) -> Result<Analysis, SideloadError> {
	let plan = match Source::classify(path)? {
		Source::Apk(apk) => InstallPlan::from_apk(&apk)?,
		Source::Folder(dir) => InstallPlan::from_folder(&dir)?,
		Source::Zip(_) => return Err(SideloadError::Unsupported(path.to_path_buf())),
	};
	let Some(meta) = plan.apks.first() else {
		return Err(SideloadError::NothingToInstall(path.to_path_buf()));
	};
	let apk = meta.path.clone();
	let package = meta.package.clone();
	let apk_err = |message: String| SideloadError::Apk {
		path: apk.clone(),
		message,
	};

	let file = std::fs::File::open(&apk).map_err(crate::sideload::io_err(&apk))?;
	let mut zip =
		zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| apk_err(e.to_string()))?;
	let mut read = |name: &str| -> Result<Option<Vec<u8>>, SideloadError> {
		let mut entry = match zip.by_name(name) {
			Ok(entry) => entry,
			Err(zip::result::ZipError::FileNotFound) => return Ok(None),
			Err(e) => return Err(apk_err(e.to_string())),
		};
		let mut data = Vec::with_capacity(entry.size().min(1 << 30) as usize);
		entry
			.read_to_end(&mut data)
			.map_err(|e| apk_err(e.to_string()))?;
		Ok(Some(data))
	};

	let manifest_data =
		read("AndroidManifest.xml")?.ok_or_else(|| apk_err(t!("ren.no_manifest").into_owned()))?;
	let xml = Axml::parse(&manifest_data).map_err(|e| apk_err(e.to_string()))?;
	let manifest = manifest_changes(&xml, &package).map_err(apk_err)?;

	let mut resources = Vec::new();
	let mut firebase = false;
	if let Some(data) = read("resources.arsc")? {
		let table = ResourceTable::parse(&data).map_err(|e| apk_err(e.to_string()))?;
		firebase = table.has_key("google_app_id");
		resources = table
			.strings
			.iter()
			.filter(|s| mentions(s, &package))
			.map(str::to_owned)
			.collect();
	}
	let platform_loader = read(PLATFORM_LOADER)?.map(|data| hex(&Sha256::digest(&data)));

	let names: Vec<String> = zip
		.file_names()
		.filter_map(|n| n.ok().map(|n| n.into_owned()))
		.collect();
	let mut dex_strings = Vec::new();
	let mut dex_count = 0;
	let mut jni_libraries = Vec::new();
	let jni = jni_prefix(&package);
	for name in &names {
		let is_dex = name.starts_with("classes") && name.ends_with(".dex") && !name.contains('/');
		let is_lib = name.starts_with("lib/") && name.ends_with(".so");
		if !is_dex && !is_lib {
			continue;
		}
		let mut entry = zip.by_name(name).map_err(|e| apk_err(e.to_string()))?;
		if is_lib {
			if stream_contains(&mut entry, jni.as_bytes()).map_err(|e| apk_err(e.to_string()))? {
				jni_libraries.push(name.clone());
			}
			continue;
		}
		let mut data = Vec::new();
		entry
			.read_to_end(&mut data)
			.map_err(|e| apk_err(e.to_string()))?;
		dex::for_each_string(&data, |s| {
			if mentions(s, &package) {
				dex_count += 1;
				if dex_strings.len() < MAX_EXAMPLES && !dex_strings.iter().any(|d| d == s) {
					dex_strings.push(s.to_owned());
				}
			}
		})
		.map_err(|e| apk_err(format!("{name}: {e}")))?;
	}

	let obb_files = match plan.obbs.iter().find(|o| o.package == package) {
		Some(obb) => list_files(&obb.path)
			.map_err(crate::sideload::io_err(&obb.path))?
			.into_iter()
			.map(|f| f.relative)
			.collect(),
		None => Vec::new(),
	};
	let script_lines = match &plan.script {
		Some(script) => std::fs::read_to_string(script)
			.map_err(crate::sideload::io_err(script))?
			.lines()
			.filter(|l| mentions(l, &package))
			.map(|l| l.trim().to_owned())
			.collect(),
		None => Vec::new(),
	};

	Ok(Analysis {
		title: plan.title.clone(),
		root: plan.root.clone(),
		apk: apk.clone(),
		package,
		version_code: meta.version_code,
		version_name: meta.version_name.clone(),
		manifest,
		resources,
		dex_strings,
		dex_count,
		jni_libraries,
		firebase,
		platform_loader,
		obb_files,
		script_lines,
		other_apks: plan.apks.len() - 1,
	})
}

/// `main.1.com.game.obb` becomes `main.1.com.copy.obb`; other files keep their names.
pub fn obb_name(name: &str, old: &str, new: &str) -> String {
	let Some(stem) = name.strip_suffix(&format!(".{old}.obb")) else {
		return name.to_owned();
	};
	let mut parts = stem.splitn(2, '.');
	match (parts.next(), parts.next()) {
		(Some(kind @ ("main" | "patch")), Some(version))
			if version.bytes().all(|b| b.is_ascii_digit()) =>
		{
			format!("{kind}.{version}.{new}.obb")
		}
		_ => name.to_owned(),
	}
}

pub fn suggest_package(old: &str) -> String {
	format!("{old}.copy")
}

pub fn check_package(new: &str, old: &str) -> Option<String> {
	if !is_package_name(new) {
		Some(t!("ren.bad_package").into_owned())
	} else if new == old {
		Some(t!("ren.same_package").into_owned())
	} else {
		None
	}
}

fn jni_prefix(package: &str) -> String {
	let mut out = String::from("Java_");
	for c in package.chars() {
		match c {
			'.' => out.push('_'),
			'_' => out.push_str("_1"),
			c => out.push(c),
		}
	}
	out.push('_');
	out
}

fn stream_contains(reader: &mut impl Read, needle: &[u8]) -> std::io::Result<bool> {
	let mut buf = vec![0u8; 1 << 20];
	let mut kept = 0;
	loop {
		let n = reader.read(&mut buf[kept..])?;
		if n == 0 {
			return Ok(false);
		}
		let filled = kept + n;
		let hay = &buf[..filled];
		if hay.len() >= needle.len()
			&& (0..=hay.len() - needle.len())
				.any(|i| hay[i] == needle[0] && &hay[i..i + needle.len()] == needle)
		{
			return Ok(true);
		}
		// A match can straddle two reads.
		kept = (needle.len() - 1).min(filled);
		buf.copy_within(filled - kept..filled, 0);
	}
}

fn hex(bytes: &[u8]) -> String {
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rename::axml::tests::manifest;

	#[test]
	fn finds_what_the_manifest_changes() {
		let data = manifest(&[
			("manifest", 0, &[(false, "package", "com.game")]),
			(
				"permission",
				1,
				&[(true, "name", "com.game.permission.C2D_MESSAGE")],
			),
			("application", 1, &[(true, "name", ".App")]),
			("activity", 2, &[(true, "name", "com.game.ui.Main")]),
			(
				"provider",
				2,
				&[
					(true, "authorities", "com.game.files"),
					(true, "name", "androidx.core.content.FileProvider"),
				],
			),
			(
				"meta-data",
				2,
				&[
					(true, "name", "com.oculus.supportedDevices"),
					(true, "value", "quest2|quest3"),
				],
			),
			("queries", 1, &[]),
			("package", 2, &[(true, "name", "com.game")]),
		]);
		let xml = Axml::parse(&data).unwrap();
		let changes = manifest_changes(&xml, "com.game").unwrap();
		let summary: Vec<(&str, &str, String)> = changes
			.iter()
			.map(|c| {
				(
					c.element.as_str(),
					c.attribute.as_str(),
					c.new_value("com.game", "com.copy"),
				)
			})
			.collect();
		assert_eq!(
			summary,
			[
				("manifest", "package", "com.copy".to_owned()),
				(
					"permission",
					"name",
					"com.copy.permission.C2D_MESSAGE".to_owned()
				),
				("application", "name", "com.game.App".to_owned()),
				("provider", "authorities", "com.copy.files".to_owned()),
			]
		);
	}

	#[test]
	fn obb_names_follow_the_package() {
		assert_eq!(
			obb_name("main.12.com.game.obb", "com.game", "com.copy"),
			"main.12.com.copy.obb"
		);
		assert_eq!(
			obb_name("patch.3.com.game.obb", "com.game", "com.copy"),
			"patch.3.com.copy.obb"
		);
		assert_eq!(
			obb_name("extra/data.bin", "com.game", "com.copy"),
			"extra/data.bin"
		);
		assert_eq!(
			obb_name("main.x.com.game.obb", "com.game", "com.copy"),
			"main.x.com.game.obb"
		);
	}

	#[test]
	fn finds_split_jni_names() {
		assert_eq!(jni_prefix("com.my_game"), "Java_com_my_1game_");
		let mut data = vec![0u8; (1 << 20) - 3];
		data.extend_from_slice(b"Java_com_a_Main");
		assert!(stream_contains(&mut data.as_slice(), b"Java_com_a_").unwrap());
		assert!(!stream_contains(&mut data.as_slice(), b"Java_com_b_").unwrap());
	}

	#[test]
	fn checks_the_new_package() {
		assert!(check_package("com.copy", "com.game").is_none());
		assert!(check_package("com.game", "com.game").is_some());
		assert!(check_package("copy", "com.game").is_some());
		assert!(check_package("com.1x", "com.game").is_some());
	}
}
