//! `ResStringPool`, the string table shared by binary XML and `resources.arsc`.

use super::chunk::{FormatError, Reader, align4};

pub const TYPE: u16 = 0x0001;
const HEADER_SIZE: u16 = 28;
const UTF8_FLAG: u32 = 1 << 8;

/// One string, with the bytes it was stored as so unchanged strings are
/// written back exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
	text: String,
	raw: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringPool {
	flags: u32,
	entries: Vec<Entry>,
	style_offsets: Vec<u32>,
	/// Everything from the first style to the end of the chunk, kept as is.
	/// Styles only point at the first strings, so appending doesn't move them.
	styles: Vec<u8>,
}

impl StringPool {
	/// `data` is the whole chunk, header included.
	pub fn parse(data: &[u8]) -> Result<Self, FormatError> {
		let r = Reader::new(data);
		if r.u16(0)? != TYPE {
			return Err(FormatError::new("not a string pool"));
		}
		let header_size = r.u16(2)? as usize;
		let size = r.u32(4)? as usize;
		let data = r.slice(0, size)?;
		let r = Reader::new(data);
		let count = r.u32(8)? as usize;
		let style_count = r.u32(12)? as usize;
		let flags = r.u32(16)?;
		let strings_start = r.u32(20)? as usize;
		let styles_start = r.u32(24)? as usize;
		let utf8 = flags & UTF8_FLAG != 0;

		let offsets_end = header_size + count * 4;
		let strings_end = if styles_start != 0 {
			styles_start
		} else {
			size
		};
		let mut entries = Vec::with_capacity(count.min(1 << 20));
		for i in 0..count {
			let offset = strings_start + r.u32(header_size + i * 4)? as usize;
			let (text, len) = if utf8 {
				decode_utf8(&r, offset)?
			} else {
				decode_utf16(&r, offset)?
			};
			if offset + len > strings_end {
				return Err(FormatError::new("string runs past the pool"));
			}
			entries.push(Entry {
				text,
				raw: data[offset..offset + len].to_vec(),
			});
		}
		let style_offsets = (0..style_count)
			.map(|i| r.u32(offsets_end + i * 4))
			.collect::<Result<_, _>>()?;
		let styles = if styles_start != 0 {
			r.slice(styles_start, size - styles_start)?.to_vec()
		} else {
			Vec::new()
		};
		Ok(Self {
			flags,
			entries,
			style_offsets,
			styles,
		})
	}

	pub fn get(&self, index: u32) -> Option<&str> {
		self.entries.get(index as usize).map(|e| e.text.as_str())
	}

	pub fn iter(&self) -> impl Iterator<Item = &str> {
		self.entries.iter().map(|e| e.text.as_str())
	}

	/// Index of `text`, appending it if it isn't there yet.
	pub fn intern(&mut self, text: &str) -> u32 {
		if let Some(i) = self.entries.iter().position(|e| e.text == text) {
			return i as u32;
		}
		let raw = if self.flags & UTF8_FLAG != 0 {
			encode_utf8(text)
		} else {
			encode_utf16(text)
		};
		self.entries.push(Entry {
			text: text.to_owned(),
			raw,
		});
		(self.entries.len() - 1) as u32
	}

	/// Adds `text` as a new string even if it's there already, for a name
	/// that needs an index of its own.
	pub fn append(&mut self, text: &str) -> u32 {
		let raw = if self.flags & UTF8_FLAG != 0 {
			encode_utf8(text)
		} else {
			encode_utf16(text)
		};
		self.entries.push(Entry {
			text: text.to_owned(),
			raw,
		});
		(self.entries.len() - 1) as u32
	}

	/// Replaces the string in place, so every reference to it changes too.
	pub fn set(&mut self, index: u32, text: &str) {
		let utf8 = self.flags & UTF8_FLAG != 0;
		if let Some(entry) = self.entries.get_mut(index as usize) {
			entry.raw = if utf8 {
				encode_utf8(text)
			} else {
				encode_utf16(text)
			};
			entry.text = text.to_owned();
		}
	}

	pub fn write(&self) -> Vec<u8> {
		let header = HEADER_SIZE as usize;
		let offsets_len = (self.entries.len() + self.style_offsets.len()) * 4;
		let strings_start = header + offsets_len;
		let mut strings = Vec::new();
		let mut offsets = Vec::with_capacity(self.entries.len());
		for entry in &self.entries {
			offsets.push(strings.len() as u32);
			strings.extend_from_slice(&entry.raw);
		}
		strings.resize(align4(strings.len()), 0);
		let styles_start = if self.styles.is_empty() {
			0
		} else {
			strings_start + strings.len()
		};
		let size = strings_start + strings.len() + self.styles.len();
		// The sorted flag would no longer hold after appending.
		let flags = self.flags & UTF8_FLAG;

		let mut out = Vec::with_capacity(size);
		out.extend_from_slice(&TYPE.to_le_bytes());
		out.extend_from_slice(&HEADER_SIZE.to_le_bytes());
		out.extend_from_slice(&(size as u32).to_le_bytes());
		out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
		out.extend_from_slice(&(self.style_offsets.len() as u32).to_le_bytes());
		out.extend_from_slice(&flags.to_le_bytes());
		out.extend_from_slice(&(strings_start as u32).to_le_bytes());
		out.extend_from_slice(&(styles_start as u32).to_le_bytes());
		for offset in offsets.iter().chain(&self.style_offsets) {
			out.extend_from_slice(&offset.to_le_bytes());
		}
		out.extend_from_slice(&strings);
		out.extend_from_slice(&self.styles);
		out
	}

