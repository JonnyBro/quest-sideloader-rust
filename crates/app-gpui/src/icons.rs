use gpui_kit::component::Icon;

macro_rules! icons {
	($($name:ident => $file:literal,)*) => {
		#[derive(Clone, Copy)]
		pub enum AppIcon {
			$($name,)*
		}

		impl AppIcon {
			fn bytes(self) -> &'static [u8] {
				match self {
					$(Self::$name => include_bytes!(concat!("../../app/assets/icons/", $file)),)*
				}
			}
		}
	};
}

icons! {
	Headset => "device-vision-pro.svg",
	Catalog => "books.svg",
	Downloads => "download.svg",
	Rename => "copy.svg",
	Devices => "devices.svg",
	Inspector => "file-search.svg",
	Console => "terminal-2.svg",
	Logs => "file-text.svg",
	Settings => "settings.svg",
	Refresh => "refresh.svg",
	Dots => "dots.svg",
}

impl From<AppIcon> for Icon {
	fn from(icon: AppIcon) -> Self {
		Icon::default().data(icon.bytes())
	}
}
