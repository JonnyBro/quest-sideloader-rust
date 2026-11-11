use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AdbError {
	#[error("adb binary not found at {0}")]
	BinaryNotFound(PathBuf),

	#[error("could not start adb server: {0}")]
	StartFailed(String),

	#[error("adb server is not reachable: {0}")]
	Unreachable(#[source] std::io::Error),

	#[error("adb: {0}")]
	Server(String),

	#[error("adb protocol error: {0}")]
	Protocol(String),

	#[error("adb connection error: {0}")]
	Io(#[from] std::io::Error),

	#[error("adb request timed out")]
	Timeout,
}
