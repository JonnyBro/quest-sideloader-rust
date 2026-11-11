//! Parsers for text that the ADB server and device tools print.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
	Online,
	Offline,
	Unauthorized,
	Authorizing,
	Connecting,
	/// Linux without udev rules for the device.
	NoPermissions,
	Other(String),
}

impl DeviceState {
	fn parse(s: &str) -> Self {
		match s {
			"device" => Self::Online,
			"offline" => Self::Offline,
			"unauthorized" => Self::Unauthorized,
			"authorizing" => Self::Authorizing,
			"connecting" => Self::Connecting,
			other => Self::Other(other.to_owned()),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
	pub serial: String,
	pub state: DeviceState,
	pub model: Option<String>,
	pub product: Option<String>,
	pub transport_id: Option<u64>,
}

impl DeviceEntry {
	pub fn is_wireless(&self) -> bool {
		self.serial.contains(':') || self.serial.starts_with("adb-")
	}

	pub fn display_name(&self) -> String {
		match &self.model {
			Some(model) => model.replace('_', " "),
			None => self.serial.clone(),
		}
	}
}

/// Parses `host:devices-l` output, e.g.
/// `1WMHH000000000 device usb:1-4 product:eureka model:Quest_3 device:eureka transport_id:2`.
pub fn parse_devices(text: &str) -> Vec<DeviceEntry> {
	text.lines().filter_map(parse_device_line).collect()
}

fn parse_device_line(line: &str) -> Option<DeviceEntry> {
	let line = line.trim();
	let (serial, rest) = line.split_once(char::is_whitespace)?;
	let rest = rest.trim_start();

	// "no permissions (...); see [url]" contains spaces, so it can't be split by whitespace.
	let (state, props) = if let Some(after) = rest.strip_prefix("no permissions") {
		let props = after.rfind(']').map_or(after, |i| &after[i + 1..]);
		(DeviceState::NoPermissions, props)
	} else {
		let (state, props) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
		(DeviceState::parse(state), props)
	};

	let mut entry = DeviceEntry {
		serial: serial.to_owned(),
		state,
		model: None,
		product: None,
		transport_id: None,
	};
	for prop in props.split_whitespace() {
		match prop.split_once(':') {
			Some(("model", v)) => entry.model = Some(v.to_owned()),
			Some(("product", v)) => entry.product = Some(v.to_owned()),
			Some(("transport_id", v)) => entry.transport_id = v.parse().ok(),
			_ => {}
		}
	}
	Some(entry)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
	pub level: u8,
	pub charging: bool,
}

/// Parses `dumpsys battery`.
pub fn parse_battery(text: &str) -> Option<Battery> {
	const STATUS_CHARGING: u8 = 2;
	let mut level = None;
	let mut status = None;
	for line in text.lines() {
		let Some((key, value)) = line.trim().split_once(':') else {
			continue;
		};
		match key.trim() {
			"level" => level = value.trim().parse::<u8>().ok(),
			"status" => status = value.trim().parse::<u8>().ok(),
			_ => {}
		}
	}
	Some(Battery {
		level: level?.min(100),
		charging: status == Some(STATUS_CHARGING),
	})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storage {
	pub total_bytes: u64,
	pub free_bytes: u64,
}

/// Parses `df -k <path>`: the last line has 1K-blocks in column 2 and available in column 4.
pub fn parse_df(text: &str) -> Option<Storage> {
	let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
	let cols: Vec<&str> = line.split_whitespace().collect();
	let total = cols.get(1)?.parse::<u64>().ok()?;
	let free = cols.get(3)?.parse::<u64>().ok()?;
	Some(Storage {
		total_bytes: total * 1024,
		free_bytes: free * 1024,
	})
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
	pub name: String,
	pub version_code: Option<u64>,
}

/// Parses `pm list packages --show-versioncode`: `package:com.foo versionCode:123`.
pub fn parse_packages(text: &str) -> Vec<Package> {
	let mut packages: Vec<Package> = text
		.lines()
		.filter_map(|line| {
			let rest = line.trim().strip_prefix("package:")?;
			let mut parts = rest.split_whitespace();
			let name = parts.next()?.to_owned();
			let version_code = parts
				.find_map(|p| p.strip_prefix("versionCode:"))
				.and_then(|v| v.parse().ok());
			Some(Package { name, version_code })
		})
		.collect();
	packages.sort_by_cached_key(|p| p.name.to_lowercase());
	packages
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
	pub model: String,
	pub android_version: String,
	pub battery: Option<Battery>,
	pub storage: Option<Storage>,
	pub packages: Vec<Package>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn devices_long() {
		let text = "1WMHH000000000         device usb:1-4 product:eureka model:Quest_3 device:eureka transport_id:2\n\
			192.168.1.50:5555      device product:eureka model:Quest_3 device:eureka transport_id:5\n\
			2G0YC1ZF000000         unauthorized usb:3-1 transport_id:7\n";
		let devices = parse_devices(text);
		assert_eq!(devices.len(), 3);
		assert_eq!(devices[0].serial, "1WMHH000000000");
		assert_eq!(devices[0].state, DeviceState::Online);
		assert_eq!(devices[0].model.as_deref(), Some("Quest_3"));
		assert_eq!(devices[0].display_name(), "Quest 3");
		assert_eq!(devices[0].transport_id, Some(2));
		assert!(!devices[0].is_wireless());
		assert!(devices[1].is_wireless());
		assert_eq!(devices[2].state, DeviceState::Unauthorized);
		assert_eq!(devices[2].display_name(), "2G0YC1ZF000000");
	}

	#[test]
	fn device_without_permissions() {
		let text = "1WMHH000000000 no permissions (missing udev rules? user is in the plugdev group); \
			see [http://developer.android.com/tools/device.html] usb:1-4 transport_id:3";
		let devices = parse_devices(text);
		assert_eq!(devices[0].state, DeviceState::NoPermissions);
		assert_eq!(devices[0].transport_id, Some(3));
	}

	#[test]
	fn empty_device_list() {
		assert!(parse_devices("").is_empty());
		assert!(parse_devices("\n").is_empty());
	}

	#[test]
	fn battery() {
		let text = "Current Battery Service state:\n  AC powered: false\n  USB powered: true\n  status: 2\n  health: 2\n  level: 87\n  scale: 100\n";
		assert_eq!(
			parse_battery(text),
			Some(Battery {
				level: 87,
				charging: true
			})
		);
		assert_eq!(parse_battery("nothing here"), None);
	}

	#[test]
	fn df() {
		let text = "Filesystem     1K-blocks     Used Available Use% Mounted on\n\
			/dev/fuse      116298364 40123456  76174908  35% /storage/emulated\n";
		assert_eq!(
			parse_df(text),
			Some(Storage {
				total_bytes: 116_298_364 * 1024,
				free_bytes: 76_174_908 * 1024,
			})
		);
		assert_eq!(parse_df("df: /sdcard: No such file"), None);
	}

	#[test]
	fn packages() {
		let text = "package:com.b.game versionCode:42\npackage:com.a.app versionCode:7\npackage:com.C.nover\ngarbage\n";
		assert_eq!(
			parse_packages(text),
			vec![
				Package {
					name: "com.a.app".into(),
					version_code: Some(7)
				},
				Package {
					name: "com.b.game".into(),
					version_code: Some(42)
				},
				Package {
					name: "com.C.nover".into(),
					version_code: None
				},
			]
		);
	}
}
