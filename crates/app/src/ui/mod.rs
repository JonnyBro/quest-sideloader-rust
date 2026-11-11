pub mod devices;
pub mod settings;

pub fn format_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

#[cfg(test)]
mod tests {
	use super::format_bytes;

	#[test]
	fn bytes() {
		assert_eq!(format_bytes(0), "0 B");
		assert_eq!(format_bytes(1023), "1023 B");
		assert_eq!(format_bytes(1536), "1.5 KiB");
		assert_eq!(format_bytes(119_089_524_736), "110.9 GiB");
	}
}
