use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Reads `name.7z.001`, `name.7z.002`, ... as one continuous file. Split 7z
/// volumes are plain byte slices of a single archive.
pub struct MultiVolumeReader {
	volumes: Vec<(PathBuf, u64)>,
	/// Start offset of each volume within the joined stream.
	starts: Vec<u64>,
	total: u64,
	pos: u64,
	current: Option<(usize, File)>,
}

impl MultiVolumeReader {
	pub fn open(paths: &[PathBuf]) -> io::Result<Self> {
		let mut volumes = Vec::with_capacity(paths.len());
		let mut starts = Vec::with_capacity(paths.len());
		let mut total = 0;
		for path in paths {
			let len = std::fs::metadata(path)?.len();
			starts.push(total);
			total += len;
			volumes.push((path.clone(), len));
		}
		Ok(Self {
			volumes,
			starts,
			total,
			pos: 0,
			current: None,
		})
	}

	fn volume_at(&self, pos: u64) -> Option<usize> {
		if pos >= self.total {
			return None;
		}
		Some(self.starts.partition_point(|&start| start <= pos) - 1)
	}
}

impl Read for MultiVolumeReader {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let Some(index) = self.volume_at(self.pos) else {
			return Ok(0);
		};
		let offset = self.pos - self.starts[index];
		let file = match &mut self.current {
			Some((i, file)) if *i == index => file,
			_ => {
				let mut file = File::open(&self.volumes[index].0)?;
				file.seek(SeekFrom::Start(offset))?;
				&mut self.current.insert((index, file)).1
			}
		};
		let remaining_in_volume = self.volumes[index].1 - offset;
		let want = buf
			.len()
			.min(usize::try_from(remaining_in_volume).unwrap_or(usize::MAX));
		let n = file.read(&mut buf[..want])?;
		self.pos += n as u64;
		Ok(n)
	}
}

impl Seek for MultiVolumeReader {
	fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
		let target = match from {
			SeekFrom::Start(p) => Some(p),
			SeekFrom::End(d) => self.total.checked_add_signed(d),
			SeekFrom::Current(d) => self.pos.checked_add_signed(d),
		}
		.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
		if target != self.pos {
			self.pos = target;
			let index = self.volume_at(target);
			let start = index.map(|i| self.starts[i]);
			match (&mut self.current, index, start) {
				(Some((i, file)), Some(index), Some(start)) if *i == index => {
					file.seek(SeekFrom::Start(target - start))?;
				}
				_ => self.current = None,
			}
		}
		Ok(self.pos)
	}
}

/// Given any file of an archive, returns all its volumes in order: either the
/// single `.7z`, or `.7z.001`, `.7z.002`, ... while they exist.
pub fn find_volumes(any: &Path) -> io::Result<Vec<PathBuf>> {
	let name = any
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
	let Some((stem, number)) = name.rsplit_once('.') else {
		return Ok(vec![any.to_path_buf()]);
	};
	if number.len() < 3 || !number.bytes().all(|b| b.is_ascii_digit()) {
		return Ok(vec![any.to_path_buf()]);
	}
	let width = number.len();
	let dir = any.parent().unwrap_or(Path::new("."));
	let mut volumes = Vec::new();
	for n in 1.. {
		let path = dir.join(format!("{stem}.{n:0width$}"));
		if !path.is_file() {
			break;
		}
		volumes.push(path);
	}
	if volumes.is_empty() {
		return Err(io::Error::new(
			io::ErrorKind::NotFound,
			format!("first volume {stem}.{:0width$} is missing", 1),
		));
	}
	Ok(volumes)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn split(dir: &Path, data: &[u8], sizes: &[usize]) -> Vec<PathBuf> {
		let mut paths = Vec::new();
		let mut rest = data;
		for (i, &size) in sizes.iter().enumerate() {
			let (chunk, tail) = rest.split_at(size.min(rest.len()));
			let path = dir.join(format!("a.7z.{:03}", i + 1));
			std::fs::write(&path, chunk).unwrap();
			paths.push(path);
			rest = tail;
		}
		paths
	}

	#[test]
	fn reads_and_seeks_across_volumes() {
		let dir = tempfile::tempdir().unwrap();
		let data: Vec<u8> = (0..10_000u32).map(|i| (i % 253) as u8).collect();
		let paths = split(dir.path(), &data, &[3000, 1, 4999, 2000]);

		let mut reader = MultiVolumeReader::open(&paths).unwrap();
		let mut all = Vec::new();
		reader.read_to_end(&mut all).unwrap();
		assert_eq!(all, data);

		for pos in [0u64, 2999, 3000, 3001, 7999, 9999] {
			reader.seek(SeekFrom::Start(pos)).unwrap();
			let mut byte = [0u8; 1];
			reader.read_exact(&mut byte).unwrap();
			assert_eq!(byte[0], data[pos as usize], "at {pos}");
		}
		assert_eq!(reader.seek(SeekFrom::End(-10)).unwrap(), 9990);
		let mut tail = Vec::new();
		reader.read_to_end(&mut tail).unwrap();
		assert_eq!(tail, &data[9990..]);
		assert!(reader.seek(SeekFrom::Current(-20_000)).is_err());
	}

	#[test]
	fn finds_volumes() {
		let dir = tempfile::tempdir().unwrap();
		let paths = split(dir.path(), b"abcdef", &[2, 2, 2]);
		assert_eq!(find_volumes(&paths[1]).unwrap(), paths);

		let single = dir.path().join("meta.7z");
		std::fs::write(&single, b"x").unwrap();
		assert_eq!(
			find_volumes(&single).unwrap(),
			std::slice::from_ref(&single)
		);

		std::fs::remove_file(&paths[0]).unwrap();
		assert!(find_volumes(&paths[1]).is_err());
	}
}
