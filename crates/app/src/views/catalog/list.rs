use super::*;

impl CatalogView {
	pub(super) fn list(&mut self, cx: &mut Context<Self>) -> AnyElement {
		let theme = cx.theme().clone();
		let (_, current, descending) = self.effective();
		let connected = self.installed.is_some();
		let column = |sort: Sort, label: String, width: Option<f32>, right: bool| {
			let arrow = (current == sort).then(|| {
				Icon::new(if descending {
					IconName::ChevronDown
				} else {
					IconName::ChevronUp
				})
				.xsmall()
			});
			h_flex()
				.id(SharedString::from(format!("col-{sort:?}")))
				.gap_1()
				.whitespace_nowrap()
				.when_some(width, |d, w| d.w(px(w)).flex_shrink_0())
				.when(width.is_none(), |d| d.flex_1().min_w_0())
				.when(right, |d| d.justify_end())
				.when(current == sort, |d| d.text_color(theme.foreground))
				.when(sort != Sort::Status || connected, |d| {
					d.cursor_pointer()
						.hover(|s| s.text_color(theme.foreground))
						.on_click(cx.listener(move |this, _, _, cx| this.set_sort(sort, cx)))
				})
				.child(div().min_w_0().truncate().child(label))
				.children(arrow)
		};
		let c = self.cols;
		let header = column_header(cx)
			.child(div().w(px(COVER.0)).flex_shrink_0())
			.child(column(Sort::Name, t!("lib.col_game").into(), None, false).min_w(px(NAME_MIN)))
			.when(c[0], |h| {
				h.child(column(
					Sort::Version,
					t!("lib.col_version").into(),
					Some(VERSION_W),
					true,
				))
			})
			.when(connected && c[1], |h| {
				h.child(
					div()
						.w(px(INSTALLED_W))
						.flex_shrink_0()
						.text_right()
						.whitespace_nowrap()
						.child(t!("lib.col_installed").into_owned()),
				)
			})
			.when(c[2], |h| {
				h.child(column(
					Sort::Size,
					t!("lib.col_size").into(),
					Some(SIZE_W),
					true,
				))
			})
			.when(c[3], |h| {
				h.child(column(
					Sort::Updated,
					t!("lib.col_updated").into(),
					Some(DATE_W),
					true,
				))
			})
			.when(c[4], |h| {
				h.child(column(
					Sort::Popularity,
					t!("lib.col_popularity").into(),
					Some(RANK_W),
					true,
				))
			})
			.when(c[5], |h| {
				h.child(column(
					Sort::Status,
					t!("lib.col_status").into(),
					Some(STATUS_W),
					true,
				))
			});
		v_flex()
			.size_full()
			.child(header)
			.child(
				uniform_list(
					"catalog-rows",
					self.rows.len(),
					cx.processor(Self::list_rows),
				)
				.track_scroll(&self.list_scroll)
				.flex_1(),
			)
			.into_any_element()
	}

