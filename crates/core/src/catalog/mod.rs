//! Game library from a mirror: its configuration, the game list and the
//! metadata archive (`meta.7z`) with thumbnails and notes.

mod fetch;
mod list;
mod mirror;
mod rclone_config;
mod store;

use std::path::PathBuf;

pub use fetch::{FetchProgress, Fetcher, RcloneAccess, RemoteFile, release_file_path};
pub use list::{BadHeader, Game, parse_game_list};
pub use mirror::{Mirror, MirrorKind, rclone_remotes, release_hash};
pub use rclone_config::check_mirror_rclone_config;
pub use store::{Catalog, SyncStage, UploadTarget, sync};

use crate::archive::ArchiveError;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
	#[error("{0}")]
	Config(String),

	#[error("{}", rust_i18n::t!("err.cat_http", error = .0))]
	Http(String),

	#[error("{}", rust_i18n::t!("err.cat_status", status = .status, url = .url))]
	Status { status: u16, url: String },

	#[error("{}", rust_i18n::t!("err.rclone_not_found", path = .0.display()))]
	RcloneNotFound(PathBuf),

	#[error("rclone: {0}")]
	Rclone(String),

	#[error("{path}: {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},

	#[error(transparent)]
	Archive(#[from] ArchiveError),

	#[error("{}", rust_i18n::t!("err.no_game_list"))]
	NoGameList,

	#[error(transparent)]
	BadList(#[from] BadHeader),
}

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> CatalogError {
	let path = path.into();
	move |source| CatalogError::Io { path, source }
}
