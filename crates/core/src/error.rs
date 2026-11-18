use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
	#[error("{}", rust_i18n::t!("err.no_platform_dirs"))]
	NoPlatformDirs,

	#[error("{}", rust_i18n::t!("err.io", path = .path.display(), source = .source))]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("{}", rust_i18n::t!("err.settings_parse", path = .path.display(), source = .source))]
	SettingsParse {
		path: PathBuf,
		#[source]
		source: toml::de::Error,
	},

	#[error("{}", rust_i18n::t!("err.settings_serialize", error = .0))]
	SettingsSerialize(#[from] toml::ser::Error),

	#[error("{}", rust_i18n::t!("err.runtime", error = .0))]
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
