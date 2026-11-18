use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AdbError {
	#[error("{}", rust_i18n::t!("err.adb_not_found", path = .0.display()))]
	BinaryNotFound(PathBuf),

	#[error("{}", rust_i18n::t!("err.adb_start", error = .0))]
	StartFailed(String),

	#[error("{}", rust_i18n::t!("err.adb_unreachable", error = .0))]
	Unreachable(#[source] std::io::Error),

	#[error("adb: {0}")]
	Server(String),

	#[error("{}", rust_i18n::t!("err.adb_protocol", error = .0))]
	Protocol(String),

	#[error("{}", rust_i18n::t!("err.adb_io", error = .0))]
	Io(#[from] std::io::Error),

	#[error("{}", rust_i18n::t!("err.adb_timeout"))]
	Timeout,

	#[error("{}", rust_i18n::t!("err.adb_local_io", path = .path.display(), source = .source))]
	LocalIo {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error("{}", rust_i18n::t!("err.adb_push", remote = .remote, message = .message))]
	PushFailed { remote: String, message: String },

	#[error("{}", rust_i18n::t!("err.adb_pull", remote = .remote, message = .message))]
	PullFailed { remote: String, message: String },

	#[error("{}", rust_i18n::t!("err.adb_install", error = .0))]
	InstallFailed(String),
}
