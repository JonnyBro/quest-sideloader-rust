//! The mirror's game list: `;`-separated text with a header row, e.g.
//! `Game Name;Release Name;Package Name;Version Code;Last Updated;Size (MB);Downloads`.

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
	pub name: String,
	pub release_name: String,
	pub package: String,
	pub version_code: Option<u64>,
	/// As written by the mirror (`YYYY-MM-DD HH:MM UTC`), so it sorts as text.
	pub last_updated: String,
	pub size_mb: Option<f64>,
	pub downloads: Option<f64>,
}

struct Columns {
	name: usize,
	release: usize,
	package: usize,
	version: Option<usize>,
	updated: Option<usize>,
	size: Option<usize>,
	downloads: Option<usize>,
}

impl Columns {
	fn from_header(header: &str) -> Option<Self> {
		let names: Vec<String> = header
			.split(';')
			.map(|c| c.trim().to_ascii_lowercase())
			.collect();
		let find = |wanted: &str| names.iter().position(|n| n == wanted);
		Some(Self {
			name: find("game name")?,
			release: find("release name")?,
			package: find("package name")?,
			version: find("version code"),
			updated: find("last updated"),
			size: find("size (mb)"),
			downloads: find("downloads"),
		})
	}
}

#[derive(Debug, thiserror::Error)]
#[error("the game list has no header with Game Name, Release Name and Package Name columns")]
pub struct BadHeader;

pub fn parse_game_list(text: &str) -> Result<Vec<Game>, BadHeader> {
	let text = text.trim_start_matches('\u{feff}');
	let mut lines = text.lines();
	let columns = lines
		.next()
		.and_then(Columns::from_header)
		.ok_or(BadHeader)?;

	let mut games = Vec::new();
	for line in lines {
		let fields: Vec<&str> = line.split(';').map(str::trim).collect();
		let get = |i: usize| fields.get(i).copied().unwrap_or("");
		let opt = |i: Option<usize>| i.map(get).filter(|v| !v.is_empty());

		let name = get(columns.name);
		let release_name = get(columns.release);
		let package = get(columns.package);
		if name.is_empty() || release_name.is_empty() || package.is_empty() {
			continue;
		}
		games.push(Game {
			name: name.to_owned(),
			release_name: release_name.to_owned(),
			package: package.to_owned(),
			version_code: opt(columns.version).and_then(|v| v.parse().ok()),
			last_updated: opt(columns.updated).unwrap_or_default().to_owned(),
			size_mb: opt(columns.size).and_then(|v| v.parse().ok()),
			downloads: opt(columns.downloads).and_then(|v| v.parse().ok()),
		});
	}
	Ok(games)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_typical_list() {
		let text = "\u{feff}Game Name;Release Name;Package Name;Version Code;Last Updated;Size (MB);Downloads;Rating;Rating Count\r\n\
			Beat Saber;Beat Saber v1.40.0+1.40;com.beatgames.beatsaber;1402;2024-11-02 10:15 UTC;1350;98.5;;\r\n\
			Broken line without fields\r\n\
			Pistol Whip;Pistol Whip v2.1 +2.1;com.cloudheadgames.pistolwhip;210;2024-01-05 00:00 UTC;;12\r\n\
			\r\n";
		let games = parse_game_list(text).unwrap();
		assert_eq!(games.len(), 2);
		assert_eq!(games[0].name, "Beat Saber");
		assert_eq!(games[0].release_name, "Beat Saber v1.40.0+1.40");
		assert_eq!(games[0].package, "com.beatgames.beatsaber");
		assert_eq!(games[0].version_code, Some(1402));
		assert_eq!(games[0].size_mb, Some(1350.0));
		assert_eq!(games[0].downloads, Some(98.5));
		assert_eq!(games[1].size_mb, None);
		assert_eq!(games[1].downloads, Some(12.0));
	}

	#[test]
	fn column_order_comes_from_header() {
		let text = "Package Name;Game Name;Release Name\ncom.a.b;A;A v1\n";
		let games = parse_game_list(text).unwrap();
		assert_eq!(games[0].package, "com.a.b");
		assert_eq!(games[0].version_code, None);
		assert_eq!(games[0].last_updated, "");
	}

	#[test]
	fn rejects_unknown_format() {
		assert!(parse_game_list("").is_err());
		assert!(parse_game_list("<html>Not Found</html>").is_err());
	}
}
