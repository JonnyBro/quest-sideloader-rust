//! Desktop notifications.

/// Shows a notification without blocking the UI; failures only go to the log
/// (there may be no notification service at all).
pub fn send(title: String, body: String) {
	std::thread::spawn(move || {
		let result = notify_rust::Notification::new()
			.appname("Quest Sideloader")
			.summary(&title)
			.body(&body)
			.show();
		if let Err(e) = result {
			tracing::debug!("notification not shown: {e}");
		}
	});
}
