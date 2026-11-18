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

	#[error("{}", rust_i18n::t!("err.sl_apk", path = .path.display(), message = .message))]
	Apk { path: PathBuf, message: String },

	#[error("{}", rust_i18n::t!("err.sl_zip", path = .path.display(), message = .message))]
	Zip { path: PathBuf, message: String },

	#[error("{}", rust_i18n::t!("err.sl_nothing", path = .0.display()))]
	NothingToInstall(PathBuf),

	#[error("{}", rust_i18n::t!("err.sl_unsupported", path = .0.display()))]
	Unsupported(PathBuf),

	#[error("{}", rust_i18n::t!("err.sl_script", line = .line, message = .message))]
	Script { line: usize, message: String },

	#[error("{}", rust_i18n::t!("err.sl_cancelled"))]
	Cancelled,

	#[error(transparent)]
	Adb(#[from] AdbError),
}

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> SideloadError {
	let path = path.into();
	move |source| SideloadError::Io { path, source }
}
