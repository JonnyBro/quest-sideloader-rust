//! Android's binary XML, as `AndroidManifest.xml` is stored inside an APK.
//! Only attribute values change; every other byte is written back as read.

use super::chunk::{FormatError, Reader};
use super::strpool::{self, StringPool};

const XML_TYPE: u16 = 0x0003;
const RESOURCE_MAP_TYPE: u16 = 0x0180;
const START_ELEMENT: u16 = 0x0102;
const END_ELEMENT: u16 = 0x0103;
const NO_STRING: u32 = u32::MAX;
const TYPE_STRING: u8 = 0x03;

pub const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

#[derive(Debug, Clone)]
pub struct Axml {
	pool: StringPool,
	resource_map: Vec<u32>,
	resource_map_raw: Vec<u8>,
	/// The chunks after the string pool and resource map.
	body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
	pub name: String,
	/// Names of the enclosing elements, outermost first.
	pub ancestors: Vec<String>,
	pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
	pub namespace: Option<String>,
	pub name: String,
	/// Android attribute id, when the name is in the resource map.
	pub resource_id: Option<u32>,
	/// The value when it is a plain string.
	pub value: Option<String>,
	/// Where the attribute is in `body`.
	offset: usize,
}

impl Attribute {
	pub fn is_android(&self, name: &str, id: u32) -> bool {
		self.resource_id == Some(id)
			|| (self.name == name && self.namespace.as_deref() == Some(ANDROID_NS))
	}
}

impl Axml {
	pub fn parse(data: &[u8]) -> Result<Self, FormatError> {
		let r = Reader::new(data);
		if r.u16(0)? != XML_TYPE {
			return Err(FormatError::new("not binary XML"));
		}
		let header_size = r.u16(2)? as usize;
		let size = (r.u32(4)? as usize).min(data.len());
		let mut pool = None;
		let mut resource_map = Vec::new();
		let mut resource_map_raw = Vec::new();
		let mut body = Vec::new();
		let mut at = header_size;
		while at + 8 <= size {
			let kind = r.u16(at)?;
			let chunk_size = r.u32(at + 4)? as usize;
			if chunk_size < 8 {
				return Err(FormatError::new("bad chunk size"));
			}
			let chunk = r.slice(at, chunk_size)?;
			match kind {
				strpool::TYPE if pool.is_none() => pool = Some(StringPool::parse(chunk)?),
				RESOURCE_MAP_TYPE if body.is_empty() => {
					let chunk_header = r.u16(at + 2)? as usize;
					let ids = Reader::new(chunk);
					resource_map = (chunk_header..chunk_size)
						.step_by(4)
						.map(|i| ids.u32(i))
						.collect::<Result<_, _>>()?;
					resource_map_raw = chunk.to_vec();
				}
				_ => body.extend_from_slice(chunk),
			}
			at += chunk_size;
		}
		Ok(Self {
			pool: pool.ok_or_else(|| FormatError::new("no string pool"))?,
			resource_map,
			resource_map_raw,
			body,
		})
	}

	pub fn write(&self) -> Vec<u8> {
		let pool = self.pool.write();
		let size = 8 + pool.len() + self.resource_map_raw.len() + self.body.len();
		let mut out = Vec::with_capacity(size);
		out.extend_from_slice(&XML_TYPE.to_le_bytes());
		out.extend_from_slice(&8u16.to_le_bytes());
		out.extend_from_slice(&(size as u32).to_le_bytes());
		out.extend_from_slice(&pool);
		out.extend_from_slice(&self.resource_map_raw);
		out.extend_from_slice(&self.body);
		out
	}

	fn string(&self, index: u32) -> Option<String> {
		(index != NO_STRING)
			.then(|| self.pool.get(index).map(str::to_owned))
			.flatten()
	}

	pub fn elements(&self) -> Result<Vec<Element>, FormatError> {
		let r = Reader::new(&self.body);
		let mut elements = Vec::new();
		let mut stack: Vec<String> = Vec::new();
		let mut at = 0;
		while at + 8 <= self.body.len() {
			let kind = r.u16(at)?;
			let header_size = r.u16(at + 2)? as usize;
			let size = r.u32(at + 4)? as usize;
			if size < 8 {
				return Err(FormatError::new("bad node size"));
			}
			match kind {
				START_ELEMENT => {
					let ext = at + header_size;
					let name = self.string(r.u32(ext + 4)?).unwrap_or_default();
					let attr_start = r.u16(ext + 8)? as usize;
					let attr_size = r.u16(ext + 10)? as usize;
					let attr_count = r.u16(ext + 12)? as usize;
					let mut attributes = Vec::with_capacity(attr_count);
					for i in 0..attr_count {
						let offset = ext + attr_start + i * attr_size;
						let name_index = r.u32(offset + 4)?;
						let raw = r.u32(offset + 8)?;
						let data_type = r.u8(offset + 15)?;
						let data = r.u32(offset + 16)?;
						let value_index = if raw != NO_STRING {
							raw
						} else if data_type == TYPE_STRING {
							data
						} else {
							NO_STRING
						};
						attributes.push(Attribute {
							namespace: self.string(r.u32(offset)?),
							name: self.string(name_index).unwrap_or_default(),
							resource_id: self.resource_map.get(name_index as usize).copied(),
							value: self.string(value_index),
							offset,
						});
					}
					elements.push(Element {
						name: name.clone(),
						ancestors: stack.clone(),
						attributes,
					});
					stack.push(name);
				}
				END_ELEMENT => {
					stack.pop();
				}
				_ => {}
			}
			at += size;
		}
		Ok(elements)
	}

