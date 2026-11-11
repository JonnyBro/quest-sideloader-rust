//! Async client for the ADB server protocol (the one `adb` itself speaks to
//! `adb server` on 127.0.0.1:5037). The `adb` binary is only used to start the server.

mod device;
mod error;
mod parse;
mod protocol;
mod server;
mod sync;
#[cfg(test)]
pub(crate) mod testing;

pub use device::{Device, ShellOutput};
pub use error::AdbError;
pub use parse::{Battery, DeviceEntry, DeviceInfo, DeviceState, Package, Storage};
pub use server::{AdbServer, DEFAULT_PORT, DeviceTracker};
pub use sync::{LocalFile, Progress, list_files};
