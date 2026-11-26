use super::*;

impl HeadsetView {
	pub(super) fn apps_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		h_flex()
			.gap_3()
			.px_5()
			.py_3()
			.child(
				Input::new(&self.filter)
					.prefix(Icon::new(IconName::Search).small())
					.cleanable(true)
					.small()
					.w(px(280.)),
			)
			.child(
				Switch::new("show-system")
					.small()
					.checked(self.show_system)
					.label(if self.show_system || self.hidden == 0 {
						t!("hs.system").into_owned()
					} else {
						format!("{} ({})", t!("hs.system"), self.hidden)
					})
					.on_click(cx.listener(|this, checked: &bool, _, cx| {
						this.show_system = *checked;
						cx.notify();
					})),
			)
			.child(div().flex_1())
			.child(
				Button::new("install-folder")
					.label(t!("hs.install_folder"))
					.outline()
					.small()
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_install(true, cx))),
			)
			.child(
				Button::new("install-file")
					.label(t!("hs.install_file"))
					.primary()
					.small()
					.on_click(cx.listener(|this, _, _, cx| this.pick_and_install(false, cx))),
			)
	}

	/// Picking the current column again flips the order.
	pub(super) fn set_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
		if self.sort == sort {
			self.descending = !self.descending;
		} else {
			self.sort = sort;
			self.descending = sort.starts_descending();
		}
		cx.notify();
	}

	pub(super) fn apps_header_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let c = self.app_cols;
		let theme = cx.theme().clone();
		let (current, descending) = (self.sort, self.descending);
		let column = |sort: Sort, label: String, right: bool| {
			let arrow = (current == sort).then(|| {
				Icon::new(if descending {
					IconName::ChevronDown
				} else {
					IconName::ChevronUp
				})
				.xsmall()
			});
			h_flex()
				.id(SharedString::from(format!("hs-col-{sort:?}")))
				.gap_1()
				.whitespace_nowrap()
				.cursor_pointer()
				.when(right, |d| d.justify_end())
				.when(current == sort, |d| d.text_color(theme.foreground))
				.hover(|s| s.text_color(theme.foreground))
				.on_click(cx.listener(move |this, _, _, cx| this.set_sort(sort, cx)))
				.child(div().min_w_0().truncate().child(label))
				.children(arrow)
		};
		column_header(cx)
			.child(div().w(px(COVER.0)).flex_shrink_0())
			.child(
				column(Sort::Name, t!("hs.col_name").into(), false)
					.flex_1()
					.min_w(px(NAME_MIN)),
			)
			.when(c[0], |h| {
				h.child(
					column(Sort::Package, t!("hs.col_package").into(), false)
						.flex_1()
						.min_w_0(),
				)
			})
			.when(c[1], |h| {
				h.child(
					column(Sort::Version, t!("hs.col_version").into(), true)
						.w(px(VERSION_W))
						.flex_shrink_0(),
				)
			})
			.when(c[2], |h| {
				h.child(
					column(Sort::Size, t!("hs.col_size").into(), true)
						.w(px(SIZE_W))
						.flex_shrink_0(),
				)
			})
			.when(c[3], |h| h.child(div().w(px(TAG_W)).flex_shrink_0()))
			.child(div().w(px(MENU_W)).flex_shrink_0())
	}

	pub(super) fn render_app_rows(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let theme = cx.theme().clone();
		range
			.filter_map(|ix| self.rows.get(ix).map(|row| (ix, row)))
			.map(|(ix, row)| {
				let cover = match &row.cover {
					Some(path) => img(path.clone())
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.object_fit(ObjectFit::Cover)
						.into_any_element(),
					None => div()
						.w(px(COVER.0))
						.h(px(COVER.1))
						.flex_shrink_0()
						.rounded(px(4.))
						.bg(theme.muted)
						.flex()
						.items_center()
						.justify_center()
						.text_color(theme.muted_foreground)
						.child(initial(&row.app.name))
						.into_any_element(),
				};
				let tag = match (row.app.version_code, row.latest) {
					_ if self.copies.contains(&row.app.package) => {
						Some(soft_tag(theme.info, t!("hs.copy").into_owned()).into_any_element())
					}
					(Some(have), Some(latest)) if have >= latest => Some(
						soft_tag(theme.success, t!("hs.latest").into_owned()).into_any_element(),
					),
					(have, Some(latest)) if self.held.contains(&row.app.package) => Some(with_tip(
						("held-tip", ix),
						soft_tag(theme.muted_foreground, t!("hs.update_held").into_owned()),
						update_tip(have, Some(latest), row.installed),
					)),
					(have, Some(latest)) => Some(with_tip(
						("update-tip", ix),
						soft_tag(theme.warning, t!("hs.update").into_owned()),
						update_tip(have, Some(latest), row.installed),
					)),
					_ => None,
				};
				let version = row
					.app
					.version_code
					.map_or_else(String::new, |v| format!("v{v}"));
				let size = row
					.usage
					.map_or_else(|| "...".to_owned(), |u| format_bytes(u.total()));
				let breakdown = row.usage.map(|u| {
					t!(
						"hs.size_breakdown",
						apk = format_bytes(u.apk),
						obb = format_bytes(u.obb),
						data = format_bytes(u.data)
					)
					.into_owned()
				});
				let has_name = row.app.name != row.app.package;

				let c = self.app_cols;
				table_row(("app-row", ix), ROW_HEIGHT, &theme)
					.child(cover)
					.child(
						div()
							.flex_1()
							.min_w(px(NAME_MIN))
							.truncate()
							.child(row.app.name.clone()),
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
								.when(has_name, |d| d.child(row.app.package.clone())),
						)
					})
					.when(c[1], |r| {
						r.child(
							div()
								.w(px(VERSION_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.text_color(theme.muted_foreground)
								.mono(cx)
								.child(version),
						)
					})
					.when(c[2], |r| {
						r.child(
							div()
								.id(("app-size", ix))
								.w(px(SIZE_W))
								.flex_shrink_0()
								.text_right()
								.text_sm()
								.mono(cx)
								.when_some(breakdown, |d, text| {
									d.tooltip(move |window, cx| {
										Tooltip::new(text.clone()).build(window, cx)
									})
								})
								.child(size),
						)
					})
					.when(c[3], |r| {
						r.child(
							h_flex()
								.w(px(TAG_W))
								.flex_shrink_0()
								.justify_end()
								.children(tag),
						)
					})
					.child(self.app_menu(ix, &row.app, cx.entity().downgrade()))
					.into_any_element()
			})
			.collect()
	}

	pub(super) fn app_menu(
		&self,
		ix: usize,
		app: &AppInfo,
		this: WeakEntity<Self>,
	) -> impl IntoElement {
		let engine = self.engine.clone();
		let serial = self.serial.clone().unwrap_or_default();
		let app = app.clone();
		Button::new(("app-menu", ix))
			.icon(Icon::from(AppIcon::Dots))
			.ghost()
			.small()
			.w(px(MENU_W))
			.dropdown_menu(move |menu, _, _| {
				let launch = Command::LaunchApp {
					serial: serial.clone(),
					package: app.package.clone(),
				};
				let stop = Command::StopApp {
					serial: serial.clone(),
					package: app.package.clone(),
				};
				let back_up = Command::BackUpSaves {
					serial: serial.clone(),
					app: app.clone(),
				};
				let (uninstall_serial, uninstall_app) = (serial.clone(), app.clone());
				let uninstall_engine = engine.clone();
				let (log_view, log_app) = (this.clone(), app.clone());
				menu.item(send_item(&engine, t!("hs.launch").into(), launch))
					.item(send_item(&engine, t!("hs.stop").into(), stop))
					.item(
						PopupMenuItem::new(t!("hs.app_log")).on_click(move |_, _, cx| {
							let event = HeadsetEvent::AppLog {
								package: log_app.package.clone(),
								name: log_app.name.clone(),
							};
							let _ = log_view.update(cx, |_, cx| cx.emit(event));
						}),
					)
					.separator()
					.item(send_item(&engine, t!("hs.back_up").into(), back_up))
					.separator()
					.item(PopupMenuItem::new(t!("hs.uninstall_button")).on_click(
						move |_, window, cx| {
							open_uninstall(
								uninstall_engine.clone(),
								uninstall_serial.clone(),
								uninstall_app.clone(),
								window,
								cx,
							)
						},
					))
			})
	}

	pub(super) fn apps_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
		self.rebuild_rows(cx);
		let body = if self.rows.is_empty() {
			centered(t!("hs.nothing").into_owned(), cx)
		} else {
			uniform_list(
				"headset-apps",
				self.rows.len(),
				cx.processor(Self::render_app_rows),
			)
			.flex_1()
			.into_any_element()
		};
		v_flex()
			.size_full()
			.child(self.apps_toolbar(cx))
			.child(self.apps_header_row(cx))
			.child(body)
			.into_any_element()
	}
}

