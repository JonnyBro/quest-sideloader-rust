//! Whether a newer release of the app is out on GitHub.

use serde::Deserialize;

const LATEST_RELEASE_API: &str =
	"https://api.github.com/repos/JonnyBro/quest-sideloader-rust/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRelease {
	/// Without the leading `v`, like `2.1.0`.
	pub version: String,
	pub url: String,
}

#[derive(Deserialize)]
struct LatestRelease {
	tag_name: String,
	html_url: String,
}

/// `None` also when the repository has no releases yet.
pub async fn check() -> Result<Option<AppRelease>, String> {
	let client = crate::platform_tools::http_client()?;
	let response = client
		.get(LATEST_RELEASE_API)
		.header("Accept", "application/vnd.github+json")
		.send()
		.await
		.map_err(|e| e.to_string())?;
	if response.status() == reqwest::StatusCode::NOT_FOUND {
		return Ok(None);
	}
	let text = response
		.error_for_status()
		.map_err(|e| e.to_string())?
		.text()
		.await
		.map_err(|e| e.to_string())?;
	let latest: LatestRelease = serde_json::from_str(&text).map_err(|e| e.to_string())?;
	Ok(
		is_newer(&latest.tag_name, env!("CARGO_PKG_VERSION")).then(|| AppRelease {
			version: latest.tag_name.trim_start_matches('v').to_owned(),
			url: latest.html_url,
		}),
	)
}

/// `v2.1.0`, `2.1` or `2.1.0-beta` as numbers; anything after `-` is ignored.
fn parse_version(text: &str) -> Option<[u64; 3]> {
	let core = text
		.trim()
		.trim_start_matches('v')
		.split(['-', '+'])
		.next()?;
	let mut parts = [0u64; 3];
	for (slot, part) in parts.iter_mut().zip(core.split('.')) {
		*slot = part.parse().ok()?;
	}
	Some(parts)
}

fn is_newer(tag: &str, current: &str) -> bool {
	match (parse_version(tag), parse_version(current)) {
		(Some(tag), Some(current)) => tag > current,
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn compares_versions() {
		assert!(is_newer("v2.1.0", "2.0.0"));
		assert!(is_newer("v2.0.1", "2.0.0"));
		assert!(is_newer("3", "2.9.9"));
		assert!(!is_newer("v2.0.0", "2.0.0"));
		assert!(!is_newer("v1.9.0", "2.0.0"));
		assert!(!is_newer("v2.0.0-beta", "2.0.0"));
		assert!(!is_newer("nightly", "2.0.0"));
		assert_eq!(parse_version("v2.1"), Some([2, 1, 0]));
	}
}
