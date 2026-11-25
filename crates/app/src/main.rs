// Release builds on Windows would otherwise open a console next to the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

rust_i18n::i18n!("locales", fallback = "en");

#[cfg(target_os = "linux")]
mod desktop;
mod dialogs;
mod icons;
mod logbuf;
mod notify;
mod shell;
mod theme;
mod ui_state;
mod views;
mod widgets;

use gpui_kit::*;
use quest_sideloader_core::settings::Language;
use quest_sideloader_core::{AppPaths, Settings};
use tracing_subscriber::prelude::*;

fn main() {
	let paths = match AppPaths::detect().and_then(|p| p.ensure_exist().map(|()| p)) {
		Ok(paths) => paths,
		Err(e) => fatal(&e.to_string()),
	};
	let logs = logbuf::LogBuffer::default();
	let _log_guard = init_logging(&paths, logs.clone());

	// Checked before anything saves the settings.
	let first_run = !paths.settings_file().exists();
	let ui = ui_state::UiState::load(&paths.ui_state_file());
	let scale = ui.scale.unwrap_or(1.);
	let (settings, load_error) = match Settings::load(&paths) {
		Ok(settings) => (settings, None),
		Err(e) => {
			tracing::error!("{e}");
			(Settings::defaults_for(&paths), Some(e.to_string()))
		}
	};
	apply_language(settings.language);
	#[cfg(target_os = "linux")]
	desktop::refresh_if_installed();

	gpui_kit::application()
		.with_assets(icons::AppAssets)
		.run(move |cx: &mut App| {
			gpui_kit::init(cx);
			theme::install(cx);
			cx.set_global(theme::UiScale(scale));

			let options = WindowOptions {
				window_bounds: Some(ui.window.map(|b| b.to_window()).unwrap_or_else(|| {
					WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(780.)), cx))
				})),
				window_min_size: Some(size(px(900.), px(560.))),
				titlebar: Some(TitlebarOptions {
					title: Some(base_title(paths.portable_root().is_some()).into()),
					..Default::default()
				}),
				app_id: Some("quest-sideloader".into()),
				..Default::default()
			};
			let for_theme = settings.clone();
			gpui_kit::open_window(options, cx, move |window, cx| {
				theme::apply(&for_theme, window, cx);
				cx.new(|cx| {
					shell::Shell::new(
						shell::Startup {
							paths,
							settings,
							load_error,
							ui,
							first_run,
							logs,
						},
						window,
						cx,
					)
				})
			})
			.expect("open window");
			cx.on_window_closed(|cx, _| cx.quit()).detach();
			cx.activate(true);
		});
}

pub fn base_title(portable: bool) -> &'static str {
	if portable {
		"Quest Sideloader (portable)"
	} else {
		"Quest Sideloader"
	}
}

fn apply_language(language: Language) {
	let locale = match language {
		Language::English => "en",
		Language::Russian => "ru",
		Language::Ukrainian => "uk",
		Language::System => match sys_locale::get_locale() {
			Some(l) if l.starts_with("uk") => "uk",
			Some(l) if l.starts_with("ru") => "ru",
			_ => "en",
		},
	};
	// One global locale for this crate, the core and gpui-component.
	rust_i18n::set_locale(locale);
}

/// Release builds on Windows have no console, so the error also goes to a dialog.
fn fatal(message: &str) -> ! {
	eprintln!("fatal: {message}");
	rfd::MessageDialog::new()
		.set_level(rfd::MessageLevel::Error)
		.set_title("Quest Sideloader")
		.set_description(message)
		.show();
	std::process::exit(1);
}

/// Logs go to stderr, to the logs panel and to a daily file in the logs folder.
fn init_logging(
	paths: &AppPaths,
	buffer: logbuf::LogBuffer,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
	use tracing_subscriber::{EnvFilter, fmt};
	// zbus warns on every portal file dialog: the request object is gone
	// before it reads its properties, which the dialog doesn't need.
	let default = if cfg!(debug_assertions) {
		"info,quest_sideloader=debug,quest_sideloader_core=debug,wgpu_hal=warn,wgpu_core=warn,zbus=error"
	} else {
		"info,wgpu_hal=warn,wgpu_core=warn,zbus=error"
	};
	let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
	// tracing writes timestamps in UTC by default; these use the local time zone.
	let timer = fmt::time::ChronoLocal::rfc_3339();
	let stderr = fmt::layer()
		.with_timer(timer.clone())
		.with_writer(std::io::stderr);
	let in_app = fmt::layer()
		.with_timer(timer.clone())
		.with_ansi(false)
		.with_target(false)
		.with_writer(buffer);
	match tracing_appender::rolling::Builder::new()
		.rotation(tracing_appender::rolling::Rotation::DAILY)
		.filename_prefix("quest-sideloader")
		.filename_suffix("log")
		.max_log_files(7)
		.build(paths.logs_dir())
	{
		Ok(appender) => {
			let (writer, guard) = tracing_appender::non_blocking(appender);
			let file = fmt::layer()
				.with_timer(timer)
				.with_ansi(false)
				.with_writer(writer);
			tracing_subscriber::registry()
				.with(filter)
				.with(stderr)
				.with(in_app)
				.with(file)
				.init();
			Some(guard)
		}
		Err(e) => {
			tracing_subscriber::registry()
				.with(filter)
				.with(stderr)
				.with(in_app)
				.init();
			tracing::warn!("file logging disabled: {e}");
			None
		}
	}
}
