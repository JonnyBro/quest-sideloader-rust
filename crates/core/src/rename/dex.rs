//! The string table of a `classes.dex`, read only. Dex strings are kept
//! sorted, so changing one means renumbering every reference in the code;
//! that's why the analysis only reports them.

use super::chunk::{FormatError, Reader};

/// Calls `visit` with every string in the file.
pub fn for_each_string(data: &[u8], mut visit: impl FnMut(&str)) -> Result<(), FormatError> {
	let r = Reader::new(data);
	if r.slice(0, 4)? != b"dex\n" {
		return Err(FormatError::new("not a dex file"));
	}
	let count = r.u32(0x38)? as usize;
	let ids = r.u32(0x3c)? as usize;
	for i in 0..count {
		let mut at = r.u32(ids + i * 4)? as usize;
		// The UTF-16 length, as ULEB128; the bytes end with a zero instead.
		while r.u8(at)? & 0x80 != 0 {
			at += 1;
		}
		at += 1;
		let rest = data
			.get(at..)
			.ok_or_else(|| FormatError::new("string past the end"))?;
		let end = rest
			.iter()
			.position(|&b| b == 0)
			.ok_or_else(|| FormatError::new("unterminated string"))?;
		// Modified UTF-8 only differs from UTF-8 for NUL and characters outside
		// the BMP, neither of which is in a package name.
		visit(&String::from_utf8_lossy(&rest[..end]));
	}
	Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	pub fn dex(strings: &[&str]) -> Vec<u8> {
		let ids_off = 0x70;
		let mut data = vec![0u8; ids_off];
		data[..8].copy_from_slice(b"dex\n035\0");
		data[0x38..0x3c].copy_from_slice(&(strings.len() as u32).to_le_bytes());
		data[0x3c..0x40].copy_from_slice(&(ids_off as u32).to_le_bytes());
		let mut payload = Vec::new();
		let start = ids_off + strings.len() * 4;
		for s in strings {
			data.extend_from_slice(&((start + payload.len()) as u32).to_le_bytes());
			payload.push(s.len() as u8);
			payload.extend_from_slice(s.as_bytes());
			payload.push(0);
		}
		data.extend_from_slice(&payload);
		data
	}

	#[test]
	fn reads_strings() {
		let mut seen = Vec::new();
		for_each_string(&dex(&["Lcom/a/Main;", "com.a.files"]), |s| {
			seen.push(s.to_owned())
		})
		.unwrap();
		assert_eq!(seen, ["Lcom/a/Main;", "com.a.files"]);
	}
}
