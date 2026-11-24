use super::*;

/// `names` are the games' lowercase names; `rank` gives each game's
/// `status_rank` on the compared headset.
pub(super) fn sort_rows(
	rows: &mut [usize],
	games: &[Game],
	names: &[String],
	ranks: &[Option<u32>],
	sort: Sort,
	descending: bool,
	rank: impl Fn(usize) -> u8,
) {
	let by_name = |a: usize, b: usize| names[a].cmp(&names[b]);
	rows.sort_by(|&a, &b| {
		let (ga, gb) = (&games[a], &games[b]);
		let primary = match sort {
			Sort::Name => by_name(a, b),
			Sort::Package => ga.package.cmp(&gb.package),
			Sort::Version => ga.version_code.cmp(&gb.version_code),
			Sort::Updated => ga.last_updated.cmp(&gb.last_updated),
			Sort::Size => ga
				.size_mb
				.unwrap_or(0.0)
				.total_cmp(&gb.size_mb.unwrap_or(0.0)),
			// A better rank is a smaller number; unranked games are the lowest.
			Sort::Popularity => ranks[a].map(Reverse).cmp(&ranks[b].map(Reverse)),
			Sort::Status => rank(a).cmp(&rank(b)),
		};
		// Ties stay A to Z whichever way the main key goes.
		let primary = if descending {
			primary.reverse()
		} else {
			primary
		};
		primary
			.then_with(|| by_name(a, b))
			.then_with(|| ga.release_name.cmp(&gb.release_name))
	});
}

/// Letters and digits only, lowercased: "Beat Saber!" -> "beatsaber".
pub(super) fn squash(text: &str) -> String {
	text.chars()
		.filter(|c| c.is_alphanumeric())
		.flat_map(char::to_lowercase)
		.collect()
}

/// The squashed name and its initials, numbers kept whole:
/// "Resident Evil 4" -> ("residentevil4", "re4").
pub(super) fn search_key(name: &str) -> (String, String) {
	let mut initials = String::new();
	for word in name
		.split(|c: char| !c.is_alphanumeric())
		.filter(|w| !w.is_empty())
	{
		if word.chars().all(|c| c.is_ascii_digit()) {
			initials.push_str(word);
		} else if let Some(first) = word.chars().next() {
			initials.extend(first.to_lowercase());
		}
	}
	(squash(name), initials)
}

/// Finds games despite spacing, punctuation and abbreviations: "beatsaber",
/// "beat-saber" and "bs" all find Beat Saber.
pub(super) fn matches_loosely(key: &(String, String), search: &str) -> bool {
	let wanted = squash(search);
	!wanted.is_empty()
		&& (key.0.contains(&wanted) || (wanted.chars().count() >= 2 && key.1.starts_with(&wanted)))
}

/// Ranks packages by their best download score, like androidsideloader;
/// every release of a package gets the package's rank, and a score of 0 none.
pub(super) fn popularity_ranks(games: &[Game]) -> Vec<Option<u32>> {
	let mut best: HashMap<&str, f64> = HashMap::new();
	for game in games {
		if let Some(score) = game.downloads.filter(|s| *s > 0.0) {
			let entry = best.entry(game.package.as_str()).or_insert(score);
			*entry = entry.max(score);
		}
	}
	let mut order: Vec<(&str, f64)> = best.into_iter().collect();
	order.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
	let ranks: HashMap<&str, u32> = order
		.iter()
		.zip(1..)
		.map(|((package, _), rank)| (*package, rank))
		.collect();
	games
		.iter()
		.map(|g| ranks.get(g.package.as_str()).copied())
		.collect()
}

/// The mirror marks adult games with `(18+)` in their name; there's no tag for it.
pub(super) fn is_adult(game: &Game) -> bool {
	game.name.contains("(18+)")
}

pub(super) fn rank_text(rank: Option<u32>) -> String {
	rank.map(|r| format!("#{r}")).unwrap_or_default()
}

