//! Bounds-checked little-endian reads for Android's binary formats.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FormatError(String);

impl FormatError {
	pub fn new(message: impl Into<String>) -> Self {
		Self(message.into())
	}
}

#[derive(Clone, Copy)]
pub struct Reader<'a> {
	data: &'a [u8],
}

impl<'a> Reader<'a> {
	pub fn new(data: &'a [u8]) -> Self {
		Self { data }
	}

	pub fn slice(&self, at: usize, len: usize) -> Result<&'a [u8], FormatError> {
		at.checked_add(len)
			.and_then(|end| self.data.get(at..end))
			.ok_or_else(|| FormatError::new(format!("truncated at {at}")))
	}

	pub fn u8(&self, at: usize) -> Result<u8, FormatError> {
		Ok(self.slice(at, 1)?[0])
	}

	pub fn u16(&self, at: usize) -> Result<u16, FormatError> {
		let b = self.slice(at, 2)?;
		Ok(u16::from_le_bytes([b[0], b[1]]))
	}

	pub fn u32(&self, at: usize) -> Result<u32, FormatError> {
		let b = self.slice(at, 4)?;
		Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
	}
}

pub fn align4(n: usize) -> usize {
	(n + 3) & !3
}
