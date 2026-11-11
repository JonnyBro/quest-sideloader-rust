//! Game library from a mirror: its configuration, the game list and the
//! metadata archive (`meta.7z`) with thumbnails and notes.

mod fetch;
mod list;
mod mirror;
mod store;

use std::path::PathBuf;

pub use fetch::{FetchProgress, Fetcher};
pub use list::{BadHeader, Game, parse_game_list};
pub use mirror::{Header, Mirror, MirrorKind, rclone_remotes, release_hash};
pub use store::{Catalog, SyncStage, sync};

use crate::archive::ArchiveError;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
	#[error("{0}")]
	Config(String),

	#[error("network error: {0}")]
	Http(String),

	#[error("mirror answered {status} for {url}")]
	Status { status: u16, url: String },

	#[error("rclone binary not found at {0}")]
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

	#[error("the mirror's metadata has no game list")]
	NoGameList,

	#[error(transparent)]
	BadList(#[from] BadHeader),
}

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> CatalogError {
	let path = path.into();
	move |source| CatalogError::Io { path, source }
}
