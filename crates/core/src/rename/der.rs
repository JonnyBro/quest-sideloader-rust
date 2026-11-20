//! Just enough DER to write a self-signed certificate and a PKCS#7 signature,
//! and to find fields in them again.

use super::chunk::FormatError;

pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTF8_STRING: u8 = 0x0c;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;

pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
	let mut out = vec![tag];
	let len = content.len();
	if len < 0x80 {
		out.push(len as u8);
	} else {
		let bytes = len.to_be_bytes();
		let skip = bytes.iter().take_while(|&&b| b == 0).count();
		out.push(0x80 | (bytes.len() - skip) as u8);
		out.extend_from_slice(&bytes[skip..]);
	}
	out.extend_from_slice(content);
	out
}

pub fn seq(parts: &[&[u8]]) -> Vec<u8> {
	tlv(SEQUENCE, &parts.concat())
}

pub fn set(parts: &[&[u8]]) -> Vec<u8> {
	tlv(SET, &parts.concat())
}

/// `[n]` around a complete element.
pub fn explicit(n: u8, inner: &[u8]) -> Vec<u8> {
	tlv(0xa0 | n, inner)
}

/// A non-negative integer from big-endian bytes.
pub fn uint(bytes: &[u8]) -> Vec<u8> {
	let trimmed = &bytes[bytes
		.iter()
		.take_while(|&&b| b == 0)
		.count()
		.min(bytes.len() - 1)..];
	let mut content = Vec::with_capacity(trimmed.len() + 1);
	if trimmed[0] & 0x80 != 0 {
		content.push(0);
	}
	content.extend_from_slice(trimmed);
	tlv(INTEGER, &content)
}

pub fn oid(arcs: &[u64]) -> Vec<u8> {
	let mut content = vec![(arcs[0] * 40 + arcs[1]) as u8];
	for &arc in &arcs[2..] {
		let mut groups = vec![(arc & 0x7f) as u8];
		let mut rest = arc >> 7;
		while rest > 0 {
			groups.push((rest & 0x7f) as u8 | 0x80);
			rest >>= 7;
		}
		content.extend(groups.iter().rev());
	}
	tlv(OID, &content)
}

pub fn null() -> Vec<u8> {
	vec![NULL, 0]
}

/// `SEQUENCE { oid, NULL }`
pub fn algorithm(arcs: &[u64]) -> Vec<u8> {
	seq(&[&oid(arcs), &null()])
}

pub const SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
pub const RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 1];
pub const SHA256_WITH_RSA: &[u64] = &[1, 2, 840, 113549, 1, 1, 11];
pub const COMMON_NAME: &[u64] = &[2, 5, 4, 3];
pub const PKCS7_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 1];
pub const PKCS7_SIGNED_DATA: &[u64] = &[1, 2, 840, 113549, 1, 7, 2];

#[derive(Debug, Clone, Copy)]
pub struct Element<'a> {
	pub content: &'a [u8],
	pub raw: &'a [u8],
}

pub fn read(data: &[u8]) -> Result<Element<'_>, FormatError> {
	let bad = || FormatError::new("bad DER");
	data.first().ok_or_else(bad)?;
	let first = *data.get(1).ok_or_else(bad)? as usize;
	let (len, header) = if first < 0x80 {
		(first, 2)
	} else {
		let n = first & 0x7f;
		if n == 0 || n > 4 {
			return Err(bad());
		}
		let bytes = data.get(2..2 + n).ok_or_else(bad)?;
		(
			bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize),
			2 + n,
		)
	};
	let raw = data.get(..header + len).ok_or_else(bad)?;
	Ok(Element {
		content: &raw[header..],
		raw,
	})
}

/// The elements directly inside a constructed one.
pub fn children(content: &[u8]) -> Result<Vec<Element<'_>>, FormatError> {
	let mut out = Vec::new();
	let mut rest = content;
	while !rest.is_empty() {
		let element = read(rest)?;
		rest = &rest[element.raw.len()..];
		out.push(element);
	}
	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn encodes_known_values() {
		assert_eq!(
			oid(SHA256_WITH_RSA),
			[6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 11]
		);
		assert_eq!(uint(&[0, 0, 0x80]), [2, 2, 0, 0x80]);
		assert_eq!(uint(&[0]), [2, 1, 0]);
		let long = tlv(OCTET_STRING, &[7; 300]);
		assert_eq!(&long[..4], [4, 0x82, 1, 44]);
		let element = read(&long).unwrap();
		assert_eq!(element.content.len(), 300);
		let both = seq(&[&null(), &uint(&[5])]);
		let parts = children(read(&both).unwrap().content).unwrap();
		assert_eq!(parts.len(), 2);
		assert_eq!(parts[1].content, [5]);
	}
}
