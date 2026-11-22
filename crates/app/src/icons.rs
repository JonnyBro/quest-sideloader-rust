use std::borrow::Cow;

use gpui_kit::assets::{Assets, IconName, icon_assets};
use gpui_kit::component::Icon;
use gpui_kit::{AssetSource, Result, SharedString};

// The default Assets embed only the icons gpui-component itself uses; every other
// Lucide icon the app shows has to be listed here or it renders blank.
icon_assets!(
	ExtraIcons,
	[
		Download,
		File,
		FileSearch,
		Gamepad2,
		LibraryBig,
		MonitorSmartphone,
		RectangleGoggles,
		Upload
	]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
	fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
		match ExtraIcons.load(path)? {
			Some(bytes) => Ok(Some(bytes)),
			None => Assets.load(path),
		}
	}

	fn list(&self, path: &str) -> Result<Vec<SharedString>> {
		let mut paths = Assets.list(path)?;
		paths.extend(ExtraIcons.list(path)?);
		paths.sort();
		paths.dedup();
		Ok(paths)
	}
}

#[derive(Clone, Copy)]
pub enum AppIcon {
	Headset,
	Catalog,
	Downloads,
	Rename,
	Devices,
	Inspector,
	Console,
	Logs,
	Settings,
	Refresh,
	Dots,
	Controller,
	File,
	Upload,
}

impl AppIcon {
	fn name(self) -> IconName {
		match self {
			Self::Headset => IconName::RectangleGoggles,
			Self::Catalog => IconName::LibraryBig,
			Self::Downloads => IconName::Download,
			Self::Rename => IconName::Copy,
			Self::Devices => IconName::MonitorSmartphone,
			Self::Inspector => IconName::FileSearch,
			Self::Console => IconName::SquareTerminal,
			Self::Logs => IconName::FileText,
			Self::Settings => IconName::Settings,
			Self::Refresh => IconName::RefreshCw,
			Self::Dots => IconName::Ellipsis,
			Self::Controller => IconName::Gamepad2,
			Self::File => IconName::File,
			Self::Upload => IconName::Upload,
		}
	}
}

impl From<AppIcon> for Icon {
	fn from(icon: AppIcon) -> Self {
		Icon::new(icon.name())
	}
}
