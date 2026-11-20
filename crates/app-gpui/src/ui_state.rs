//! What the window remembers between runs: its size, the open tab, view choices.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::shell::Tab;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
	pub x: f32,
	pub y: f32,
	pub width: f32,
	pub height: f32,
	pub maximized: bool,
}

impl SavedBounds {
	pub fn from_window(bounds: WindowBounds) -> Self {
		let (b, maximized) = match bounds {
			WindowBounds::Windowed(b) => (b, false),
			WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
		};
		Self {
			x: b.origin.x.as_f32(),
			y: b.origin.y.as_f32(),
			width: b.size.width.as_f32(),
			height: b.size.height.as_f32(),
			maximized,
		}
	}

	pub fn to_window(self) -> WindowBounds {
		let b = Bounds::new(
			point(px(self.x), px(self.y)),
			size(px(self.width), px(self.height)),
		);
		if self.maximized {
			WindowBounds::Maximized(b)
		} else {
			WindowBounds::Windowed(b)
		}
	}
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
	pub window: Option<SavedBounds>,
	pub tab: Option<Tab>,
	pub logs_open: bool,
	/// The Wi-Fi headset of the last run, reconnected on start.
	pub last_wireless: Option<String>,
	pub headset: HeadsetPrefs,
	pub catalog: crate::views::catalog::CatalogPrefs,
	/// The patched Platform SDK loader picked on the Rename tab.
	pub rename_loader: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeadsetPrefs {
	pub sort: crate::views::headset::Sort,
	pub descending: bool,
	pub show_system: bool,
}

pub fn file(ui_state_file: &Path) -> PathBuf {
	// The egui app keeps its own state in ui.ron next to this.
	ui_state_file.with_file_name("ui.json")
}

impl UiState {
	pub fn load(path: &Path) -> Self {
		let Ok(text) = std::fs::read_to_string(path) else {
			return Self::default();
		};
		serde_json::from_str(&text).unwrap_or_else(|e| {
			tracing::warn!("{}: {e}", path.display());
			Self::default()
		})
	}

	pub fn save(&self, path: &Path) {
		let result = serde_json::to_string_pretty(self)
			.map_err(std::io::Error::other)
			.and_then(|text| std::fs::write(path, text));
		if let Err(e) = result {
			tracing::warn!("could not save {}: {e}", path.display());
		}
	}
}
