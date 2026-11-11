mod app;
mod ui;

use quest_sideloader_core::{AppPaths, Settings};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> eframe::Result {
	let paths = match AppPaths::from_platform().and_then(|p| p.ensure_exist().map(|()| p)) {
		Ok(paths) => paths,
		Err(e) => {
			eprintln!("fatal: {e}");
			std::process::exit(1);
		}
	};
	let _log_guard = init_logging(&paths);

	let (settings, load_error) = match Settings::load(&paths) {
		Ok(settings) => (settings, None),
		Err(e) => {
			tracing::error!("{e}");
			(Settings::defaults_for(&paths), Some(e.to_string()))
		}
	};

	let options = eframe::NativeOptions {
		viewport: egui::ViewportBuilder::default()
			.with_title("Quest Sideloader")
			.with_inner_size([1100.0, 720.0])
			.with_min_inner_size([800.0, 520.0]),
		..Default::default()
	};

	eframe::run_native(
		"quest-sideloader",
		options,
		Box::new(move |cc| {
			let app = app::SideloaderApp::new(cc, paths, settings, load_error)?;
			Ok(Box::new(app))
		}),
	)
}

fn init_logging(paths: &AppPaths) -> Option<tracing_appender::non_blocking::WorkerGuard> {
	let filter = EnvFilter::try_from_default_env()
		.unwrap_or_else(|_| EnvFilter::new("info,wgpu_hal=warn,wgpu_core=warn,egui_wgpu=warn"));
	let stderr = fmt::layer().with_writer(std::io::stderr);

	match tracing_appender::rolling::Builder::new()
		.rotation(tracing_appender::rolling::Rotation::DAILY)
		.filename_prefix("quest-sideloader")
		.filename_suffix("log")
		.max_log_files(7)
		.build(paths.logs_dir())
	{
		Ok(appender) => {
			let (writer, guard) = tracing_appender::non_blocking(appender);
			let file = fmt::layer().with_ansi(false).with_writer(writer);
			tracing_subscriber::registry()
				.with(filter)
				.with(stderr)
				.with(file)
				.init();
			Some(guard)
		}
		Err(e) => {
			tracing_subscriber::registry()
				.with(filter)
				.with(stderr)
				.init();
			tracing::warn!("file logging disabled: {e}");
			None
		}
	}
}
