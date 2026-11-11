//! Installing something the user dropped onto the window: an APK, a release
//! folder or a ZIP with one.

mod plan;
pub mod run;
pub mod script;

use std::path::PathBuf;

pub use plan::{
	ApkMeta, InstallPlan, ObbFolder, Source, extract_zip, is_package_name, read_apk_meta,
};

use crate::adb::AdbError;

#[derive(Debug, thiserror::Error)]
pub enum SideloadError {
	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("cannot read APK {path}: {message}")]
	Apk { path: PathBuf, message: String },

	#[error("cannot extract {path}: {message}")]
	Zip { path: PathBuf, message: String },

	#[error("nothing to install in {0}: no APK, OBB folder or install.txt found")]
	NothingToInstall(PathBuf),

	#[error("unsupported file {0}: drop an .apk, a .zip or a folder")]
	Unsupported(PathBuf),

	#[error("install.txt line {line}: {message}")]
	Script { line: usize, message: String },

	#[error("installation was cancelled")]
	Cancelled,

	#[error(transparent)]
	Adb(#[from] AdbError),
}

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> SideloadError {
	let path = path.into();
	move |source| SideloadError::Io { path, source }
}
