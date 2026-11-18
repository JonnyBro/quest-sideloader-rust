rust_i18n::i18n!("locales", fallback = "en");

pub mod adb;
pub mod archive;
pub mod catalog;
pub mod contribute;
pub mod downloaded;
pub mod engine;
pub mod error;
mod format;
pub mod paths;
pub mod platform_tools;
pub mod rclone_tool;
pub mod saves;
pub mod settings;
pub mod sideload;

pub use engine::{
	AdbStatus, AppInfo, CatalogStatus, Command, Engine, Event, ReviewDecision, SignatureConflict,
	TaskId, TaskKind, TaskSnapshot, TaskState,
};
pub use error::{Error, Result};
pub use format::format_bytes;
pub use paths::AppPaths;
pub use settings::Settings;
