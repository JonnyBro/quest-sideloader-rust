//! A zip writer for APKs: entries are copied as they are (already compressed
//! data isn't touched) and uncompressed ones are aligned like `zipalign`
//! does, which Android needs to map them straight from the file.

use std::io::{Seek, Write};

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
/// Android's alignment extra field, as `apksigner` writes it.
const ALIGNMENT_EXTRA: u16 = 0xd935;
const UTF8_NAMES: u16 = 1 << 11;
pub const STORED: u16 = 0;

/// Native libraries are mapped by page, everything else by word.
pub fn alignment(name: &str) -> u64 {
	if name.ends_with(".so") { 4096 } else { 4 }
}

pub struct EntryInfo {
	pub name: String,
	pub method: u16,
	pub crc32: u32,
	pub compressed_size: u64,
	pub size: u64,
	pub dos_time: u16,
	pub dos_date: u16,
}

pub struct ZipWriter<W: Write + Seek> {
	out: W,
	offset: u64,
	central: Vec<u8>,
	count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ZipWriteError {
	#[error(transparent)]
	Io(#[from] std::io::Error),
	#[error("the APK is too large for a plain zip file (over 4 GiB or 65535 files)")]
	TooLarge,
}

fn u32_field(n: u64) -> Result<u32, ZipWriteError> {
	u32::try_from(n).map_err(|_| ZipWriteError::TooLarge)
}

impl<W: Write + Seek> ZipWriter<W> {
	pub fn new(mut out: W) -> std::io::Result<Self> {
		let offset = out.stream_position()?;
		Ok(Self {
			out,
			offset,
			central: Vec::new(),
			count: 0,
		})
	}

	/// Writes the local header; the caller then writes exactly
	/// `compressed_size` bytes of data through [`Self::data`].
	pub fn start(&mut self, entry: &EntryInfo) -> Result<(), ZipWriteError> {
		let name = entry.name.as_bytes();
		let flags = if entry.name.is_ascii() { 0 } else { UTF8_NAMES };
		let header_offset = self.offset;
		let data_offset = header_offset + 30 + name.len() as u64;
		let extra = if entry.method == STORED {
			alignment_extra(data_offset, alignment(&entry.name))
		} else {
			Vec::new()
		};

		let mut header = Vec::with_capacity(30 + name.len() + extra.len());
		header.extend_from_slice(&LOCAL_HEADER.to_le_bytes());
		header.extend_from_slice(&20u16.to_le_bytes());
		header.extend_from_slice(&flags.to_le_bytes());
		header.extend_from_slice(&entry.method.to_le_bytes());
		header.extend_from_slice(&entry.dos_time.to_le_bytes());
		header.extend_from_slice(&entry.dos_date.to_le_bytes());
		header.extend_from_slice(&entry.crc32.to_le_bytes());
		header.extend_from_slice(&u32_field(entry.compressed_size)?.to_le_bytes());
		header.extend_from_slice(&u32_field(entry.size)?.to_le_bytes());
		header.extend_from_slice(&(name.len() as u16).to_le_bytes());
		header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
		header.extend_from_slice(name);
		header.extend_from_slice(&extra);
		self.out.write_all(&header)?;
		self.offset += header.len() as u64;

		let c = &mut self.central;
		c.extend_from_slice(&CENTRAL_HEADER.to_le_bytes());
		c.extend_from_slice(&20u16.to_le_bytes());
		c.extend_from_slice(&20u16.to_le_bytes());
		c.extend_from_slice(&flags.to_le_bytes());
		c.extend_from_slice(&entry.method.to_le_bytes());
		c.extend_from_slice(&entry.dos_time.to_le_bytes());
		c.extend_from_slice(&entry.dos_date.to_le_bytes());
		c.extend_from_slice(&entry.crc32.to_le_bytes());
		c.extend_from_slice(&u32_field(entry.compressed_size)?.to_le_bytes());
		c.extend_from_slice(&u32_field(entry.size)?.to_le_bytes());
		c.extend_from_slice(&(name.len() as u16).to_le_bytes());
		// Extra field, comment, disk number, internal and external attributes.
		c.extend_from_slice(&[0; 12]);
		c.extend_from_slice(&u32_field(header_offset)?.to_le_bytes());
		c.extend_from_slice(name);
		self.count += 1;
		Ok(())
	}

	pub fn data(&mut self, bytes: &[u8]) -> std::io::Result<()> {
		self.out.write_all(bytes)?;
		self.offset += bytes.len() as u64;
		Ok(())
	}

	pub fn copy(&mut self, reader: &mut impl std::io::Read) -> std::io::Result<()> {
		let n = std::io::copy(reader, &mut self.out)?;
		self.offset += n;
		Ok(())
	}

	/// Ends the entries. Returns the writer, where the central directory
	/// starts, and the central directory and end record, which the v2
	/// signature needs before they are written.
	pub fn finish(self) -> Result<(W, u64, Vec<u8>, Vec<u8>), ZipWriteError> {
		if self.count > usize::from(u16::MAX) {
			return Err(ZipWriteError::TooLarge);
		}
		let mut end = Vec::with_capacity(22);
		end.extend_from_slice(&END_OF_CENTRAL.to_le_bytes());
		end.extend_from_slice(&[0; 4]);
		end.extend_from_slice(&(self.count as u16).to_le_bytes());
		end.extend_from_slice(&(self.count as u16).to_le_bytes());
		end.extend_from_slice(&u32_field(self.central.len() as u64)?.to_le_bytes());
		end.extend_from_slice(&u32_field(self.offset)?.to_le_bytes());
		end.extend_from_slice(&0u16.to_le_bytes());
		Ok((self.out, self.offset, self.central, end))
	}
}

/// Padding that moves the data start to a multiple of `align`.
fn alignment_extra(data_offset: u64, align: u64) -> Vec<u8> {
	let mut pad = ((align - data_offset % align) % align) as usize;
	if pad == 0 {
		return Vec::new();
	}
	// The field's own header and alignment value take 6 bytes.
	while pad < 6 {
		pad += align as usize;
	}
	let mut extra = Vec::with_capacity(pad);
	extra.extend_from_slice(&ALIGNMENT_EXTRA.to_le_bytes());
	extra.extend_from_slice(&((pad - 4) as u16).to_le_bytes());
	extra.extend_from_slice(&(align as u16).to_le_bytes());
	extra.resize(pad, 0);
	extra
}

pub fn set_central_offset(end: &mut [u8], offset: u64) -> Result<(), ZipWriteError> {
	end[16..20].copy_from_slice(&u32_field(offset)?.to_le_bytes());
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn padding_reaches_the_alignment() {
		for offset in 0..20u64 {
			for align in [4u64, 4096] {
				let extra = alignment_extra(offset, align);
				assert_eq!((offset + extra.len() as u64) % align, 0, "{offset} {align}");
				assert!(extra.is_empty() || extra.len() >= 6);
			}
		}
	}
}
