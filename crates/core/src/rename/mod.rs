//! A second copy of a game under another package name, so it installs beside
//! the original. Only the manifest, a few resource strings and the signature
//! change; every other file in the APK is copied as it is.

mod analyze;
mod arsc;
mod axml;
mod build;
mod chunk;
mod der;
mod dex;
mod keys;
mod sign;
mod strpool;
mod token;
mod verify;
mod zipw;

pub use analyze::{
	Analysis, ChangeKind, ManifestChange, PLATFORM_LOADER, analyze, check_package, obb_name,
	suggest_package,
};
pub use build::{BuildError, BuildRequest, build, output_dir};
pub use keys::{KeyError, KeyStore, SigningKey};
pub use token::rename_value;