/// The game's size and the headset's free space, when the game won't fit.
/// The mirror lists the download size; the installed game takes about as much.
pub(super) fn short_of_space(game: &Game, apps: &Installed) -> Option<(u64, u64)> {
	let needed = (game.size_mb? * 1024.0 * 1024.0) as u64;
	let free = apps.free_bytes?;
	(needed > free).then_some((needed, free))
}

pub(super) fn size_text(game: &Game) -> String {
	game.size_mb
		.map(|mb| format_bytes((mb * 1024.0 * 1024.0) as u64))
		.unwrap_or_default()
}

/// The mirror writes `YYYY-MM-DD HH:MM UTC`; shown in the computer's time zone.
pub(super) fn mirror_time(value: &str) -> String {
	let trimmed = value.trim().trim_end_matches("UTC").trim();
	chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M")
		.map(|t| {
			t.and_utc()
				.with_timezone(&chrono::Local)
				.format("%Y-%m-%d %H:%M")
				.to_string()
		})
		.unwrap_or_else(|_| value.to_owned())
}

/// `2024-11-02 10:15 UTC` -> `2024-11-02`, in the computer's time zone.
pub(super) fn short_date(value: &str) -> String {
	let local = mirror_time(value);
	local.split_whitespace().next().unwrap_or(&local).to_owned()
}

pub(super) fn ago(time: SystemTime) -> String {
	let secs = SystemTime::now()
		.duration_since(time)
		.map_or(0, |d| d.as_secs());
	match secs {
		0..60 => t!("lib.just_now").into_owned(),
		60..3600 => t!("lib.mins_ago", n = secs / 60).into_owned(),
		3600..86_400 => t!("lib.hours_ago", n = secs / 3600).into_owned(),
		_ => t!("lib.days_ago", n = secs / 86_400).into_owned(),
	}
}

pub(super) fn install_label(status: Option<GameStatus>) -> String {
	match status {
		Some(GameStatus::UpdateAvailable) => t!("lib.download_update"),
		Some(GameStatus::Installed) => t!("lib.download_reinstall"),
		_ => t!("lib.download_install"),
	}
	.into_owned()
}

#[cfg(test)]
mod tests {
	use super::*;
	// The gpui glob import brings its own `test` attribute.
	use ::core::prelude::v1::test;

	fn game(package: &str, version: Option<u64>) -> Game {
		Game {
			name: "G".into(),
			release_name: "G v1".into(),
			package: package.into(),
			version_code: version,
			last_updated: String::new(),
			size_mb: None,
			downloads: None,
		}
	}

	#[test]
	fn adult_marker_in_name() {
		let mut g = game("com.a", None);
		g.name = "Cherry VX (18+)".into();
		assert!(is_adult(&g));
		g.name = "1976".into();
		g.release_name = "1976 v18+1.34 -VRP".into();
		assert!(!is_adult(&g), "a version code of 18 isn't the marker");
	}

	#[test]
	fn loose_search() {
		let key = search_key("Resident Evil 4");
		assert_eq!(key, ("residentevil4".into(), "re4".into()));
		assert!(matches_loosely(&key, "re4"));
		assert!(matches_loosely(&key, "resident-evil 4"));
		assert!(matches_loosely(&key, "Evil4"));
		assert!(matches_loosely(&key, "r"));
		assert!(!matches_loosely(&key, "rex"));
		assert!(!matches_loosely(&key, "re5"));
		assert!(matches_loosely(&search_key("Beat Saber"), "beatsaber"));
		assert!(matches_loosely(&search_key("Beat Saber"), "bs"));
	}

