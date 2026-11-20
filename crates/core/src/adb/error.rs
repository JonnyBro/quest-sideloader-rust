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

	#[error("{}", install_message(.0))]
	InstallFailed(String),
}

/// Package manager failure codes and what they mean for the user.
const INSTALL_REASONS: &[(&str, &str)] = &[
	("INSTALL_FAILED_INSUFFICIENT_STORAGE", "err.inst_storage"),
	("INSTALL_FAILED_UPDATE_INCOMPATIBLE", "err.inst_signature"),
	("INSTALL_FAILED_VERSION_DOWNGRADE", "err.inst_downgrade"),
	("INSTALL_FAILED_OLDER_SDK", "err.inst_older_sdk"),
	("INSTALL_FAILED_NO_MATCHING_ABIS", "err.inst_abi"),
	("INSTALL_FAILED_DUPLICATE_PERMISSION", "err.inst_permission"),
	("INSTALL_FAILED_CONFLICTING_PROVIDER", "err.inst_provider"),
	("INSTALL_FAILED_USER_RESTRICTED", "err.inst_restricted"),
	("INSTALL_PARSE_FAILED", "err.inst_broken"),
];

/// A plain explanation in front of the package manager's own words, which
/// stay for searching and bug reports.
fn install_message(raw: &str) -> String {
	let raw = raw.trim();
	match INSTALL_REASONS.iter().find(|(code, _)| raw.contains(code)) {
		Some((_, key)) => rust_i18n::t!(
			"err.adb_install_explained",
			reason = rust_i18n::t!(*key),
			error = raw
		)
		.into_owned(),
		None => rust_i18n::t!("err.adb_install", error = raw).into_owned(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn explains_known_failures() {
		let message =
			AdbError::InstallFailed("Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]".into())
				.to_string();
		assert!(message.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE"));
		assert!(
			message.len()
				> "installation failed: Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]".len()
		);
		let unknown = AdbError::InstallFailed("Failure [SOMETHING_NEW]".into()).to_string();
		assert!(unknown.contains("SOMETHING_NEW"));
	}
}