	#[cfg(test)]
	pub fn new(utf8: bool, strings: &[&str]) -> Self {
		let mut pool = Self {
			flags: if utf8 { UTF8_FLAG } else { 0 },
			entries: Vec::new(),
			style_offsets: Vec::new(),
			styles: Vec::new(),
		};
		for s in strings {
			pool.entries.push(Entry {
				text: (*s).to_owned(),
				raw: if utf8 {
					encode_utf8(s)
				} else {
					encode_utf16(s)
				},
			});
		}
		pool
	}
}

/// Lengths of up to 0x7fff fit in one unit; longer ones take two with the high bit set.
fn utf8_len(r: &Reader<'_>, at: usize) -> Result<(usize, usize), FormatError> {
	let first = r.u8(at)? as usize;
	if first & 0x80 == 0 {
		Ok((first, 1))
	} else {
		Ok((((first & 0x7f) << 8) | r.u8(at + 1)? as usize, 2))
	}
}

fn decode_utf8(r: &Reader<'_>, offset: usize) -> Result<(String, usize), FormatError> {
	let (_, chars_len) = utf8_len(r, offset)?;
	let (bytes, bytes_len) = utf8_len(r, offset + chars_len)?;
	let start = offset + chars_len + bytes_len;
	let text = String::from_utf8_lossy(r.slice(start, bytes)?).into_owned();
	Ok((text, start + bytes + 1 - offset))
}

fn decode_utf16(r: &Reader<'_>, offset: usize) -> Result<(String, usize), FormatError> {
	let first = r.u16(offset)? as usize;
	let (units, len_size) = if first & 0x8000 == 0 {
		(first, 2)
	} else {
		(((first & 0x7fff) << 16) | r.u16(offset + 2)? as usize, 4)
	};
	let start = offset + len_size;
	let raw = r.slice(start, units * 2)?;
	let text = String::from_utf16_lossy(
		&raw.as_chunks::<2>()
			.0
			.iter()
			.map(|c| u16::from_le_bytes(*c))
			.collect::<Vec<_>>(),
	);
	Ok((text, len_size + units * 2 + 2))
}

fn push_utf8_len(out: &mut Vec<u8>, len: usize) {
	if len > 0x7f {
		out.push(((len >> 8) as u8) | 0x80);
	}
	out.push(len as u8);
}

fn encode_utf8(text: &str) -> Vec<u8> {
	let mut out = Vec::with_capacity(text.len() + 5);
	push_utf8_len(&mut out, text.encode_utf16().count());
	push_utf8_len(&mut out, text.len());
	out.extend_from_slice(text.as_bytes());
	out.push(0);
	out
}

fn encode_utf16(text: &str) -> Vec<u8> {
	let units: Vec<u16> = text.encode_utf16().collect();
	let mut out = Vec::with_capacity(units.len() * 2 + 6);
	if units.len() > 0x7fff {
		out.extend_from_slice(&(((units.len() >> 16) as u16) | 0x8000).to_le_bytes());
	}
	out.extend_from_slice(&(units.len() as u16).to_le_bytes());
	for unit in units {
		out.extend_from_slice(&unit.to_le_bytes());
	}
	out.extend_from_slice(&[0, 0]);
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trips_both_encodings() {
		let long = "x".repeat(300);
		for utf8 in [true, false] {
			let pool = StringPool::new(utf8, &["package", "привет", "", &long]);
			let bytes = pool.write();
			let parsed = StringPool::parse(&bytes).unwrap();
			assert_eq!(
				parsed.iter().collect::<Vec<_>>(),
				["package", "привет", "", long.as_str()]
			);
			assert_eq!(parsed.write(), bytes);
		}
	}

	#[test]
	fn intern_reuses_and_appends() {
		let mut pool = StringPool::new(true, &["a", "b"]);
		assert_eq!(pool.intern("b"), 1);
		assert_eq!(pool.intern("c"), 2);
		pool.set(0, "z");
		let parsed = StringPool::parse(&pool.write()).unwrap();
		assert_eq!(parsed.iter().collect::<Vec<_>>(), ["z", "b", "c"]);
	}
}
