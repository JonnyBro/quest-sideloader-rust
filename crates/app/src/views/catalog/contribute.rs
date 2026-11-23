use super::*;

impl CatalogView {
	pub(super) fn contribute_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
		let installed = self.installed.as_ref()?;
		let count = self
			.candidates
			.iter()
			.filter(|c| !self.requested.contains(&c.package))
			.count();
		if count == 0 {
			return None;
		}
		let theme = cx.theme();
		Some(
			h_flex()
				.mx_5()
				.mb_2()
				.px_3()
				.py_2()
				.gap_3()
				.rounded(theme.radius)
				.bg(theme.info.opacity(0.12))
				.border_1()
				.border_color(theme.info.opacity(0.4))
				.text_sm()
				.child(div().flex_1().child(
					t!("con.banner", n = count, device = installed.device_name).into_owned(),
				))
				.child(
					Button::new("contribute")
						.label(t!("con.review"))
						.small()
						.outline()
						.on_click(
							cx.listener(|this, _, window, cx| this.open_contribute(window, cx)),
						),
				),
		)
	}

	pub(super) fn open_contribute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
		let view = cx.entity().downgrade();
		window.open_dialog(cx, move |dialog, _, cx| {
			let Some(view) = view.upgrade() else {
				return dialog;
			};
			let this = view.read(cx);
			let theme = cx.theme();
			let device = this
				.installed
				.as_ref()
				.map(|i| i.device_name.clone())
				.unwrap_or_default();
			let rows = this.candidates.iter().enumerate().map(|(ix, candidate)| {
				let reason = match &candidate.reason {
					Reason::NotInLibrary => {
						t!("con.not_in_library", version = candidate.version_code)
					}
					Reason::Newer { library_version } => t!(
						"con.newer",
						version = candidate.version_code,
						library = library_version
					),
				};
				let actions = if this.requested.contains(&candidate.package) {
					div()
						.text_color(theme.muted_foreground)
						.child(t!("con.queued").into_owned())
						.into_any_element()
				} else {
					let (send, ignore) = (candidate.clone(), candidate.package.clone());
					let (send_view, ignore_view) = (view.clone(), view.clone());
					h_flex()
						.gap_1()
						.child(
							Button::new(("ignore", ix))
								.label(t!("con.ignore"))
								.outline()
								.xsmall()
								.on_click(move |_, _, cx| {
									ignore_view.update(cx, |this, cx| {
										this.engine
											.send(Command::IgnoreContribution(ignore.clone()));
										this.no_upload.insert(ignore.clone());
										this.refresh_candidates();
										cx.notify();
									});
								}),
						)
						.child(
							Button::new(("upload", ix))
								.label(t!("con.upload"))
								.primary()
								.xsmall()
								.on_click(move |_, _, cx| {
									send_view.update(cx, |this, cx| this.upload(&send, cx));
								}),
						)
						.into_any_element()
				};
				h_flex()
					.py_2()
					.gap_3()
					.border_b_1()
					.border_color(theme.border)
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(candidate.name.clone()))
							.child(
								div()
									.text_sm()
									.truncate()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(candidate.package.clone()),
							),
					)
					.child(div().text_sm().mono(cx).child(reason.into_owned()))
					.child(actions)
			});
			let body = if this.candidates.is_empty() {
				div()
					.text_color(theme.muted_foreground)
					.child(t!("con.nothing").into_owned())
					.into_any_element()
			} else {
				div()
					.id("candidates")
					.max_h(px(380.))
					.overflow_y_scroll()
					.children(rows)
					.into_any_element()
			};
			dialog
				.title(t!("con.title").into_owned())
				.w(px(680.))
				.child(
					v_flex()
						.gap_3()
						.child(div().child(t!("con.intro", device = device).into_owned()))
						.child(
							div()
								.text_sm()
								.text_color(theme.muted_foreground)
								.child(t!("con.privacy").into_owned()),
						)
						.child(body),
				)
		});
	}

	/// Each upload needs the user's click in the dialog.
	pub(super) fn upload(&mut self, candidate: &Candidate, cx: &mut Context<Self>) {
		let Some(installed) = &self.installed else {
			return;
		};
		self.requested.insert(candidate.package.clone());
		self.engine.send(Command::Contribute {
			serial: installed.serial.clone(),
			package: candidate.package.clone(),
			name: candidate.name.clone(),
		});
		cx.notify();
	}
}