	pub(super) fn list_rows(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let Some(catalog) = self.catalog.clone() else {
			return Vec::new();
		};
		let theme = cx.theme().clone();
		range
			.filter_map(|pos| self.rows.get(pos).map(|&index| (pos, index)))
			.map(|(pos, index)| {
				let game = &catalog.games[index];
				let status = self.installed.as_ref().map(|apps| apps.status(game));
				let installed_version = self
					.installed
					.as_ref()
					.and_then(|apps| apps.versions.get(&game.package).copied());
				let selected = self.selected == Some(index);
				let numbers = |text: String, width: f32| {
					div()
						.w(px(width))
						.flex_shrink_0()
						.text_right()
						.text_sm()
						.mono(cx)
						.child(text)
				};
				let c = self.cols;
				table_row(("game", pos), ROW_HEIGHT, &theme)
					.when(selected, |r| r.bg(theme.list_active))
					.on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
					.child(cover(&catalog, game, COVER, &theme))
					.child(
						v_flex()
							.flex_1()
							.min_w(px(NAME_MIN))
							.child(div().truncate().child(game.name.clone()))
							.child(
								div()
									.truncate()
									.text_sm()
									.text_color(theme.muted_foreground)
									.mono(cx)
									.child(game.release_name.clone()),
							),
					)
					.when(c[0], |r| {
						r.child(numbers(
							game.version_code.map(|v| v.to_string()).unwrap_or_default(),
							VERSION_W,
						))
					})
					.when(self.installed.is_some() && c[1], |r| {
						r.child(
							numbers(
								installed_version
									.flatten()
									.map(|v| v.to_string())
									.unwrap_or_default(),
								INSTALLED_W,
							)
							.text_color(theme.muted_foreground),
						)
					})
					.when(c[2], |r| r.child(numbers(size_text(game), SIZE_W)))
					.when(c[3], |r| {
						r.child(numbers(short_date(&game.last_updated), DATE_W))
					})
					.when(c[4], |r| {
						r.child(numbers(rank_text(self.ranks[index]), RANK_W))
					})
					.when(c[5], |r| {
						r.child(
							h_flex()
								.w(px(STATUS_W))
								.flex_shrink_0()
								.justify_end()
								.children(status.and_then(|s| {
									let tip =
										self.installed.as_ref().and_then(|a| a.update_tip(game));
									status_badge(("status-row", index), s, &theme, tip)
								})),
						)
					})
					.context_menu(self.menu(index))
					.into_any_element()
			})
			.collect()
	}

	pub(super) fn grid(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
		let details = if self.selected.is_some() {
			DETAILS_WIDTH
		} else {
			0.
		};
		// gpui has no element width before layout; the window width minus the
		// sidebar and the details panel is close enough for the column count.
		let available = content_width(window, details) - 40.;
		self.grid_columns = ((available + CARD_GAP) / (CARD_WIDTH + CARD_GAP)).max(1.) as usize;
		let lines = self.rows.len().div_ceil(self.grid_columns);
		uniform_list("catalog-grid", lines, cx.processor(Self::grid_lines))
			.track_scroll(&self.list_scroll)
			.size_full()
			.into_any_element()
	}

	pub(super) fn grid_lines(
		&mut self,
		range: Range<usize>,
		_window: &mut Window,
		cx: &mut Context<Self>,
	) -> Vec<AnyElement> {
		let Some(catalog) = self.catalog.clone() else {
			return Vec::new();
		};
		let theme = cx.theme().clone();
		let columns = self.grid_columns;
		let thumb = (CARD_WIDTH - 16., (CARD_WIDTH - 16.) * 0.5625);
		range
			.map(|line| {
				let start = line * columns;
				let end = (start + columns).min(self.rows.len());
				let cards = self.rows[start..end].iter().map(|&index| {
					let game = &catalog.games[index];
					let selected = self.selected == Some(index);
					let status = self.installed.as_ref().map(|apps| apps.status(game));
					let mut meta = Vec::new();
					if let Some(version) = game.version_code {
						meta.push(format!("v{version}"));
					}
					let size = size_text(game);
					if !size.is_empty() {
						meta.push(size);
					}
					v_flex()
						.id(("card", index))
						.w(px(CARD_WIDTH))
						.p_2()
						.gap_1()
						.rounded(theme.radius_lg)
						.border_1()
						.border_color(if selected {
							theme.list_active_border
						} else {
							theme.border
						})
						.bg(if selected {
							theme.list_active
						} else {
							theme.muted
						})
						.hover(|s| s.bg(theme.list_hover))
						.on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
						.child(cover(&catalog, game, thumb, &theme))
						.child(div().truncate().child(game.name.clone()))
						.child(
							h_flex()
								.gap_2()
								.text_sm()
								.child(
									div()
										.flex_1()
										.truncate()
										.text_color(theme.muted_foreground)
										.mono(cx)
										.child(meta.join("  ")),
								)
								.children(status.and_then(|s| {
									let tip =
										self.installed.as_ref().and_then(|a| a.update_tip(game));
									status_badge(("status-card", index), s, &theme, tip)
								})),
						)
						.context_menu(self.menu(index))
				});
				h_flex()
					.px_5()
					.pb(px(CARD_GAP))
					.gap(px(CARD_GAP))
					.children(cards)
					.into_any_element()
			})
			.collect()
	}

