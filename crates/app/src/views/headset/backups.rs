use super::*;

impl HeadsetView {
	/// Installed apps a backup can go into: renamed copies first, then by name.
	pub(super) fn restore_targets(&self) -> Vec<(bool, String, String)> {
		let mut targets: Vec<(bool, String, String)> = self
			.info
			.iter()
			.flat_map(|i| i.packages.iter())
			.filter(|p| !is_system(&p.name))
			.map(|p| {
				let name = self
					.names
					.get(&p.name)
					.cloned()
					.unwrap_or_else(|| p.name.clone());
				(self.copies.contains(&p.name), name, p.name.clone())
			})
			.collect();
		targets.sort_by_key(|(copy, name, _)| (!*copy, name.to_lowercase()));
		targets
	}

	pub(super) fn restore_into_menu(
		&self,
		ix: usize,
		backup: &Backup,
		targets: Rc<Vec<(bool, String, String)>>,
	) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let backup = backup.clone();
		let any = targets.iter().any(|(_, _, p)| p != &backup.meta.package);
		Button::new(("restore-into", ix))
			.label(t!("hs.restore_into"))
			.outline()
			.xsmall()
			.dropdown_caret(true)
			.disabled(!any || self.serial.is_none())
			.tooltip(t!("hs.restore_into_hint"))
			.dropdown_menu(move |mut menu: PopupMenu, _, _| {
				menu = menu.scrollable(true).max_h(px(320.));
				for (copy, name, package) in targets.iter() {
					if package == &backup.meta.package {
						continue;
					}
					let label = if *copy {
						t!("hs.copy_entry", name = name, package = package).into_owned()
					} else {
						format!("{name} ({package})")
					};
					let engine = engine.clone();
					let serial = serial.clone();
					let backup = backup.clone();
					let (name, package) = (name.clone(), package.clone());
					menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
						confirm(
							&engine,
							Confirm {
								title: t!("hs.restore_into_title").into_owned(),
								text: t!(
									"hs.restore_into_text",
									backup = backup.meta.name,
									date = format_date(backup.meta.created),
									name = name,
									package = package
								)
								.into_owned(),
								ok: t!("hs.restore").into_owned(),
								danger: false,
							},
							Command::RestoreBackup {
								serial: serial.clone(),
								backup: backup.clone(),
								package: package.clone(),
							},
							window,
							cx,
						);
					}));
				}
				menu
			})
	}

	pub(super) fn backups_section(&self, cx: &mut Context<Self>) -> AnyElement {
		if self.backups.is_empty() {
			return centered(t!("hs.no_backups").into_owned(), cx);
		}
		let theme = cx.theme().clone();
		let c = self.backup_cols;
		let header = column_header(cx)
			.child(
				div()
					.flex_1()
					.min_w(px(NAME_MIN))
					.child(t!("hs.col_name").into_owned()),
			)
			.when(c[0], |h| {
				h.child(
					div()
						.flex_1()
						.min_w_0()
						.child(t!("hs.col_package").into_owned()),
				)
			})
			.when(c[1], |h| {
				h.child(
					div()
						.w(px(DATE_W))
						.flex_shrink_0()
						.child(t!("hs.col_date").into_owned()),
				)
			})
			.when(c[2], |h| {
				h.child(
					div()
						.w(px(SIZE_W))
						.flex_shrink_0()
						.text_right()
						.child(t!("hs.col_size").into_owned()),
				)
			})
			.child(div().w(px(BACKUP_ACTIONS_W)).flex_shrink_0());
		let targets = Rc::new(self.restore_targets());
		let rows = self.backups.iter().enumerate().map(|(ix, backup)| {
			let meta = &backup.meta;
			let date = format_date(meta.created);
			let restore = {
				let engine = self.engine.clone();
				let serial = self.serial.clone();
				let backup = backup.clone();
				let title = t!("hs.restore_title", name = meta.name).into_owned();
				Button::new(("restore", ix))
					.label(t!("hs.restore"))
					.outline()
					.xsmall()
					.disabled(serial.is_none())
					.tooltip(t!("hs.restore_hint"))
					.on_click(move |_, window, cx| {
						if let Some(serial) = &serial {
							confirm(
								&engine,
								Confirm {
									title: title.clone(),
									text: t!("hs.restore_hint").into_owned(),
									ok: t!("hs.restore").into_owned(),
									danger: false,
								},
								Command::RestoreBackup {
									serial: serial.clone(),
									backup: backup.clone(),
									package: backup.meta.package.clone(),
								},
								window,
								cx,
							);
						}
					})
			};
			let delete = {
				let engine = self.engine.clone();
				let backup = backup.clone();
				let text = t!("hs.delete_backup_text", name = meta.name, date = date).into_owned();
				Button::new(("delete", ix))
					.icon(Icon::new(IconName::Delete))
					.ghost()
					.xsmall()
					.tooltip(t!("common.delete"))
					.on_click(move |_, window, cx| {
						confirm(
							&engine,
							Confirm {
								title: t!("hs.delete_backup_title").into_owned(),
								text: text.clone(),
								ok: t!("common.delete").into_owned(),
								danger: true,
							},
							Command::DeleteBackup(backup.clone()),
							window,
							cx,
						);
					})
			};
			table_row(("backup-row", ix), ROW_HEIGHT, &theme)
				.child(
					div()
						.flex_1()
						.min_w(px(NAME_MIN))
						.truncate()
						.child(meta.name.clone()),
				)
				.when(c[0], |r| {
					r.child(
						div()
							.flex_1()
							.min_w_0()
							.truncate()
							.text_sm()
							.text_color(theme.muted_foreground)
							.mono(cx)
							.child(meta.package.clone()),
					)
				})
				.when(c[1], |r| {
					r.child(
						div()
							.w(px(DATE_W))
							.flex_shrink_0()
							.text_sm()
							.mono(cx)
							.child(date),
					)
				})
				.when(c[2], |r| {
					r.child(
						div()
							.w(px(SIZE_W))
							.flex_shrink_0()
							.text_right()
							.text_sm()
							.mono(cx)
							.child(format_bytes(meta.bytes)),
					)
				})
				.child(
					h_flex()
						.w(px(BACKUP_ACTIONS_W))
						.flex_shrink_0()
						.gap_1()
						.justify_end()
						.child(restore)
						.child(self.restore_into_menu(ix, backup, targets.clone()))
						.child({
							let dir = backup.dir.clone();
							Button::new(("open", ix))
								.icon(Icon::new(IconName::FolderOpen))
								.ghost()
								.xsmall()
								.tooltip(t!("common.open"))
								.on_click(move |_, _, _| open_folder(&dir))
						})
						.child(delete),
				)
		});
		v_flex()
			.size_full()
			.pt_2()
			.child(header)
			.child(
				div()
					.id("backups")
					.flex_1()
					.overflow_y_scroll()
					.children(rows),
			)
			.into_any_element()
	}
}
