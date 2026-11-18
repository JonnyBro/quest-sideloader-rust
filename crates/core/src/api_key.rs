//! The mirror API key that `build.rs` baked in, sent to mirrors as `X-API-Key`.

const MASK: &[u8] = b"quest-sideloader/mirror-key";

pub const HEADER: &str = "X-API-Key";

/// The key of this build, or `None` for builds made without `API_KEY`.
pub fn embedded() -> Option<String> {
	unmask(env!("API_KEY_MASKED"))
}

fn unmask(hex: &str) -> Option<String> {
	if hex.is_empty() || !hex.len().is_multiple_of(2) {
		return None;
	}
	let bytes: Option<Vec<u8>> = (0..hex.len())
		.step_by(2)
		.zip(MASK.iter().cycle())
		.map(|(i, m)| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|b| b ^ m))
		.collect();
	String::from_utf8(bytes?).ok().filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unmasks_what_build_rs_masks() {
		let masked: String = "secret-key"
			.bytes()
			.zip(MASK.iter().cycle())
			.map(|(b, m)| format!("{:02x}", b ^ m))
			.collect();
		assert_eq!(unmask(&masked).as_deref(), Some("secret-key"));
		assert_eq!(unmask(""), None);
		assert_eq!(unmask("zz"), None);
	}
}
