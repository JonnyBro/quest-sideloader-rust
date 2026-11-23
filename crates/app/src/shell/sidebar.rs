use super::*;

impl Shell {
	pub(super) fn device_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let theme = cx.theme();
		let (dot, title, subtitle) = match self.selected_device() {
			Some(device) => (
				theme.success,
				device.display_name(),
				transport_label(device),
			),
			None => match self.devices.first() {
				Some(device) => (
					theme.warning,
					device.display_name(),
					state_hint(&device.state),
				),
				None => (
					theme.muted_foreground,
					t!("dev.none").into_owned(),
					t!("dev.connect_hint").into_owned(),
				),
			},
		};

		let devices = self.devices.clone();
		let current = self.selected.clone();
		let shell = cx.entity().downgrade();
		let header = SidebarHeader::new()
			.w_full()
			.child(
				h_flex()
					.w_full()
					.gap_2()
					.child(div().size_2().flex_shrink_0().rounded_full().bg(dot))
					.child(
						v_flex()
							.flex_1()
							.min_w_0()
							.child(div().truncate().child(title))
							.child(
								div()
									.text_sm()
									.text_color(theme.muted_foreground)
									.truncate()
									.child(subtitle),
							),
					)
					.child(
						Icon::new(IconName::ChevronsUpDown)
							.small()
							.text_color(theme.muted_foreground),
					),
			)
			.dropdown_menu(move |mut menu, _, _| {
				for device in &devices {
					let serial = device.serial.clone();
					let online = device.state == DeviceState::Online;
					let detail = if online {
						transport_label(device)
					} else {
						state_hint(&device.state)
					};
					let shell = shell.clone();
					menu = menu.item(
						PopupMenuItem::new(format!("{}  {detail}", device.display_name()))
							.checked(current.as_deref() == Some(serial.as_str()))
							.disabled(!online)
							.on_click(move |_, _, cx| {
								let serial = serial.clone();
								let _ = shell.update(cx, |s, cx| s.select(Some(serial), cx));
							}),
					);
				}
				if !devices.is_empty() {
					menu = menu.separator();
				}
				let wifi = shell.clone();
				let manage = shell.clone();
				menu.item(
					PopupMenuItem::new(t!("wifi.menu"))
						.icon(Icon::new(IconName::Globe))
						.on_click(move |_, window, cx| {
							let _ = wifi.update(cx, |s, cx| s.open_wifi_dialog(window, cx));
						}),
				)
				.item(
					PopupMenuItem::new(t!("dev.manage"))
						.icon(Icon::from(AppIcon::Devices))
						.on_click(move |_, _, cx| {
							let _ = manage.update(cx, |s, cx| {
								s.tab = Tab::Devices;
								cx.notify();
							});
						}),
				)
			});
		// The menu wraps the header in a container as wide as its content; a column
		// stretches that container to the sidebar's width.
		v_flex().flex_1().min_w_0().child(header)
	}

	pub(super) fn nav_item(&self, tab: Tab, cx: &mut Context<Self>) -> SidebarMenuItem {
		let mut item = SidebarMenuItem::new(tab.label())
			.icon(Icon::from(tab.icon()))
			.active(self.tab == tab)
			.on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)));
		let active = self.active_tasks().count();
		if tab == Tab::Downloads && active > 0 {
			item = item.suffix(move |_, _| Tag::primary().small().child(active.to_string()));
		}
		let updates = self.catalog.read(cx).updates();
		if tab == Tab::Catalog && updates > 0 {
			let color = cx.theme().warning;
			item = item.suffix(move |_, _| {
				div()
					.id("catalog-updates")
					.child(crate::widgets::soft_tag(color, updates.to_string()))
					.tooltip(move |window, cx| {
						Tooltip::new(t!("lib.updates_badge", n = updates).into_owned())
							.build(window, cx)
					})
			});
		}
		item
	}

	pub(super) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
		let main = SidebarMenu::new().children([
			self.nav_item(Tab::Headset, cx),
			self.nav_item(Tab::Catalog, cx),
			self.nav_item(Tab::Downloads, cx),
			self.nav_item(Tab::Rename, cx),
		]);
		// Folded, the group still shows the tab that is open.
		let advanced = SidebarMenu::new().children(
			[Tab::Devices, Tab::Inspector, Tab::Console]
				.into_iter()
				.filter(|tab| !self.advanced_folded || self.tab == *tab)
				.map(|tab| self.nav_item(tab, cx))
				.collect::<Vec<_>>(),
		);
		Sidebar::new("sidebar")
			.collapsible(SidebarCollapsible::None)
			.w(px(240.))
			.header(self.device_switcher(cx))
			.child(SidebarGroup::new(t!("nav.main")).child(main))
			.child(
				SidebarGroup::new(t!("nav.advanced"))
					.child(advanced)
					.suffix({
						let shell = cx.entity().downgrade();
						let folded = self.advanced_folded;
						move |_, _| {
							let shell = shell.clone();
							Button::new("fold-advanced")
								.icon(Icon::new(if folded {
									IconName::ChevronRight
								} else {
									IconName::ChevronDown
								}))
								.ghost()
								.xsmall()
								.on_click(move |_, _, cx| {
									let _ = shell.update(cx, |this, cx| {
										this.advanced_folded = !this.advanced_folded;
										cx.notify();
									});
								})
						}
					}),
			)
			.footer(
				SidebarFooter::new().p_0().child(
					Button::new("nav-settings")
						.icon(Icon::from(AppIcon::Settings))
						.label(Tab::Settings.label())
						.ghost()
						.w_full()
						.justify_start()
						.selected(self.tab == Tab::Settings)
						.on_click(cx.listener(|this, _, window, cx| {
							this.set_tab(Tab::Settings, window, cx)
						})),
				),
			)
	}
}

pub(super) fn transport_label(device: &DeviceEntry) -> String {
	if device.is_wireless() {
		format!("Wi-Fi {}", device.serial)
	} else {
		"USB".to_owned()
	}
}

pub(super) fn state_hint(state: &DeviceState) -> String {
	match state {
		DeviceState::Unauthorized | DeviceState::Authorizing => t!("dev.unauthorized").into_owned(),
		_ => t!("dev.offline").into_owned(),
	}
}
