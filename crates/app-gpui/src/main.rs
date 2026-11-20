//! Prototype of the gpui-component UI: the Headset screen on the real engine.

rust_i18n::i18n!("locales", fallback = "en");

mod headset;
mod icons;
mod logbuf;
mod shell;
mod theme;

use gpui_kit::*;
use quest_sideloader_core::settings::Language;
use quest_sideloader_core::{AppPaths, Settings};
use tracing_subscriber::prelude::*;

fn main() {
	let logs = logbuf::LogBuffer::default();
	let filter = tracing_subscriber::EnvFilter::try_from_default_env()
		.unwrap_or_else(|_| "info,quest_sideloader_core=debug,wgpu_hal=warn,wgpu_core=warn".into());
	tracing_subscriber::registry()
		.with(filter)
		.with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
		.with(
			tracing_subscriber::fmt::layer()
				.with_ansi(false)
				.with_target(false)
				.with_writer(logs.clone()),
		)
		.init();

	let paths = AppPaths::detect()
		.and_then(|p| p.ensure_exist().map(|()| p))
		.expect("data folders");
	let settings = Settings::load(&paths).unwrap_or_else(|e| {
		tracing::error!("{e}");
		Settings::defaults_for(&paths)
	});
	apply_language(settings.language);

	gpui_kit::application()
		.with_assets(gpui_kit::assets::Assets)
		.run(move |cx: &mut App| {
			gpui_kit::init(cx);
			theme::install(cx);

			let options = WindowOptions {
				window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
					None,
					size(px(1200.), px(780.)),
					cx,
				))),
				window_min_size: Some(size(px(900.), px(560.))),
				titlebar: Some(TitlebarOptions {
					title: Some("Quest Sideloader (gpui prototype)".into()),
					..Default::default()
				}),
				app_id: Some("quest-sideloader-gpui".into()),
				..Default::default()
			};
			let theme_setting = settings.theme;
			gpui_kit::open_window(options, cx, move |window, cx| {
				theme::apply(theme_setting, window, cx);
				cx.new(|cx| shell::Shell::new(paths, settings, logs, window, cx))
			})
			.expect("open window");
			cx.on_window_closed(|cx, _| cx.quit()).detach();
			cx.activate(true);
		});
}

fn apply_language(language: Language) {
	let russian = match language {
		Language::Russian => true,
		Language::English => false,
		Language::System => sys_locale::get_locale().is_some_and(|l| l.starts_with("ru")),
	};
	// One global locale for this crate, the core and gpui-component.
	rust_i18n::set_locale(if russian { "ru" } else { "en" });
}
