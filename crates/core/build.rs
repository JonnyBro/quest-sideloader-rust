//! Bakes the mirror API key into the build: from the `API_KEY` environment
//! variable, or else from an `api_key.txt` next to the workspace's
//! `Cargo.toml` (ignored by git). The key never goes into the repository:
//! official builds get it from a CI secret, and self-built copies have none.

use std::path::Path;

/// Must match `MASK` in `src/api_key.rs`.
const MASK: &[u8] = b"quest-sideloader/mirror-key";

fn main() {
	println!("cargo:rerun-if-env-changed=API_KEY");
	let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
	let workspace = Path::new(&manifest_dir)
		.ancestors()
		.nth(2)
		.expect("crates/core sits two levels below the workspace");
	let file = workspace.join("api_key.txt");
	// Cargo reruns on every build for a watched path that doesn't exist, and
	// scans a watched folder recursively (target included), so a missing key
	// file isn't watched: after creating one, touch this file once.
	if file.exists() {
		println!("cargo:rerun-if-changed={}", file.display());
	} else {
		println!("cargo:rerun-if-changed=build.rs");
	}

	let key = std::env::var("API_KEY")
		.ok()
		.filter(|k| !k.trim().is_empty())
		.or_else(|| std::fs::read_to_string(&file).ok())
		.unwrap_or_default();
	let masked: String = key
		.trim()
		.bytes()
		.zip(MASK.iter().cycle())
		.map(|(b, m)| format!("{:02x}", b ^ m))
		.collect();
	println!("cargo:rustc-env=API_KEY_MASKED={masked}");
}
