//! The app's look on top of egui's defaults: an accent color, corners and
//! spacing the user picks, and the system's own sans-serif font.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{Color32, CornerRadius, FontData, FontFamily, Theme, Vec2};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

pub const GOOD: Color32 = Color32::from_rgb(59, 109, 17);
pub const WARN: Color32 = Color32::from_rgb(186, 117, 23);
pub const BAD: Color32 = Color32::from_rgb(163, 45, 45);
pub const MUTED: Color32 = Color32::from_rgb(95, 94, 90);

/// Fonts with Cyrillic that ship with the system, first found wins. Without
/// any, egui's own font stays.
const SYSTEM_FONTS: &[&str] = &[
	"C:\\Windows\\Fonts\\segoeui.ttf",
	"/System/Library/Fonts/Supplemental/Arial.ttf",
	"/usr/share/fonts/noto/NotoSans-Regular.ttf",
	"/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
	"/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
	"/usr/share/fonts/TTF/DejaVuSans.ttf",
	"/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

pub fn install_font(ctx: &egui::Context) {
	if let Some(data) = SYSTEM_FONTS
		.iter()
		.find_map(|path| std::fs::read(path).ok())
	{
		// Highest priority, so egui's fonts still cover the few symbols the
		// system font lacks.
		ctx.add_font(FontInsert {
			name: "system".into(),
			data: FontData::from_owned(data),
			families: vec![InsertFontFamily {
				family: FontFamily::Proportional,
				priority: FontPriority::Highest,
			}],
		});
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Accent {
	#[default]
	Violet,
	Blue,
	Teal,
	Green,
	Orange,
	Pink,
	Custom,
}

impl Accent {
	pub const PRESETS: [Accent; 6] = [
		Accent::Violet,
		Accent::Blue,
		Accent::Teal,
		Accent::Green,
		Accent::Orange,
		Accent::Pink,
	];

	/// Dark enough for white text in both themes.
	pub fn color(self) -> Color32 {
		match self {
			Accent::Violet => Color32::from_rgb(83, 74, 183),
			Accent::Blue => Color32::from_rgb(24, 95, 165),
			Accent::Teal => Color32::from_rgb(15, 110, 86),
			Accent::Green => Color32::from_rgb(59, 109, 17),
			Accent::Orange => Color32::from_rgb(153, 60, 29),
			Accent::Pink => Color32::from_rgb(153, 53, 86),
			Accent::Custom => Color32::GRAY,
		}
	}

	/// The same hue light enough to read as link text on a dark background.
	fn light(self) -> Color32 {
		match self {
			Accent::Violet => Color32::from_rgb(175, 169, 236),
			Accent::Blue => Color32::from_rgb(133, 183, 235),
			Accent::Teal => Color32::from_rgb(93, 202, 165),
			Accent::Green => Color32::from_rgb(151, 196, 89),
			Accent::Orange => Color32::from_rgb(240, 153, 123),
			Accent::Pink => Color32::from_rgb(237, 147, 177),
			Accent::Custom => Color32::GRAY,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Corners {
	Square,
	#[default]
	Rounded,
	Round,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Density {
	Compact,
	#[default]
	Normal,
	Spacious,
}

/// How the app looks, kept between runs and applied as soon as it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
	pub accent: Accent,
	/// Used when `accent` is [`Accent::Custom`].
	pub custom: [u8; 3],
	pub corners: Corners,
	pub density: Density,
}

impl Appearance {
	pub fn accent_color(&self) -> Color32 {
		match self.accent {
			Accent::Custom => {
				let [r, g, b] = self.custom;
				Color32::from_rgb(r, g, b)
			}
			preset => preset.color(),
		}
	}

	/// Halfway to white, for a custom color the presets have no light shade of.
	fn link_color(&self) -> Color32 {
		match self.accent {
			Accent::Custom => {
				let [r, g, b] = self.custom.map(|c| c / 2 + 128);
				Color32::from_rgb(r, g, b)
			}
			preset => preset.light(),
		}
	}
}

static CORNERS: AtomicU8 = AtomicU8::new(Corners::Rounded as u8);

/// A corner radius for something drawn by hand, matched to the chosen
/// corners: none when they're square, larger when they're round.
pub fn radius(base: f32) -> f32 {
	match CORNERS.load(Ordering::Relaxed) {
		0 => 0.0,
		2 => base * 1.6,
		_ => base,
	}
}

pub fn apply(ctx: &egui::Context, appearance: &Appearance) {
	let (widget, window) = match appearance.corners {
		Corners::Square => (0, 0),
		Corners::Rounded => (6, 10),
		Corners::Round => (10, 16),
	};
	let (item, button, height) = match appearance.density {
		Density::Compact => (Vec2::new(6.0, 3.0), Vec2::new(5.0, 1.0), 16.0),
		Density::Normal => (Vec2::new(8.0, 6.0), Vec2::new(8.0, 3.0), 20.0),
		Density::Spacious => (Vec2::new(10.0, 9.0), Vec2::new(12.0, 5.0), 26.0),
	};
	CORNERS.store(appearance.corners as u8, Ordering::Relaxed);
	let accent = appearance.accent_color();
	// White text on light colors would vanish; a custom accent can be light.
	let on_accent =
		if accent.r() as u32 * 299 + accent.g() as u32 * 587 + accent.b() as u32 * 114 > 150_000 {
			Color32::BLACK
		} else {
			Color32::WHITE
		};
	ctx.all_styles_mut(|style| {
		style.spacing.item_spacing = item;
		style.spacing.button_padding = button;
		style.spacing.interact_size.y = height;
		let visuals = &mut style.visuals;
		visuals.window_corner_radius = CornerRadius::same(window);
		visuals.menu_corner_radius = CornerRadius::same(window.saturating_sub(2));
		for state in [
			&mut visuals.widgets.noninteractive,
			&mut visuals.widgets.inactive,
			&mut visuals.widgets.hovered,
			&mut visuals.widgets.active,
			&mut visuals.widgets.open,
		] {
			state.corner_radius = CornerRadius::same(widget);
		}
		visuals.selection.bg_fill = accent;
		visuals.selection.stroke.color = on_accent;
	});
	ctx.style_mut_of(Theme::Dark, |style| {
		style.visuals.hyperlink_color = appearance.link_color();
	});
	ctx.style_mut_of(Theme::Light, |style| {
		let visuals = &mut style.visuals;
		visuals.hyperlink_color = accent;
		// egui's light theme is nearly all one off-white; a slightly darker
		// page makes the white panels and cards stand out.
		visuals.panel_fill = Color32::from_gray(238);
		visuals.window_fill = Color32::from_gray(250);
		visuals.faint_bg_color = Color32::from_gray(244);
		visuals.extreme_bg_color = Color32::WHITE;
		visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(226);
		visuals.widgets.inactive.bg_fill = Color32::from_gray(222);
		visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(214);
		visuals.widgets.noninteractive.bg_stroke.color = Color32::from_gray(210);
	});
}

/// A rounded panel around a section of a tab.
pub fn panel(ui: &egui::Ui) -> egui::Frame {
	egui::Frame::group(ui.style())
		.corner_radius(ui.visuals().window_corner_radius)
		.inner_margin(egui::Margin::same(10))
		.fill(ui.visuals().faint_bg_color)
		.stroke(ui.visuals().widgets.noninteractive.bg_stroke)
}

/// A small rounded label on a colored background, like "Installed".
pub fn pill(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) -> egui::Response {
	// Painted at the text's own height, so a tall table row doesn't stretch it.
	let galley = ui.painter().layout_no_wrap(
		text.into(),
		egui::TextStyle::Small.resolve(ui.style()),
		Color32::WHITE,
	);
	let size = galley.size() + Vec2::new(12.0, 4.0);
	let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
	ui.painter()
		.rect_filled(rect, radius(rect.height() / 2.0), color);
	ui.painter()
		.galley(rect.min + Vec2::new(6.0, 2.0), galley, Color32::WHITE);
	response
}

/// A thin bar showing `fraction`, colored by how good the value is.
pub fn meter(ui: &mut egui::Ui, fraction: f32, color: Color32) {
	let width = ui.available_width();
	let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), egui::Sense::hover());
	let painter = ui.painter();
	painter.rect_filled(rect, radius(4.0), ui.visuals().widgets.inactive.bg_fill);
	let mut filled = rect;
	filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
	painter.rect_filled(filled, radius(4.0), color);
}

/// A big muted message in the middle of an empty area.
pub fn empty_state(ui: &mut egui::Ui, title: impl Into<String>, body: impl Into<String>) {
	ui.vertical_centered(|ui| {
		ui.add_space((ui.available_height() / 3.0).max(24.0));
		ui.label(egui::RichText::new(title.into()).heading().weak());
		ui.add_space(4.0);
		ui.weak(body.into());
	});
}
