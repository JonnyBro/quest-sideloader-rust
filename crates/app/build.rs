fn main() {
	println!("cargo:rerun-if-changed=assets/app-icon/quest-sideloader.ico");
	println!("cargo:rerun-if-changed=locales");

	// Build scripts are compiled for the host, so check the target through the env.
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
		// gpui loads the window icon from resource id 1, which set_icon uses.
		winresource::WindowsResource::new()
			.set_icon("assets/app-icon/quest-sideloader.ico")
			.compile()
			.expect("failed to embed the Windows icon");
	}
}
