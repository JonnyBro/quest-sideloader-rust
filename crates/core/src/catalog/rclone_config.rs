//! Safety checks for rclone configs that come from a mirror.

use rust_i18n::t;

use super::CatalogError;

/// Backends that wrap other remotes: fine when they wrap another remote of the
/// same config, not when they point at a folder on this computer.
const WRAPPER_BACKENDS: [&str; 8] = [
	"alias", "union", "combine", "crypt", "chunker", "compress", "hasher", "cache",
];

/// Options that make rclone run an external program.
const COMMAND_OPTIONS: [&str; 3] = ["ssh", "password_command", "command"];

/// Remote sections of an INI rclone config: name -> lowercased key -> value.
fn sections(text: &str) -> Vec<(String, Vec<(String, String)>)> {
	let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
	for line in text.lines() {
		let line = line.trim_start_matches('\u{feff}').trim();
		if line.starts_with(['#', ';']) || line.is_empty() {
			continue;
		}
		if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
			out.push((name.trim().to_owned(), Vec::new()));
			continue;
		}
		if let (Some((key, value)), Some((_, options))) = (line.split_once('='), out.last_mut()) {
			options.push((key.trim().to_ascii_lowercase(), value.trim().to_owned()));
		}
	}
	out
}

/// The remote a wrapper's `remote`/`upstreams` entry refers to, or `None`
/// if it's a path on this computer or an on-the-fly backend (`:local:`).
fn referenced_remote(target: &str) -> Option<&str> {
	let (name, rest) = target.split_once(':')?;
	let windows_drive = name.len() == 1 && rest.starts_with(['\\', '/']);
	(!name.is_empty() && !windows_drive).then_some(name)
}

/// Rejects mirror-supplied rclone configs that could touch local files or run
/// programs on this computer.
pub fn check_mirror_rclone_config(text: &str) -> Result<(), CatalogError> {
	let sections = sections(text);
	let names: Vec<&str> = sections.iter().map(|(n, _)| n.as_str()).collect();
	for (name, options) in &sections {
		let get = |key: &str| {
			options
				.iter()
				.find(|(k, _)| k == key)
				.map(|(_, v)| v.as_str())
		};
		let kind = get("type").unwrap_or_default().to_ascii_lowercase();
		if kind == "local" {
			return Err(CatalogError::Config(
				t!("rcfg.local_remote", name = name).into_owned(),
			));
		}
		if let Some((key, _)) = options
			.iter()
			.find(|(k, _)| COMMAND_OPTIONS.contains(&k.as_str()))
		{
			return Err(CatalogError::Config(
				t!("rcfg.runs_program", key = key).into_owned(),
			));
		}
		if !WRAPPER_BACKENDS.contains(&kind.as_str()) {
			continue;
		}
		let targets = get("remote")
			.into_iter()
			.chain(get("upstreams").into_iter().flat_map(str::split_whitespace));
		for target in targets {
			match referenced_remote(target) {
				Some(other) if names.contains(&other) && other != name => {}
				_ => {
					return Err(CatalogError::Config(
						t!(
							"rcfg.bad_wrapper",
							name = name,
							kind = kind,
							target = target
						)
						.into_owned(),
					));
				}
			}
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mirror_rclone_configs_are_checked() {
		assert!(check_mirror_rclone_config("[a]\ntype = http\nurl = https://x/\n").is_ok());
		assert!(
			check_mirror_rclone_config("\u{feff}[a]\r\ntype = ftp\r\nhost = x\r\npass = y\r\n")
				.is_ok()
		);
		for bad in [
			"[a]\ntype = local\n",
			"[a]\ntype = alias\nremote = /home\n",
			"[a]\nTYPE = Union\nupstreams = /\n",
			"[a]\ntype = sftp\nssh = sh -c 'touch /tmp/x'\n",
			"[a]\ntype = crypt\nremote = C:\\Users\\me\n",
			"[a]\ntype = crypt\nremote = :local:/etc\n",
			"[a]\ntype = crypt\nremote = missing:x\n",
			"[a]\ntype = crypt\nremote = a:loop\n",
			"[s]\ntype = http\nurl = x\n[a]\ntype = union\nupstreams = s:ok /home\n",
		] {
			assert!(check_mirror_rclone_config(bad).is_err(), "{bad}");
		}
		// crypt and union over storage remotes of the same config are fine.
		let mirror = "[store]\ntype = ftp\nhost = x\n\
			[games]\ntype = crypt\nremote = store:Quest\npassword = abc\n\
			[all]\ntype = union\nupstreams = store:a store:b:ro\n";
		assert!(check_mirror_rclone_config(mirror).is_ok());
	}
}
