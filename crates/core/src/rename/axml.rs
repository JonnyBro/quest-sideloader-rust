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
pub const TYPE_INT_DEC: u8 = 0x10;
pub const TYPE_INT_HEX: u8 = 0x11;
pub const TYPE_INT_BOOLEAN: u8 = 0x12;
const ATTRIBUTE_SIZE: usize = 20;

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
	offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
	pub namespace: Option<String>,
	pub name: String,
	/// Android attribute id, when the name is in the resource map.
	pub resource_id: Option<u32>,
	/// The value when it is a plain string.
	pub value: Option<String>,
	/// The typed value: numbers, booleans, references.
	pub data_type: u8,
	pub data: u32,
	offset: usize,
}

impl Attribute {
	pub fn is_android(&self, name: &str, id: u32) -> bool {
		self.resource_id == Some(id)
			|| (self.name == name && self.namespace.as_deref() == Some(ANDROID_NS))
	}

	pub fn as_bool(&self) -> Option<bool> {
		(self.data_type == TYPE_INT_BOOLEAN).then_some(self.data != 0)
	}

	pub fn as_int(&self) -> Option<u32> {
		matches!(self.data_type, TYPE_INT_DEC | TYPE_INT_HEX).then_some(self.data)
	}

	/// The value as text, whatever its type: strings as they are, numbers in
	/// decimal, booleans as `true`/`false`, references as `@0x7f...`.
	pub fn text(&self) -> String {
		if let Some(value) = &self.value {
			return value.clone();
		}
		match self.data_type {
			TYPE_INT_BOOLEAN => (self.data != 0).to_string(),
			TYPE_INT_DEC | TYPE_INT_HEX => self.data.to_string(),
			0x01 => format!("@0x{:08x}", self.data),
			_ => format!("0x{:x}", self.data),
		}
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
							resource_id: self
								.resource_map
								.get(name_index as usize)
								.copied()
								.filter(|&id| id != 0),
							value: self.string(value_index),
							data_type,
							data,
							offset,
						});
					}
					elements.push(Element {
						name: name.clone(),
						ancestors: stack.clone(),
						attributes,
						offset: at,
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

	/// Adds the attribute when it isn't there.
	pub fn set_android_bool(
		&mut self,
		element: &str,
		name: &str,
		id: u32,
		value: bool,
	) -> Result<(), FormatError> {
		let data = if value { u32::MAX } else { 0 };
		let elements = self.elements()?;
		let target = elements
			.iter()
			.find(|e| e.name == element)
			.ok_or_else(|| FormatError::new(format!("no <{element}> element")))?;
		if let Some(attribute) = target.attributes.iter().find(|a| a.is_android(name, id)) {
			let at = attribute.offset;
			self.body[at + 8..at + 12].copy_from_slice(&NO_STRING.to_le_bytes());
			self.body[at + 12..at + 14].copy_from_slice(&8u16.to_le_bytes());
			self.body[at + 14] = 0;
			self.body[at + 15] = TYPE_INT_BOOLEAN;
			self.body[at + 16..at + 20].copy_from_slice(&data.to_le_bytes());
			return Ok(());
		}

		// Android finds attributes by resource id, so the name needs one in
		// the resource map; a new string past the map's end gets it, with
		// the strings in between marked as having none.
		let name_index = match self.resource_map.iter().position(|&r| r == id) {
			Some(i) => i as u32,
			None => {
				let i = self.pool.append(name);
				self.resource_map.resize(i as usize, 0);
				self.resource_map.push(id);
				self.rebuild_resource_map();
				i
			}
		};
		let ns_index = self.pool.intern(ANDROID_NS);
		let mut attribute = Vec::with_capacity(ATTRIBUTE_SIZE);
		attribute.extend_from_slice(&ns_index.to_le_bytes());
		attribute.extend_from_slice(&name_index.to_le_bytes());
		attribute.extend_from_slice(&NO_STRING.to_le_bytes());
		attribute.extend_from_slice(&8u16.to_le_bytes());
		attribute.push(0);
		attribute.push(TYPE_INT_BOOLEAN);
		attribute.extend_from_slice(&data.to_le_bytes());

		let r = Reader::new(&self.body);
		let start = target.offset;
		let ext = start + r.u16(start + 2)? as usize;
		let attr_start = r.u16(ext + 8)? as usize;
		if r.u16(ext + 10)? as usize != ATTRIBUTE_SIZE {
			return Err(FormatError::new("unusual attribute size"));
		}
		// Attributes stay sorted by resource id, which the framework relies
		// on; ones without an id come last.
		let position = target
			.attributes
			.iter()
			.position(|a| a.resource_id.is_none_or(|r| r > id))
			.unwrap_or(target.attributes.len());
		let size = r.u32(start + 4)? + ATTRIBUTE_SIZE as u32;
		let count = r.u16(ext + 12)? + 1;
		// The id, class and style attributes are named by 1-based position.
		let mut special = [r.u16(ext + 14)?, r.u16(ext + 16)?, r.u16(ext + 18)?];
		for index in &mut special {
			if *index as usize > position {
				*index += 1;
			}
		}
		let insert_at = ext + attr_start + position * ATTRIBUTE_SIZE;
		self.body.splice(insert_at..insert_at, attribute);
		self.body[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
		self.body[ext + 12..ext + 14].copy_from_slice(&count.to_le_bytes());
		for (i, index) in special.iter().enumerate() {
			let at = ext + 14 + i * 2;
			self.body[at..at + 2].copy_from_slice(&index.to_le_bytes());
		}
		Ok(())
	}

	fn rebuild_resource_map(&mut self) {
		let mut raw = RESOURCE_MAP_TYPE.to_le_bytes().to_vec();
		raw.extend_from_slice(&8u16.to_le_bytes());
		raw.extend_from_slice(&((8 + self.resource_map.len() * 4) as u32).to_le_bytes());
		for id in &self.resource_map {
			raw.extend_from_slice(&id.to_le_bytes());
		}
		self.resource_map_raw = raw;
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
	fn adds_and_sets_a_boolean_in_id_order() {
		let data = manifest(&[
			("manifest", 0, &[(false, "package", "com.a")]),
			(
				"application",
				1,
				&[(true, "label", "Game"), (false, "custom", "x")],
			),
		]);
		let mut xml = Axml::parse(&data).unwrap();
		// Label's id, like a real manifest's resource map would have it.
		let label = xml.pool.iter().position(|s| s == "label").unwrap();
		xml.resource_map.resize(label, 0);
		xml.resource_map.push(0x0101_0001);
		xml.rebuild_resource_map();

		xml.set_android_bool("application", "debuggable", 0x0101_000f, true)
			.unwrap();
		let again = Axml::parse(&xml.write()).unwrap();
		let app = &again.elements().unwrap()[1];
		let names: Vec<&str> = app.attributes.iter().map(|a| a.name.as_str()).collect();
		assert_eq!(names, ["label", "debuggable", "custom"]);
		assert_eq!(app.attributes[1].resource_id, Some(0x0101_000f));
		assert_eq!(app.attributes[1].as_bool(), Some(true));
		assert_eq!(app.attributes[0].value.as_deref(), Some("Game"));

		// Set again: changed in place, not added twice.
		let mut again = again;
		again
			.set_android_bool("application", "debuggable", 0x0101_000f, false)
			.unwrap();
		let app = &Axml::parse(&again.write()).unwrap().elements().unwrap()[1];
		assert_eq!(app.attributes.len(), 3);
		assert_eq!(app.attributes[1].as_bool(), Some(false));
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