pub(super) fn initial(name: &str) -> String {
	name.chars()
		.find(|c| c.is_alphanumeric())
		.map(|c| c.to_uppercase().to_string())
		.unwrap_or_default()
}

/// With the old dialog's two choices: back up saves first, delete OBB files.
pub(super) fn open_uninstall(
	engine: Rc<Engine>,
	serial: String,
	app: AppInfo,
	window: &mut Window,
	cx: &mut App,
) {
	let back_up = Rc::new(Cell::new(true));
	let remove_obb = Rc::new(Cell::new(true));
	window.open_dialog(cx, move |dialog, _, _| {
		let toggle = |id: &'static str, label: String, value: &Rc<Cell<bool>>| {
			let value = value.clone();
			Checkbox::new(id)
				.label(label)
				.checked(value.get())
				.on_click(move |checked: &bool, window, _| {
					value.set(*checked);
					window.refresh();
				})
		};
		let uninstall = {
			let (engine, serial, app) = (engine.clone(), serial.clone(), app.clone());
			let (back_up, remove_obb) = (back_up.clone(), remove_obb.clone());
			move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
				engine.send(Command::Uninstall {
					serial: serial.clone(),
					app: app.clone(),
					back_up: back_up.get(),
					remove_obb: remove_obb.get(),
				});
				window.close_dialog(cx);
			}
		};
		dialog
			.title(t!("hs.uninstall_title", name = app.name).into_owned())
			.w(px(460.))
			.child(
				v_flex()
					.gap_3()
					.child(toggle("back-up", t!("hs.backup_first").into(), &back_up))
					.child(toggle(
						"remove-obb",
						t!("hs.delete_obb").into(),
						&remove_obb,
					)),
			)
			.footer(
				h_flex()
					.w_full()
					.justify_end()
					.gap_2()
					.child(
						Button::new("cancel")
							.label(t!("common.cancel"))
							.outline()
							.on_click(|_, window, cx| window.close_dialog(cx)),
					)
					.child(
						Button::new("uninstall")
							.label(t!("hs.uninstall"))
							.danger()
							.on_click(uninstall),
					),
			)
	});
}