	#[test]
	fn status_against_device() {
		let mut apps = Installed {
			device_name: "Quest 3".into(),
			serial: "S".into(),
			versions: HashMap::from([("com.a".into(), Some(10)), ("com.b".into(), None)]),
			installed_at: HashMap::new(),
			free_bytes: None,
			held: HashSet::new(),
		};
		assert_eq!(
			apps.status(&game("com.a", Some(11))),
			GameStatus::UpdateAvailable
		);
		assert_eq!(apps.status(&game("com.a", Some(10))), GameStatus::Installed);
		assert_eq!(apps.status(&game("com.a", Some(9))), GameStatus::Installed);
		assert_eq!(apps.status(&game("com.b", Some(5))), GameStatus::Installed);
		assert_eq!(
			apps.status(&game("com.c", Some(1))),
			GameStatus::NotInstalled
		);
		apps.held.insert("com.a".into());
		assert_eq!(apps.status(&game("com.a", Some(11))), GameStatus::Installed);
	}

	#[test]
	fn sorts_by_each_key_with_name_ties() {
		let mut games = vec![
			game("com.c", Some(3)),
			game("com.a", None),
			game("com.b", Some(3)),
		];
		games[0].name = "Beta".into();
		games[1].name = "Alpha".into();
		games[2].name = "Alpha".into();
		games[2].release_name = "Alpha v2".into();
		games[0].size_mb = Some(500.0);
		games[2].size_mb = Some(10.0);
		let names: Vec<String> = games.iter().map(|g| g.name.to_lowercase()).collect();
		let ranks = [Some(2), None, Some(1)];
		let order = |sort, descending, rank: &dyn Fn(usize) -> u8| {
			let mut rows = vec![0, 1, 2];
			sort_rows(&mut rows, &games, &names, &ranks, sort, descending, rank);
			rows
		};
		let none = |_| 2;
		assert_eq!(order(Sort::Name, false, &none), [2, 1, 0]);
		assert_eq!(order(Sort::Name, true, &none), [0, 2, 1]);
		assert_eq!(order(Sort::Package, false, &none), [1, 2, 0]);
		assert_eq!(order(Sort::Version, true, &none), [2, 0, 1]);
		assert_eq!(order(Sort::Size, true, &none), [0, 2, 1]);
		assert_eq!(order(Sort::Popularity, true, &none), [2, 0, 1]);
		let rank = |i| [1, 2, 0][i];
		assert_eq!(order(Sort::Status, false, &rank), [2, 0, 1]);
	}

	#[test]
	fn ranks_packages_by_their_best_score() {
		let mut games = vec![
			game("com.a", None),
			game("com.b", None),
			game("com.a", None),
			game("com.c", None),
			game("com.d", None),
		];
		games[0].downloads = Some(5.0);
		games[1].downloads = Some(7.5);
		games[2].downloads = Some(9.0);
		games[3].downloads = Some(0.0);
		assert_eq!(
			popularity_ranks(&games),
			[Some(1), Some(2), Some(1), None, None]
		);
	}

	#[test]
	fn warns_only_when_the_game_does_not_fit() {
		let mut apps = Installed {
			device_name: "Quest 3".into(),
			serial: "S".into(),
			versions: HashMap::new(),
			installed_at: HashMap::new(),
			free_bytes: Some(100 * 1024 * 1024),
			held: HashSet::new(),
		};
		let mut big = game("com.a", Some(1));
		big.size_mb = Some(150.0);
		let mut small = game("com.b", Some(1));
		small.size_mb = Some(50.0);
		assert_eq!(
			short_of_space(&big, &apps),
			Some((150 * 1024 * 1024, 100 * 1024 * 1024))
		);
		assert_eq!(short_of_space(&small, &apps), None);
		assert_eq!(short_of_space(&game("com.c", None), &apps), None);
		apps.free_bytes = None;
		assert_eq!(short_of_space(&big, &apps), None);
	}

	#[test]
	fn dates() {
		// The day depends on the time zone the test runs in.
		let date = short_date("2024-11-02 10:15 UTC");
		assert!(["2024-11-01", "2024-11-02", "2024-11-03"].contains(&date.as_str()));
		assert_eq!(short_date(""), "");
		assert_eq!(short_date("someday"), "someday");
	}
}
