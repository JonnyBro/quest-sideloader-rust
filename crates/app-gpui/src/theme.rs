use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use quest_sideloader_core::Settings;
use quest_sideloader_core::settings::Theme as ThemeSetting;
use std::sync::OnceLock;

const THEMES: &str = include_str!("../assets/themes.json");
/// Used when the settings name no scheme, or one this build doesn't have.
pub const DEFAULT_DARK: &str = "Quest Dark";
pub const DEFAULT_LIGHT: &str = "Quest Light";

/// A color scheme as the scheme picker shows it.
pub struct Scheme {
	pub name: String,
	pub dark: bool,
	pub background: Hsla,
	pub surface: Hsla,
	pub foreground: Hsla,
	pub accent: Hsla,
}

/// Every bundled scheme, in the order of the themes file.
pub fn schemes() -> &'static [Scheme] {
	static SCHEMES: OnceLock<Vec<Scheme>> = OnceLock::new();
	SCHEMES.get_or_init(|| {
		let file: serde_json::Value = serde_json::from_str(THEMES).unwrap_or_default();
		let color = |colors: &serde_json::Value, key: &str| {
			colors[key]
				.as_str()
				.and_then(|hex| u32::from_str_radix(hex.trim_start_matches('#'), 16).ok())
				.map(|rgb_value| Hsla::from(rgb(rgb_value)))
				.unwrap_or_default()
		};
		file["themes"]
			.as_array()
			.into_iter()
			.flatten()
			.map(|theme| {
				let colors = &theme["colors"];
				Scheme {
					name: theme["name"].as_str().unwrap_or_default().to_owned(),
					dark: theme["mode"] == "dark",
					background: color(colors, "background"),
					surface: color(colors, "sidebar.background"),
					foreground: color(colors, "foreground"),
					accent: color(colors, "primary.background"),
				}
			})
			.collect()
	})
}

/// Tried in order for engineering text (logs, packages, versions). Fira Code is
/// bundled; the rest only matter if loading it failed.
const MONO_FONTS: &[&str] = &[
	"Fira Code",
	"JetBrains Mono",
	"DejaVu Sans Mono",
	"Cascadia Mono",
	"Consolas",
	"Menlo",
];

const UI_FONT: &str = "Inter";

const FONTS_BUNDLED: [&[u8]; 8] = [
	include_bytes!("../assets/fonts/Inter-Regular.ttf"),
	include_bytes!("../assets/fonts/Inter-Medium.ttf"),
	include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
	include_bytes!("../assets/fonts/Inter-Bold.ttf"),
	include_bytes!("../assets/fonts/FiraCode-Regular.ttf"),
	include_bytes!("../assets/fonts/FiraCode-Medium.ttf"),
	include_bytes!("../assets/fonts/FiraCode-SemiBold.ttf"),
	include_bytes!("../assets/fonts/FiraCode-Bold.ttf"),
];

pub fn install(cx: &mut App) {
	let fonts = FONTS_BUNDLED.iter().map(|f| Cow::Borrowed(*f)).collect();
	if let Err(e) = cx.text_system().add_fonts(fonts) {
		tracing::error!("bundled fonts: {e}");
	}
	if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(THEMES) {
		tracing::error!("bundled themes: {e}");
	}
}

/// The theme mode and the dark and light schemes from the settings.
pub fn apply(settings: &Settings, window: &mut Window, cx: &mut App) {
	let pick = |name: &str, fallback: &str| {
		let themes = ThemeRegistry::global(cx).themes();
		themes.get(name).or_else(|| themes.get(fallback)).cloned()
	};
	let dark = pick(&settings.dark_scheme, DEFAULT_DARK);
	let light = pick(&settings.light_scheme, DEFAULT_LIGHT);
	let theme = Theme::global_mut(cx);
	if let Some(dark) = dark {
		theme.dark_theme = dark;
	}
	if let Some(light) = light {
		theme.light_theme = light;
	}
	match settings.theme {
		ThemeSetting::System => Theme::sync_system_appearance(Some(window), cx),
		ThemeSetting::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
		ThemeSetting::Light => Theme::change(ThemeMode::Light, Some(window), cx),
	}
	// Changing the mode reloads the theme config, fonts included.
	apply_fonts(cx);
}

/// The interface scale; every size in the UI follows the theme's font size.
pub struct UiScale(pub f32);

impl Global for UiScale {}

pub const SCALE_MIN: f32 = 0.7;
pub const SCALE_MAX: f32 = 1.6;

pub fn scale(cx: &App) -> f32 {
	cx.try_global::<UiScale>().map_or(1., |s| s.0)
}

/// Rounds to tenths and keeps the scale usable.
pub fn set_scale(scale: f32, window: &mut Window, cx: &mut App) {
	let scale = ((scale * 10.).round() / 10.).clamp(SCALE_MIN, SCALE_MAX);
	cx.set_global(UiScale(scale));
	apply_fonts(cx);
	window.refresh();
}

/// gpui panics on text in a family it can't load, so only installed fonts are used.
fn apply_fonts(cx: &mut App) {
	let scale = scale(cx);
	let installed = cx.text_system().all_font_names();
	let ui = installed
		.iter()
		.any(|f| f == UI_FONT)
		.then(|| SharedString::from(UI_FONT));
	let family = MONO_FONTS
		.iter()
		.find(|f| installed.iter().any(|i| i == *f))
		.map(|f| SharedString::from(*f));
	Theme::update(cx, |theme| {
		if let Some(ui) = ui {
			theme.font_family = ui;
		}
		if let Some(family) = family {
			theme.mono_font_family = family;
		}
		// Sizes in the UI are rem-based, and the rem is this.
		theme.font_size = px(15. * scale);
		theme.mono_font_size = px(14. * scale);
	});
}

/// Engineering text: logs, package names, versions, sizes, addresses.
pub trait Mono: Styled + Sized {
	fn mono(self, cx: &App) -> Self {
		self.font_family(cx.theme().mono_font_family.clone())
	}
}

impl<T: Styled> Mono for T {}
