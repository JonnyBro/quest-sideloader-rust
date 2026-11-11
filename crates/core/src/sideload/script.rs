//! `install.txt` support. Releases ship these scripts with `adb` commands; they
//! come from third-party mirrors, so every line is classified before anything
//! runs: safe lines run, suspicious ones only after the user approves them,
//! blocked ones never.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
	Install { apk: PathBuf },
	Push { local: PathBuf, remote: String },
	Uninstall { package: String },
	Shell { command: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
	Safe,
	Suspicious(String),
	Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
	/// Blank lines, comments and non-adb commands (e.g. `echo`) are not run.
	Ignored(Option<String>),
	Command {
		action: Option<Action>,
		verdict: Verdict,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLine {
	pub number: usize,
	pub text: String,
	pub kind: LineKind,
}

impl ScriptLine {
	pub fn verdict(&self) -> Option<&Verdict> {
		match &self.kind {
			LineKind::Command { verdict, .. } => Some(verdict),
			LineKind::Ignored(_) => None,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptAnalysis {
	pub lines: Vec<ScriptLine>,
	pub package: Option<String>,
}

impl ScriptAnalysis {
	/// The user has to look at the script before it runs.
	pub fn needs_review(&self) -> bool {
		self.lines.iter().any(|l| {
			matches!(
				l.verdict(),
				Some(Verdict::Suspicious(_) | Verdict::Blocked(_))
			)
		})
	}

	/// Actions to run, in order. Suspicious lines are included only when allowed.
	pub fn actions(&self, allow_suspicious: bool) -> Vec<(usize, Action)> {
		self.lines
			.iter()
			.filter_map(|line| match &line.kind {
				LineKind::Command {
					action: Some(action),
					verdict,
				} => {
					let runs = match verdict {
						Verdict::Safe => true,
						Verdict::Suspicious(_) => allow_suspicious,
						Verdict::Blocked(_) => false,
					};
					runs.then(|| (line.number, action.clone()))
				}
				_ => None,
			})
			.collect()
	}
}

/// `root` is the release folder; `package` is the game's package name, if known.
pub fn analyze(text: &str, root: &Path, package: Option<&str>) -> ScriptAnalysis {
	let ctx = Context { root, package };
	let lines = text
		.lines()
		.enumerate()
		.map(|(i, raw)| ScriptLine {
			number: i + 1,
			text: raw.trim_end().to_owned(),
			kind: ctx.classify(raw),
		})
		.collect();
	ScriptAnalysis {
		lines,
		package: package.map(str::to_owned),
	}
}

struct Context<'a> {
	root: &'a Path,
	package: Option<&'a str>,
}

fn command(action: Action, verdict: Verdict) -> LineKind {
	LineKind::Command {
		action: Some(action),
		verdict,
	}
}

fn blocked(reason: impl Into<String>) -> LineKind {
	LineKind::Command {
		action: None,
		verdict: Verdict::Blocked(reason.into()),
	}
}

impl Context<'_> {
	fn classify(&self, raw: &str) -> LineKind {
		let line = raw.trim();
		if line.is_empty()
			|| line.starts_with('#')
			|| line.starts_with("::")
			|| starts_with_word(line, "rem")
		{
			return LineKind::Ignored(None);
		}
		// Scripts are usually written on Windows: backslashes are path separators
		// there, while shell-words would treat them as escapes.
		let normalized = line.replace('\\', "/");
		let words = match shell_words::split(&normalized) {
			Ok(words) => words,
			Err(_) => return blocked("cannot parse the line (unbalanced quotes)"),
		};
		let Some((program, args)) = words.split_first() else {
			return LineKind::Ignored(None);
		};
		let program = program.to_ascii_lowercase();
		if program != "adb" && program != "adb.exe" {
			return LineKind::Ignored(Some(format!("not an adb command, skipped: {program}")));
		}
		let args = strip_device_selectors(args);
		let Some((subcommand, rest)) = args.split_first() else {
			return blocked("adb without a command");
		};
		match subcommand.as_str() {
			"install" => self.install(rest),
			"push" => self.push(rest),
			"uninstall" => self.uninstall(rest),
			"shell" => self.shell(rest),
			other => blocked(format!("`adb {other}` is not supported in install scripts")),
		}
	}

	fn install(&self, args: &[String]) -> LineKind {
		const KNOWN_FLAGS: [&str; 4] = ["-r", "-g", "-d", "-t"];
		let (flags, files): (Vec<_>, Vec<_>) = args.iter().partition(|a| a.starts_with('-'));
		let [apk] = files.as_slice() else {
			return blocked("install expects exactly one APK");
		};
		let apk = match self.local_path(apk) {
			Ok(path) => path,
			Err(reason) => return blocked(reason),
		};
		let action = Action::Install { apk };
		match flags.iter().find(|f| !KNOWN_FLAGS.contains(&f.as_str())) {
			Some(flag) => command(
				action,
				Verdict::Suspicious(format!("unknown install flag {flag}")),
			),
			None => command(action, Verdict::Safe),
		}
	}

	fn push(&self, args: &[String]) -> LineKind {
		let [local, remote] = args else {
			return blocked("push expects a local path and a device path");
		};
		let local_path = match self.local_path(local) {
			Ok(path) => path,
			Err(reason) => return blocked(reason),
		};
		let Some(remote_norm) = normalize_remote(remote) else {
			return blocked(format!("device path {remote} contains `..`"));
		};
		let local_name = Path::new(local)
			.file_name()
			.map(|n| n.to_string_lossy().into_owned());
		let action = Action::Push {
			local: local_path,
			remote: remote_norm.clone(),
		};

		let Some(package) = self.package else {
			return command(
				action,
				Verdict::Suspicious(
					"the game's package name is unknown, so the target can't be checked".into(),
				),
			);
		};
		// `adb push com.game /sdcard/Android/obb/` creates obb/com.game itself.
		let into_parent = ["/sdcard/Android/obb", "/sdcard/Android/data"]
			.contains(&remote_norm.as_str())
			&& local_name.as_deref() == Some(package);
		if into_parent || is_game_path(&remote_norm, package) {
			command(action, Verdict::Safe)
		} else {
			command(
				action,
				Verdict::Suspicious(format!(
					"writes to {remote_norm}, outside this game's folders"
				)),
			)
		}
	}

	fn uninstall(&self, args: &[String]) -> LineKind {
		let packages: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();
		let [package] = packages.as_slice() else {
			return blocked("uninstall expects exactly one package");
		};
		self.uninstall_verdict(package)
	}

	fn uninstall_verdict(&self, package: &str) -> LineKind {
		let action = Action::Uninstall {
			package: package.to_owned(),
		};
		if Some(package) == self.package {
			command(
				action,
				Verdict::Suspicious("removes the installed game together with its saves".into()),
			)
		} else {
			command(
				action,
				Verdict::Suspicious(format!("removes another app: {package}")),
			)
		}
	}

	fn shell(&self, args: &[String]) -> LineKind {
		if args.is_empty() {
			return blocked("interactive shell is not supported");
		}
		let command_text = if args.len() == 1 {
			args[0].clone()
		} else {
			shell_words::join(args)
		};
		let action = Action::Shell {
			command: command_text.clone(),
		};
		if command_text.contains(['\n', '\r']) {
			return blocked("multi-line shell command");
		}
		if contains_shell_operators(&command_text) {
			return command(
				action,
				Verdict::Suspicious("chains several commands or redirects output".into()),
			);
		}
		let Ok(words) = shell_words::split(&command_text) else {
			return blocked("cannot parse the shell command");
		};
		let verdict = self.shell_verdict(&words);
		if let Verdict::Suspicious(_) = &verdict
			&& let [pm, sub, pkg] = words.as_slice()
			&& pm == "pm"
			&& sub == "uninstall"
		{
			return self.uninstall_verdict(pkg);
		}
		command(action, verdict)
	}

	fn shell_verdict(&self, words: &[String]) -> Verdict {
		let Some((program, args)) = words.split_first() else {
			return Verdict::Suspicious("empty shell command".into());
		};
		let unknown = || Verdict::Suspicious(format!("unrecognized shell command `{program}`"));
		let package = self.package;
		match program.as_str() {
			"pm" => match args {
				[sub, pkg, _permission] if sub == "grant" => {
					if Some(pkg.as_str()) == package {
						Verdict::Safe
					} else {
						Verdict::Suspicious(format!("changes permissions of another app: {pkg}"))
					}
				}
				_ => Verdict::Suspicious("package manager command".into()),
			},
			"mkdir" | "rm" | "mv" | "cp" | "touch" => {
				let paths: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();
				if paths.is_empty() {
					return unknown();
				}
				let Some(package) = package else {
					return Verdict::Suspicious(
						"the game's package name is unknown, so paths can't be checked".into(),
					);
				};
				match paths
					.iter()
					.find(|p| !normalize_remote(p).is_some_and(|p| is_game_path(&p, package)))
				{
					None => Verdict::Safe,
					Some(path) if program == "rm" => {
						Verdict::Suspicious(format!("deletes {path}, outside this game's folders"))
					}
					Some(path) => {
						Verdict::Suspicious(format!("touches {path}, outside this game's folders"))
					}
				}
			}
			_ => unknown(),
		}
	}

	/// Resolves a script path against the release folder; nothing outside it may be read.
	fn local_path(&self, path: &str) -> Result<PathBuf, String> {
		let relative = Path::new(path);
		let escapes = relative
			.components()
			.any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
		if escapes || path.contains(':') {
			return Err(format!("reads {path}, outside the release folder"));
		}
		Ok(self.root.join(relative))
	}
}

fn starts_with_word(line: &str, word: &str) -> bool {
	line.len() >= word.len()
		&& line[..word.len()].eq_ignore_ascii_case(word)
		&& line[word.len()..]
			.chars()
			.next()
			.is_none_or(char::is_whitespace)
}

/// Drops `-d`, `-e` and `-s <serial>`: commands always go to the selected headset.
fn strip_device_selectors(args: &[String]) -> Vec<String> {
	let mut out = Vec::with_capacity(args.len());
	let mut iter = args.iter();
	while let Some(arg) = iter.next() {
		match arg.as_str() {
			"-d" | "-e" if out.is_empty() => {}
			"-s" if out.is_empty() => {
				iter.next();
			}
			_ => out.push(arg.clone()),
		}
	}
	out
}

fn contains_shell_operators(command: &str) -> bool {
	command.contains([';', '&', '|', '`', '>', '<']) || command.contains("$(")
}

/// Canonical device path: `/storage/emulated/0` and `/mnt/sdcard` become
/// `/sdcard`, duplicate and trailing slashes go away. `None` if it has `..`.
fn normalize_remote(path: &str) -> Option<String> {
	let mut parts = Vec::new();
	for part in path.split('/') {
		match part {
			"" | "." => {}
			".." => return None,
			p => parts.push(p),
		}
	}
	let joined = format!("/{}", parts.join("/"));
	for prefix in [
		"/storage/emulated/0",
		"/storage/self/primary",
		"/mnt/sdcard",
	] {
		if let Some(rest) = joined.strip_prefix(prefix)
			&& (rest.is_empty() || rest.starts_with('/'))
		{
			return Some(format!("/sdcard{rest}"));
		}
	}
	Some(joined)
}

fn is_game_path(path: &str, package: &str) -> bool {
	["obb", "data"].iter().any(|kind| {
		let base = format!("/sdcard/Android/{kind}/{package}");
		path == base || path.starts_with(&format!("{base}/"))
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	const PKG: &str = "com.example.game";

	fn one(line: &str) -> LineKind {
		Context {
			root: Path::new("/rel"),
			package: Some(PKG),
		}
		.classify(line)
	}

	fn verdict(line: &str) -> Verdict {
		match one(line) {
			LineKind::Command { verdict, .. } => verdict,
			other => panic!("{line}: expected a command, got {other:?}"),
		}
	}

	fn is_safe(line: &str) -> bool {
		verdict(line) == Verdict::Safe
	}

	fn is_suspicious(line: &str) -> bool {
		matches!(verdict(line), Verdict::Suspicious(_))
	}

	fn is_blocked(line: &str) -> bool {
		matches!(verdict(line), Verdict::Blocked(_))
	}

	#[test]
	fn ignores_comments_and_other_programs() {
		for line in [
			"",
			"   ",
			"# comment",
			"REM windows comment",
			":: also a comment",
		] {
			assert_eq!(one(line), LineKind::Ignored(None), "{line:?}");
		}
		assert!(matches!(
			one("echo Installing..."),
			LineKind::Ignored(Some(_))
		));
		assert!(
			matches!(one("remount.bat"), LineKind::Ignored(Some(_))),
			"rem must be a whole word"
		);
	}

	#[test]
	fn typical_release_script_is_safe() {
		let text = "adb install -r -g game.apk\n\
			adb shell mkdir -p /sdcard/Android/obb/com.example.game\n\
			adb push com.example.game/main.1.com.example.game.obb /sdcard/Android/obb/com.example.game/\n\
			adb push com.example.game /sdcard/Android/obb/\n\
			adb shell pm grant com.example.game android.permission.RECORD_AUDIO\n";
		let analysis = analyze(text, Path::new("/rel"), Some(PKG));
		assert!(!analysis.needs_review(), "{analysis:#?}");
		let actions = analysis.actions(false);
		assert_eq!(actions.len(), 5);
		assert_eq!(
			actions[0].1,
			Action::Install {
				apk: "/rel/game.apk".into()
			}
		);
		assert_eq!(
			actions[2].1,
			Action::Push {
				local: "/rel/com.example.game/main.1.com.example.game.obb".into(),
				remote: "/sdcard/Android/obb/com.example.game".into(),
			}
		);
	}

	#[test]
	fn windows_paths_and_device_selectors() {
		assert!(is_safe(
			r"adb -d push com.example.game\main.obb /sdcard/Android/obb/com.example.game/main.obb"
		));
		assert!(is_safe("adb -s 1WMHH000 install game.apk"));
		assert!(is_safe(
			"adb push data /storage/emulated/0/Android/data/com.example.game/files"
		));
	}

	#[test]
	fn reading_outside_the_release_is_blocked() {
		assert!(is_blocked(
			"adb push ../../.ssh/id_rsa /sdcard/Android/obb/com.example.game/"
		));
		assert!(is_blocked(
			"adb push /home/user/secret /sdcard/Android/obb/com.example.game/"
		));
		assert!(is_blocked(
			r"adb push C:\Users\me\file /sdcard/Android/obb/com.example.game/"
		));
		assert!(is_blocked("adb install ../other.apk"));
	}

	#[test]
	fn unsupported_commands_are_blocked() {
		for line in [
			"adb reboot",
			"adb tcpip 5555",
			"adb pull /sdcard/DCIM",
			"adb root",
			"adb shell",
			"adb",
		] {
			assert!(is_blocked(line), "{line}");
		}
		assert!(is_blocked("adb shell \"echo hi"));
	}

	#[test]
	fn writes_outside_game_folders_are_suspicious() {
		assert!(is_suspicious("adb push x.txt /sdcard/Download/x.txt"));
		assert!(is_suspicious(
			"adb push other /sdcard/Android/obb/com.other.game"
		));
		assert!(is_suspicious(
			"adb push com.example.game /sdcard/Android/obb/com.example.gamex"
		));
		assert!(is_blocked(
			"adb push x /sdcard/Android/obb/com.example.game/../../DCIM"
		));
	}

	#[test]
	fn shell_commands() {
		assert!(is_safe(
			"adb shell rm -rf /sdcard/Android/obb/com.example.game"
		));
		assert!(is_suspicious("adb shell rm -rf /sdcard/DCIM"));
		assert!(is_suspicious(
			"adb shell \"rm -rf /sdcard/Android/obb/com.example.game; reboot\""
		));
		assert!(is_suspicious(
			"adb shell pm grant com.other.app android.permission.CAMERA"
		));
		assert!(is_suspicious("adb shell settings put global ota_disable 1"));
		assert!(is_suspicious(
			"adb shell setprop debug.oculus.refreshRate 120"
		));
		assert!(is_suspicious("adb shell echo $(cat /data/x)"));
		assert!(is_suspicious("adb shell pm clear com.example.game"));
	}

	#[test]
	fn uninstall_is_always_suspicious_with_a_reason() {
		let own = verdict("adb uninstall com.example.game");
		let other = verdict("adb shell pm uninstall com.other.app");
		assert!(matches!(own, Verdict::Suspicious(r) if r.contains("saves")));
		assert!(matches!(other, Verdict::Suspicious(r) if r.contains("com.other.app")));
	}

	#[test]
	fn unknown_package_makes_pushes_suspicious() {
		let analysis = analyze(
			"adb push x /sdcard/Android/obb/com.example.game/",
			Path::new("/rel"),
			None,
		);
		assert!(analysis.needs_review());
	}

	#[test]
	fn actions_respect_approval() {
		let text = "adb install game.apk\nadb shell settings put global x 1\nadb reboot\n";
		let analysis = analyze(text, Path::new("/rel"), Some(PKG));
		assert!(analysis.needs_review());
		assert_eq!(analysis.actions(false).len(), 1);
		let approved = analysis.actions(true);
		assert_eq!(approved.len(), 2, "blocked lines never run");
		assert_eq!(approved[1].0, 2);
	}

	#[test]
	fn remote_normalization() {
		assert_eq!(
			normalize_remote("/sdcard//Android/obb/"),
			Some("/sdcard/Android/obb".into())
		);
		assert_eq!(
			normalize_remote("/storage/emulated/0"),
			Some("/sdcard".into())
		);
		assert_eq!(
			normalize_remote("/storage/emulated/01/x"),
			Some("/storage/emulated/01/x".into())
		);
		assert_eq!(normalize_remote("/sdcard/../data"), None);
	}
}
