//! `resources.arsc`: the compiled resource table.

use super::chunk::{FormatError, Reader};
use super::strpool::{self, StringPool};

const TABLE_TYPE: u16 = 0x0002;
const PACKAGE_TYPE: u16 = 0x0200;
const TYPE_TYPE: u16 = 0x0201;

/// Two letters, or three packed into the two bytes (high bit set).
fn unpack_locale(a: u8, b: u8) -> Option<String> {
	if a == 0 {
		return None;
	}
	if a & 0x80 == 0 {
		return Some(String::from_utf8_lossy(&[a, b]).into_owned());
	}
	let letter = |n: u8| char::from(b'a' + n);
	Some(
		[
			b & 0x1f,
			((b & 0xe0) >> 5) | ((a & 0x03) << 3),
			(a & 0x7c) >> 2,
		]
		.map(letter)
		.iter()
		.collect(),
	)
}

#[derive(Debug, Clone)]
pub struct ResourceTable {
	pub strings: StringPool,
	pub packages: Vec<Package>,
	header: Vec<u8>,
	/// The chunks around the string pool, written back as read.
	before: Vec<u8>,
	after: Vec<u8>,
}

/// Its name is Gradle's namespace, which needn't match the application id;
/// Android doesn't compare the two, so renaming leaves it alone.
#[derive(Debug, Clone)]
pub struct Package {
	/// Resource entry names, such as `app_name` or `google_app_id`.
	pub keys: StringPool,
	/// Languages the resources have versions for, like `ru` or `pt-BR`.
	pub locales: Vec<String>,
}

impl ResourceTable {
	pub fn parse(data: &[u8]) -> Result<Self, FormatError> {
		let r = Reader::new(data);
		if r.u16(0)? != TABLE_TYPE {
			return Err(FormatError::new("not a resource table"));
		}
		let header_size = r.u16(2)? as usize;
		let size = (r.u32(4)? as usize).min(data.len());
		let mut strings = None;
		let mut packages = Vec::new();
		let (mut before, mut after) = (Vec::new(), Vec::new());
		let mut at = header_size;
		while at + 8 <= size {
			let kind = r.u16(at)?;
			let chunk_size = r.u32(at + 4)? as usize;
			if chunk_size < 8 {
				return Err(FormatError::new("bad chunk size"));
			}
			let chunk = r.slice(at, chunk_size)?;
			match kind {
				strpool::TYPE if strings.is_none() => {
					strings = Some(StringPool::parse(chunk)?);
					at += chunk_size;
					continue;
				}
				PACKAGE_TYPE => packages.push(Package::parse(chunk)?),
				_ => {}
			}
			if strings.is_none() {
				before.extend_from_slice(chunk);
			} else {
				after.extend_from_slice(chunk);
			}
			at += chunk_size;
		}
		Ok(Self {
			strings: strings.ok_or_else(|| FormatError::new("no string pool"))?,
			packages,
			header: r.slice(0, header_size)?.to_vec(),
			before,
			after,
		})
	}

	pub fn write(&self) -> Vec<u8> {
		let pool = self.strings.write();
		let size = self.header.len() + self.before.len() + pool.len() + self.after.len();
		let mut out = Vec::with_capacity(size);
		out.extend_from_slice(&self.header);
		out[4..8].copy_from_slice(&(size as u32).to_le_bytes());
		out.extend_from_slice(&self.before);
		out.extend_from_slice(&pool);
		out.extend_from_slice(&self.after);
		out
	}

	pub fn has_key(&self, key: &str) -> bool {
		self.packages
			.iter()
			.any(|p| p.keys.iter().any(|k| k == key))
	}
}

impl Package {
	fn parse(chunk: &[u8]) -> Result<Self, FormatError> {
		let r = Reader::new(chunk);
		// After the header, id, name, typeStrings and lastPublicType.
		let key_strings = r.u32(12 + 256 + 8)? as usize;
		let mut locales = Vec::new();
		let mut at = r.u16(2)? as usize;
		while at + 8 <= chunk.len() {
			let size = r.u32(at + 4)? as usize;
			if size < 8 {
				break;
			}
			// ResTable_type: the config's language and country follow its
			// size and the mcc/mnc pair.
			if r.u16(at)? == TYPE_TYPE {
				let config = at + 20;
				let language = unpack_locale(r.u8(config + 8)?, r.u8(config + 9)?);
				let country = unpack_locale(r.u8(config + 10)?, r.u8(config + 11)?);
				if let Some(language) = language {
					let locale = match country {
						Some(country) => format!("{language}-{}", country.to_ascii_uppercase()),
						None => language,
					};
					if !locales.contains(&locale) {
						locales.push(locale);
					}
				}
			}
			at += size;
		}
		locales.sort();
		Ok(Self {
			keys: StringPool::parse(r.slice(key_strings, chunk.len() - key_strings)?)?,
			locales,
		})
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// A table with `strings` as values and one package holding `keys`.
	pub fn table(package: &str, strings: &[&str], keys: &[&str]) -> Vec<u8> {
		let values = StringPool::new(true, strings).write();
		let types = StringPool::new(false, &["string"]).write();
		let key_pool = StringPool::new(true, keys).write();
		let header = 288;
		let mut pkg = Vec::new();
		pkg.extend_from_slice(&PACKAGE_TYPE.to_le_bytes());
		pkg.extend_from_slice(&(header as u16).to_le_bytes());
		let size = header + types.len() + key_pool.len();
		pkg.extend_from_slice(&(size as u32).to_le_bytes());
		pkg.extend_from_slice(&0x7fu32.to_le_bytes());
		let mut name = [0u16; 128];
		for (slot, unit) in name.iter_mut().zip(package.encode_utf16()) {
			*slot = unit;
		}
		for unit in name {
			pkg.extend_from_slice(&unit.to_le_bytes());
		}
		pkg.extend_from_slice(&(header as u32).to_le_bytes());
		pkg.extend_from_slice(&0u32.to_le_bytes());
		pkg.extend_from_slice(&((header + types.len()) as u32).to_le_bytes());
		pkg.extend_from_slice(&0u32.to_le_bytes());
		pkg.extend_from_slice(&0u32.to_le_bytes());
		pkg.extend_from_slice(&types);
		pkg.extend_from_slice(&key_pool);

		let mut out = Vec::new();
		out.extend_from_slice(&TABLE_TYPE.to_le_bytes());
		out.extend_from_slice(&12u16.to_le_bytes());
		out.extend_from_slice(&((12 + values.len() + pkg.len()) as u32).to_le_bytes());
		out.extend_from_slice(&1u32.to_le_bytes());
		out.extend_from_slice(&values);
		out.extend_from_slice(&pkg);
		out
	}

	#[test]
	fn reads_strings_and_keys() {
		let data = table(
			"com.a",
			&["com.a.files", "Hello"],
			&["app_name", "google_app_id"],
		);
		let table = ResourceTable::parse(&data).unwrap();
		assert_eq!(
			table.strings.iter().collect::<Vec<_>>(),
			["com.a.files", "Hello"]
		);
		assert!(table.has_key("google_app_id"));
		assert!(!table.has_key("missing"));
		assert_eq!(
			table.write(),
			data,
			"unchanged tables are written back as read"
		);

		let mut table = table;
		table.strings.set(0, "com.b.files");
		let again = ResourceTable::parse(&table.write()).unwrap();
		assert_eq!(again.strings.get(0), Some("com.b.files"));
		assert!(again.has_key("google_app_id"));
	}
}
