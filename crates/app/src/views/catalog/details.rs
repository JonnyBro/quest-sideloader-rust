use super::*;

impl CatalogView {
	pub(super) fn details(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
		let catalog = self.catalog.clone()?;
		let game = catalog.games.get(index)?.clone();
		let theme = cx.theme().clone();
		let status = self.installed.as_ref().map(|apps| apps.status(&game));
		let rank = self.ranks.get(index).copied().flatten();

		let mut warnings = Vec::new();
		if let Some(apps) = &self.installed
			&& let Some((needed, free)) = short_of_space(&game, apps)
		{
			warnings.push(
				t!(
					"lib.low_space",
					needed = format_bytes(needed),
					free = format_bytes(free)
				)
				.into_owned(),
			);
		}
		// The archive and the unpacked release are on disk together for a while.
		if let (Some(free), Some(mb)) = (self.pc_free, game.size_mb) {
			let needed = (mb * 2.0 * 1024.0 * 1024.0) as u64;
			if needed > free {
				warnings.push(
					t!(
						"lib.low_pc_space",
						needed = format_bytes(needed),
						free = format_bytes(free)
					)
					.into_owned(),
				);
			}
		}

		let mut facts: Vec<(String, String, bool)> = vec![
			(t!("lib.release").into(), game.release_name.clone(), true),
			(t!("lib.package").into(), game.package.clone(), true),
			(
				t!("lib.col_version").into(),
				game.version_code.map(|v| v.to_string()).unwrap_or_default(),
				true,
			),
			(t!("lib.col_size").into(), size_text(&game), true),
			(
				t!("lib.col_updated").into(),
				mirror_time(&game.last_updated),
				true,
			),
			(t!("lib.col_popularity").into(), rank_text(rank), true),
		];
		if let Some(apps) = &self.installed
			&& let Some(Some(version)) = apps.versions.get(&game.package)
		{
			let mut text = version.to_string();
			if let Some(at) = apps.installed_at.get(&game.package) {
				text.push_str(&format!(", {}", format_date(*at)));
			}
			facts.push((t!("lib.col_installed").into(), text, true));
			if let Some(latest) = game.version_code.filter(|&latest| latest > *version) {
				let text = if apps.held.contains(&game.package) {
					t!("lib.update_held_value", have = version, latest = latest).into_owned()
				} else {
					format!("{version} -> {latest}")
				};
				facts.push((t!("lib.update").into(), text, true));
			}
		}
		let held = self
			.installed
			.as_ref()
			.filter(|apps| apps.versions.contains_key(&game.package))
			.map(|apps| apps.held.contains(&game.package));

		let install_tip = match &self.installed {
			Some(apps) => t!("lib.installs_on", name = apps.device_name),
			None => t!("lib.connect_install"),
		};
		let note = catalog.note(&game.release_name);
		let thumb_w = DETAILS_WIDTH - 32.;
		Some(
			v_flex()
				.w(px(DETAILS_WIDTH))
				.flex_shrink_0()
				.h_full()
				.border_l_1()
				.border_color(theme.border)
				.bg(theme.sidebar)
				.child(
					h_flex()
						.px_4()
						.pt_4()
						.gap_2()
						.child(div().flex_1().min_w_0().text_lg().child(game.name.clone()))
						.child(
							Button::new("close-details")
								.icon(Icon::new(IconName::Close))
								.ghost()
								.xsmall()
								.on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
						),
				)
				.child(
					div().id("details").flex_1().overflow_y_scroll().child(
						v_flex()
							.p_4()
							.gap_3()
							.child(cover(&catalog, &game, (thumb_w, thumb_w * 0.5625), &theme))
							.child(
								h_flex()
									.gap_2()
									.flex_wrap()
									.child(
										Button::new("install")
											.label(install_label(status))
											.primary()
											.small()
											.disabled(self.installed.is_none())
											.tooltip(install_tip)
											.on_click(cx.listener(move |this, _, _, _| {
												this.download(index, true)
											})),
									)
									.child(
										Button::new("download-only")
											.label(t!("lib.download_only"))
											.outline()
											.small()
											.on_click(cx.listener(move |this, _, _, _| {
												this.download(index, false)
											})),
									)
									.children(held.map(|held| {
										let package = game.package.clone();
										let (label, tip) = if held {
											(t!("lib.resume_updates"), t!("lib.resume_updates_tip"))
										} else {
											(t!("lib.hold_updates"), t!("lib.hold_updates_tip"))
										};
										Button::new("hold-updates")
											.label(label)
											.ghost()
											.small()
											.tooltip(tip)
											.on_click(cx.listener(move |this, _, _, _| {
												this.engine.send(Command::HoldUpdates {
													package: package.clone(),
													hold: !held,
												});
											}))
									})),
							)
							.children(
								warnings.into_iter().map(|text| {
									div().text_sm().text_color(theme.warning).child(text)
								}),
							)
							.child(v_flex().gap_1().text_sm().children(facts.into_iter().map(
								|(label, value, mono)| {
									h_flex()
										.gap_3()
										.child(
											div()
												.w(px(120.))
												.flex_shrink_0()
												.text_color(theme.muted_foreground)
												.child(label),
										)
										.child(
											div().min_w_0().when(mono, |d| d.mono(cx)).child(value),
										)
								},
							)))
							.children(status.and_then(|s| {
								status_tag(s, &theme).map(|tag| {
									h_flex()
										.gap_3()
										.text_sm()
										.child(
											div()
												.w(px(120.))
												.text_color(theme.muted_foreground)
												.child(t!("lib.on_headset").into_owned()),
										)
										.child(tag)
								})
							}))
							.children(note.map(|note| {
								v_flex()
									.gap_1()
									.child(
										div()
											.text_color(theme.muted_foreground)
											.child(t!("lib.note").into_owned()),
									)
									.child(div().text_sm().child(note))
							})),
					),
				)
				.into_any_element(),
		)
	}
}