	/// Points the attribute at a string with `text`, leaving the old string
	/// for anything else that uses it.
	pub fn set_string(&mut self, attribute: &Attribute, text: &str) {
		let index = self.pool.intern(text).to_le_bytes();
		let at = attribute.offset;
		self.body[at + 8..at + 12].copy_from_slice(&index);
		self.body[at + 12..at + 14].copy_from_slice(&8u16.to_le_bytes());
		self.body[at + 14] = 0;
		self.body[at + 15] = TYPE_STRING;
		self.body[at + 16..at + 20].copy_from_slice(&index);
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	/// `(android namespace?, name, value)`.
	pub type TestAttr<'a> = (bool, &'a str, &'a str);

	/// Builds a manifest from `(name, depth, attributes)`.
	pub fn manifest(elements: &[(&str, usize, &[TestAttr<'_>])]) -> Vec<u8> {
		let mut strings: Vec<String> = vec![ANDROID_NS.into()];
		let idx = |s: &str, strings: &mut Vec<String>| -> u32 {
			match strings.iter().position(|x| x == s) {
				Some(i) => i as u32,
				None => {
					strings.push(s.into());
					(strings.len() - 1) as u32
				}
			}
		};
		let mut body = Vec::new();
		let mut open: Vec<(usize, u32)> = Vec::new();
		let close = |body: &mut Vec<u8>, name: u32| {
			body.extend_from_slice(&END_ELEMENT.to_le_bytes());
			body.extend_from_slice(&16u16.to_le_bytes());
			body.extend_from_slice(&24u32.to_le_bytes());
			body.extend_from_slice(&[0; 8]);
			body.extend_from_slice(&NO_STRING.to_le_bytes());
			body.extend_from_slice(&name.to_le_bytes());
		};
		for (name, depth, attrs) in elements {
			while open.last().is_some_and(|(d, _)| d >= depth) {
				let (_, n) = open.pop().unwrap();
				close(&mut body, n);
			}
			let name_index = idx(name, &mut strings);
			let size = 16 + 20 + attrs.len() * 20;
			body.extend_from_slice(&START_ELEMENT.to_le_bytes());
			body.extend_from_slice(&16u16.to_le_bytes());
			body.extend_from_slice(&(size as u32).to_le_bytes());
			body.extend_from_slice(&[0; 8]);
			body.extend_from_slice(&NO_STRING.to_le_bytes());
			body.extend_from_slice(&name_index.to_le_bytes());
			body.extend_from_slice(&20u16.to_le_bytes());
			body.extend_from_slice(&20u16.to_le_bytes());
			body.extend_from_slice(&(attrs.len() as u16).to_le_bytes());
			body.extend_from_slice(&[0; 6]);
			for (android, attr, value) in attrs.iter() {
				let ns = if *android { 0 } else { NO_STRING };
				let a = idx(attr, &mut strings);
				let v = idx(value, &mut strings);
				body.extend_from_slice(&ns.to_le_bytes());
				body.extend_from_slice(&a.to_le_bytes());
				body.extend_from_slice(&v.to_le_bytes());
				body.extend_from_slice(&8u16.to_le_bytes());
				body.push(0);
				body.push(TYPE_STRING);
				body.extend_from_slice(&v.to_le_bytes());
			}
			open.push((*depth, name_index));
		}
		while let Some((_, n)) = open.pop() {
			close(&mut body, n);
		}
		let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
		let pool = StringPool::new(false, &refs).write();
		// Real manifests always have a resource map after the pool, and
		// apk-info won't read one without it; an empty map is enough here.
		let mut resource_map = RESOURCE_MAP_TYPE.to_le_bytes().to_vec();
		resource_map.extend_from_slice(&8u16.to_le_bytes());
		resource_map.extend_from_slice(&8u32.to_le_bytes());
		let size = 8 + pool.len() + resource_map.len() + body.len();
		let mut out = Vec::new();
		out.extend_from_slice(&XML_TYPE.to_le_bytes());
		out.extend_from_slice(&8u16.to_le_bytes());
		out.extend_from_slice(&(size as u32).to_le_bytes());
		out.extend_from_slice(&pool);
		out.extend_from_slice(&resource_map);
		out.extend_from_slice(&body);
		out
	}

	#[test]
	fn reads_and_rewrites_attributes() {
		let data = manifest(&[
			("manifest", 0, &[(false, "package", "com.a")]),
			("application", 1, &[]),
			("provider", 2, &[(true, "authorities", "com.a.files")]),
			("queries", 1, &[]),
			("package", 2, &[(true, "name", "com.other")]),
		]);
		let mut xml = Axml::parse(&data).unwrap();
		assert_eq!(
			xml.write(),
			data,
			"unchanged files are written back as read"
		);
		let elements = xml.elements().unwrap();
		assert_eq!(elements.len(), 5);
		assert_eq!(elements[2].ancestors, ["manifest", "application"]);
		assert_eq!(elements[4].ancestors, ["manifest", "queries"]);
		let authorities = &elements[2].attributes[0];
		assert_eq!(authorities.value.as_deref(), Some("com.a.files"));
		assert_eq!(authorities.namespace.as_deref(), Some(ANDROID_NS));

		xml.set_string(&elements[0].attributes[0].clone(), "com.b");
		let xml = Axml::parse(&xml.write()).unwrap();
		let elements = xml.elements().unwrap();
		assert_eq!(elements[0].attributes[0].value.as_deref(), Some("com.b"));
		assert_eq!(
			elements[2].attributes[0].value.as_deref(),
			Some("com.a.files")
		);
	}
}
