use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use quest_sideloader_core::settings::Theme as ThemeSetting;

const THEMES: &str = include_str!("../assets/themes.json");

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
		return;
	}
	let themes = ThemeRegistry::global(cx).themes();
	let dark = themes.get("Quest Dark").cloned();
	let light = themes.get("Quest Light").cloned();
	let theme = Theme::global_mut(cx);
	if let Some(dark) = dark {
		theme.dark_theme = dark;
	}
	if let Some(light) = light {
		theme.light_theme = light;
	}
}

pub fn apply(setting: ThemeSetting, window: &mut Window, cx: &mut App) {
	match setting {
		ThemeSetting::System => Theme::sync_system_appearance(Some(window), cx),
		ThemeSetting::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
		ThemeSetting::Light => Theme::change(ThemeMode::Light, Some(window), cx),
	}
	// Changing the mode reloads the theme config, fonts included.
	apply_fonts(cx);
}

/// gpui panics on text in a family it can't load, so only installed fonts are used.
fn apply_fonts(cx: &mut App) {
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
		theme.font_size = px(15.);
		theme.mono_font_size = px(14.);
	});
}

/// Engineering text: logs, package names, versions, sizes, addresses.
pub trait Mono: Styled + Sized {
	fn mono(self, cx: &App) -> Self {
		self.font_family(cx.theme().mono_font_family.clone())
	}
}

impl<T: Styled> Mono for T {}