	/// The right-click menu of a game.
	pub(super) fn menu(
		&self,
		index: usize,
	) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
		let view = self.this.clone();
		move |menu, _, cx| {
			let Some(view) = view.upgrade() else {
				return menu;
			};
			let this = view.read(cx);
			let Some(game) = this
				.catalog
				.as_ref()
				.and_then(|c| c.games.get(index))
				.cloned()
			else {
				return menu;
			};
			let status = this.installed.as_ref().map(|apps| apps.status(&game));
			let item = |label: String, f: MenuAction| {
				let view = view.downgrade();
				PopupMenuItem::new(label).on_click(move |_, _, cx| {
					let f = f.clone();
					let _ = view.update(cx, move |this, cx| f(this, cx));
				})
			};
			let copy = |label: String, text: String| {
				PopupMenuItem::new(label).on_click(move |_, _, cx| {
					cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
				})
			};
			let mut menu = menu.item(item(
				t!("lib.menu_details").into(),
				Rc::new(move |this, cx| this.select(Some(index), cx)),
			));
			menu = menu.separator();
			if let Some(status) = status {
				menu = menu.item(item(
					install_label(Some(status)),
					Rc::new(move |this, _| this.download(index, true)),
				));
			}
			menu.item(item(
				t!("lib.download_only").into(),
				Rc::new(move |this, _| this.download(index, false)),
			))
			.separator()
			.item(copy(t!("lib.copy_name").into(), game.name.clone()))
			.item(copy(
				t!("lib.copy_release").into(),
				game.release_name.clone(),
			))
			.item(copy(t!("lib.copy_package").into(), game.package.clone()))
		}
	}
}

pub(super) fn cover(catalog: &Catalog, game: &Game, size: (f32, f32), theme: &Theme) -> AnyElement {
	match catalog.thumbnail(&game.package) {
		Some(path) => img(path)
			.w(px(size.0))
			.h(px(size.1))
			.flex_shrink_0()
			.rounded(px(4.))
			.object_fit(ObjectFit::Cover)
			.into_any_element(),
		None => div()
			.w(px(size.0))
			.h(px(size.1))
			.flex_shrink_0()
			.rounded(px(4.))
			.bg(theme.muted)
			.border_1()
			.border_color(theme.border)
			.flex()
			.items_center()
			.justify_center()
			.text_color(theme.muted_foreground)
			.child(
				game.name
					.chars()
					.find(|c| c.is_alphanumeric())
					.map(|c| c.to_uppercase().to_string())
					.unwrap_or_default(),
			)
			.into_any_element(),
	}
}

/// The status tag, saying which versions an update is about on hover.
pub(super) fn status_badge(
	id: impl Into<ElementId>,
	status: GameStatus,
	theme: &Theme,
	tip: Option<String>,
) -> Option<AnyElement> {
	let tag = status_tag(status, theme)?;
	Some(match tip {
		Some(tip) => with_tip(id, tag, tip),
		None => tag.into_any_element(),
	})
}

pub(super) fn status_tag(status: GameStatus, theme: &Theme) -> Option<impl IntoElement> {
	match status {
		GameStatus::NotInstalled => None,
		GameStatus::Installed => Some(soft_tag(
			theme.success,
			t!("lib.status_installed").into_owned(),
		)),
		GameStatus::UpdateAvailable => Some(soft_tag(
			theme.warning,
			t!("lib.status_update").into_owned(),
		)),
	}
}
