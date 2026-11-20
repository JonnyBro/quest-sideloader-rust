// Release builds on Windows would otherwise open a console next to the window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

rust_i18n::i18n!("locales", fallback = "en");

mod app;
mod icons;
mod logbuf;
mod notify;
mod theme;
mod ui;

use quest_sideloader_core::{AppPaths, Settings};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> eframe::Result {
	let paths = match AppPaths::detect().and_then(|p| p.ensure_exist().map(|()| p)) {
		Ok(paths) => paths,
		Err(e) => fatal(&e.to_string()),
	};
	let log_buffer = logbuf::LogBuffer::default();
	let _log_guard = init_logging(&paths, log_buffer.clone());

	// Checked before anything saves the settings.
	let first_run = !paths.settings_file().exists();
	let (settings, load_error) = match Settings::load(&paths) {
		Ok(settings) => (settings, None),
		Err(e) => {
			tracing::error!("{e}");
			(Settings::defaults_for(&paths), Some(e.to_string()))
		}
	};

	let options = eframe::NativeOptions {
		viewport: egui::ViewportBuilder::default()
			.with_title(app::base_title(paths.portable_root().is_some()))
			.with_inner_size([1100.0, 720.0])
			.with_min_inner_size([800.0, 520.0]),
		persistence_path: Some(paths.ui_state_file()),
		..Default::default()
	};

	eframe::run_native(
		"quest-sideloader",
		options,
		Box::new(move |cc| {
			let app =
				app::SideloaderApp::new(cc, paths, settings, load_error, first_run, log_buffer)?;
			Ok(Box::new(app))
		}),
	)
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

fn init_logging(
	paths: &AppPaths,
	buffer: logbuf::LogBuffer,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
	let default = if cfg!(debug_assertions) {
		"info,quest_sideloader=debug,quest_sideloader_core=debug,wgpu_hal=warn,wgpu_core=warn,egui_wgpu=warn"
	} else {
		"info,wgpu_hal=warn,wgpu_core=warn,egui_wgpu=warn"
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
