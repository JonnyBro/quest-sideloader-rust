use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("could not determine platform directories")]
	NoPlatformDirs,

	#[error("I/O error at {path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("invalid settings file {path}: {source}")]
	SettingsParse {
		path: PathBuf,
		#[source]
		source: toml::de::Error,
	},

	#[error("failed to serialize settings: {0}")]
	SettingsSerialize(#[from] toml::ser::Error),

	#[error("failed to start async runtime: {0}")]
	Runtime(#[source] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) trait IoContext<T> {
	fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
	fn at(self, path: impl Into<PathBuf>) -> Result<T> {
		self.map_err(|source| Error::Io {
			path: path.into(),
			source,
		})
	}
}
