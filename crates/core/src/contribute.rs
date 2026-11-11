//! Contributing app versions the mirror doesn't have yet: finding them among
//! the headset's apps and packing them the way the mirror's uploads expect.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::catalog::Catalog;

/// The headset's own software, never worth offering.
const SYSTEM_PREFIXES: [&str; 6] = [
	"com.oculus.",
	"com.meta.",
	"com.android.",
	"com.qualcomm.",
	"horizonos.",
	"android.",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
	NotInLibrary,
	Newer { library_version: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
	pub package: String,
	/// Name from the library, or the package name when the library doesn't know it.
	pub name: String,
	pub version_code: u64,
	pub reason: Reason,
}

/// Installed apps the library lacks, or has only in older versions.
pub fn find_candidates(
	catalog: &Catalog,
	installed: &[(&str, Option<u64>)],
	ignored: &HashSet<String>,
) -> Vec<Candidate> {
	let mut library: HashMap<&str, (u64, &str)> = HashMap::new();
	for game in &catalog.games {
		let version = game.version_code.unwrap_or(0);
		let entry = library
			.entry(game.package.as_str())
			.or_insert((version, game.name.as_str()));
		if version > entry.0 {
			*entry = (version, game.name.as_str());
		}
	}

	let mut candidates: Vec<Candidate> = installed
		.iter()
		.filter_map(|&(package, version)| {
			let version_code = version?;
			let skip = SYSTEM_PREFIXES.iter().any(|p| package.starts_with(p))
				|| catalog.is_blacklisted(package)
				|| ignored.contains(package);
			if skip {
				return None;
			}
			let (name, reason) = match library.get(package) {
				None => (package.to_owned(), Reason::NotInLibrary),
				Some(&(library_version, _)) if version_code <= library_version => return None,
				Some(&(library_version, name)) => {
					(name.to_owned(), Reason::Newer { library_version })
				}
			};
			Some(Candidate {
				package: package.to_owned(),
				name,
				version_code,
				reason,
			})
		})
		.collect();
	candidates.sort_by_key(|c| c.name.to_lowercase());
	candidates
}

/// Stable anonymous id of a headset: SHA-256 of its serial, in hex.
pub fn hwid(serial: &str) -> String {
	Sha256::digest(serial.as_bytes())
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect()
}

/// `<Name> v<version> <package> <first hwid char> <model>.zip`, with characters
/// that file systems or the mirror can't take replaced.
pub fn archive_name(
	name: &str,
	version_code: u64,
	package: &str,
	hwid: &str,
	model: &str,
) -> String {
	let clean = |s: &str| -> String {
		s.chars()
			.map(|c| {
				if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
					'_'
				} else {
					c
				}
			})
			.collect::<String>()
			.trim()
			.to_owned()
	};
	let prefix = hwid.chars().next().unwrap_or('0');
	format!(
		"{} v{version_code} {package} {prefix} {}.zip",
		clean(name),
		clean(model)
	)
}

/// Zips everything under `dir` (paths relative to it). APKs and OBBs are
/// already compressed, so entries are stored as-is.
pub fn pack(dir: &Path, zip_path: &Path, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
	let files = crate::adb::list_files(dir)?;
	let total: u64 = files.iter().map(|f| f.size).sum();
	let out = std::fs::File::create(zip_path)?;
	let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(out));
	let mut done = 0u64;
	let mut buf = vec![0u8; 1024 * 1024];
	for file in &files {
		let options = zip::write::SimpleFileOptions::default()
			.compression_method(zip::CompressionMethod::Stored)
			.large_file(file.size >= u64::from(u32::MAX));
		writer
			.start_file(file.relative.as_str(), options)
			.map_err(std::io::Error::other)?;
		let mut input = std::fs::File::open(&file.path)?;
		loop {
			let n = input.read(&mut buf)?;
			if n == 0 {
				break;
			}
			writer.write_all(&buf[..n])?;
			done += n as u64;
			progress(done, total);
		}
	}
	writer.finish().map_err(std::io::Error::other)?.flush()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn catalog(dir: &Path) -> Catalog {
		let current = dir.join("m/current");
		std::fs::create_dir_all(current.join(".meta/nouns")).unwrap();
		std::fs::write(
			current.join("VRP-GameList.txt"),
			"Game Name;Release Name;Package Name;Version Code\n\
			 Beat Game;Beat Game v10;com.beat.game;10\n\
			 Beat Game;Beat Game v12;com.beat.game;12\n\
			 Old Game;Old Game v5;com.old.game;5\n",
		)
		.unwrap();
		std::fs::write(current.join(".meta/nouns/blacklist.txt"), "com.discord\n").unwrap();
		Catalog::load(dir, "m").unwrap().unwrap()
	}

	#[test]
	fn candidates() {
		let dir = tempfile::tempdir().unwrap();
		let catalog = catalog(dir.path());
		let installed = [
			("com.beat.game", Some(12)),
			("com.old.game", Some(7)),
			("com.new.game", Some(1)),
			("com.discord", Some(5)),
			("com.oculus.browser", Some(99)),
			("com.no.version", None),
			("com.ignored.game", Some(3)),
		];
		let ignored = HashSet::from(["com.ignored.game".to_owned()]);
		let found = find_candidates(&catalog, &installed, &ignored);
		assert_eq!(
			found,
			[
				Candidate {
					package: "com.new.game".into(),
					name: "com.new.game".into(),
					version_code: 1,
					reason: Reason::NotInLibrary,
				},
				Candidate {
					package: "com.old.game".into(),
					name: "Old Game".into(),
					version_code: 7,
					reason: Reason::Newer { library_version: 5 },
				},
			]
		);
	}

	#[test]
	fn ids_and_names() {
		// Same as `printf SER1 | sha256sum`.
		assert_eq!(
			hwid("SER1"),
			"1bf1c3aab5236eabd5c5ab5fa66f432d81c3f49986631d6cc6281491d3ed238e"
		);
		assert_eq!(
			archive_name("Game: Deluxe", 42, "com.x.y", "1bf1", "Quest 3S"),
			"Game_ Deluxe v42 com.x.y 1 Quest 3S.zip"
		);
	}

	#[test]
	fn packs_folder_into_zip() {
		let dir = tempfile::tempdir().unwrap();
		let src = dir.path().join("com.x.y");
		std::fs::create_dir_all(src.join("com.x.y")).unwrap();
		std::fs::write(src.join("com.x.y.apk"), b"apk").unwrap();
		std::fs::write(src.join("com.x.y/main.1.com.x.y.obb"), b"obb").unwrap();
		std::fs::write(src.join("HWID.txt"), b"abc").unwrap();
		let zip_path = dir.path().join("out.zip");
		pack(&src, &zip_path, &|_, _| {}).unwrap();

		let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
		let mut names: Vec<String> = archive
			.file_names()
			.map(|n| n.unwrap().into_owned())
			.collect();
		names.sort();
		assert_eq!(
			names,
			["HWID.txt", "com.x.y.apk", "com.x.y/main.1.com.x.y.obb"]
		);
		let mut content = String::new();
		archive
			.by_name("com.x.y/main.1.com.x.y.obb")
			.unwrap()
			.read_to_string(&mut content)
			.unwrap();
		assert_eq!(content, "obb");
	}
}
