//! Tabler Icons (MIT, `assets/icons/LICENSE`), built into the binary. The
//! SVGs are drawn in white so they can take any color through a tint.

use egui::{Color32, Image, ImageSource, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
	Headset,
	Library,
	Downloads,
	Tasks,
	Settings,
	Devices,
	Rename,
	Inspector,
	Console,
	Logs,
	Refresh,
	Menu,
	Check,
}

impl Icon {
	fn source(self) -> ImageSource<'static> {
		match self {
			Icon::Headset => egui::include_image!("../assets/icons/device-vision-pro.svg"),
			Icon::Library => egui::include_image!("../assets/icons/books.svg"),
			Icon::Downloads => egui::include_image!("../assets/icons/download.svg"),
			Icon::Tasks => egui::include_image!("../assets/icons/list-check.svg"),
			Icon::Settings => egui::include_image!("../assets/icons/settings.svg"),
			Icon::Devices => egui::include_image!("../assets/icons/devices.svg"),
			Icon::Rename => egui::include_image!("../assets/icons/copy.svg"),
			Icon::Inspector => egui::include_image!("../assets/icons/file-search.svg"),
			Icon::Console => egui::include_image!("../assets/icons/terminal-2.svg"),
			Icon::Logs => egui::include_image!("../assets/icons/file-text.svg"),
			Icon::Refresh => egui::include_image!("../assets/icons/refresh.svg"),
			Icon::Menu => egui::include_image!("../assets/icons/dots.svg"),
			Icon::Check => egui::include_image!("../assets/icons/checklist.svg"),
		}
	}

	pub fn image(self, size: f32, color: Color32) -> Image<'static> {
		Image::new(self.source())
			.fit_to_exact_size(Vec2::splat(size))
			.tint(color)
	}
}

/// A full-width row for the sidebar: icon and label, highlighted when
/// selected or hovered. Returns true when clicked.
pub fn nav_item(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool, size: f32) -> bool {
	let height = size + 14.0;
	let (rect, response) = ui.allocate_exact_size(
		Vec2::new(ui.available_width(), height),
		egui::Sense::click(),
	);
	let visuals = ui.visuals();
	let (fill, color) = if selected {
		(
			Some(visuals.selection.bg_fill),
			visuals.selection.stroke.color,
		)
	} else if response.hovered() {
		(
			Some(visuals.widgets.hovered.weak_bg_fill),
			visuals.strong_text_color(),
		)
	} else {
		(None, visuals.text_color())
	};
	if let Some(fill) = fill {
		ui.painter()
			.rect_filled(rect, crate::theme::radius(6.0), fill);
	}
	let icon_rect = egui::Rect::from_center_size(
		egui::pos2(rect.left() + 10.0 + size / 2.0, rect.center().y),
		Vec2::splat(size),
	);
	icon.image(size, color).paint_at(ui, icon_rect);
	let galley = ui.painter().layout_no_wrap(
		label.to_owned(),
		egui::FontId::proportional(size * 0.8 + 2.0),
		color,
	);
	let text_pos = egui::pos2(
		icon_rect.right() + 10.0,
		rect.center().y - galley.size().y / 2.0,
	);
	ui.painter().galley(text_pos, galley, color);
	response.clicked()
}
