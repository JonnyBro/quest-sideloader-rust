//! Things done to a headset beyond installing: managing apps, rebooting,
//! wireless ADB, screenshots, and reading what takes up space.

use rust_i18n::t;
use std::collections::HashMap;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::AdbError;
use super::device::Device;
use super::sync::timed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootMode {
	Normal,
	PowerOff,
	Recovery,
	Bootloader,
}

/// Space an app takes, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppUsage {
	/// `base.apk` only: split APKs and unpacked native libraries aren't
	/// readable without root.
	pub apk: u64,
	pub obb: u64,
	pub data: u64,
}

impl AppUsage {
	pub fn total(&self) -> u64 {
		self.apk + self.obb + self.data
	}
}

/// Prints `<bytes> <package>` for each third-party app's `base.apk`. One
/// command whatever the number of apps; `stat` reads the file through
/// directories the shell user may enter but not list.
const APK_SIZES: &str = "pm list packages -f -3 | while IFS= read -r line; do \
	p=${line#package:}; echo \"$(stat -c %s \"${p%=*}\" 2>/dev/null) ${p##*=}\"; done";

/// More than [`super::DeviceInfo`]: what the headset tab shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceDetails {
	pub codename: String,
	pub build: String,
	pub uptime: Option<Duration>,
	/// Degrees Celsius.
	pub battery_temperature: Option<f32>,
	pub memory_total: Option<u64>,
	pub memory_available: Option<u64>,
	pub wifi_ip: Option<String>,
	/// Port adbd listens on over the network, when wireless ADB is on.
	pub adb_tcp_port: Option<u16>,
	pub refresh_rate: Option<String>,
	pub cpu_level: Option<String>,
	pub gpu_level: Option<String>,
	/// USB is in file transfer mode; `None` when the firmware doesn't say.
	pub mtp: Option<bool>,
	/// Horizon OS release, like `v209`, from the build branch.
	pub horizon_version: Option<String>,
	/// Build number the headset's settings show next to the version.
	pub build_number: Option<String>,
	/// `YYYY-MM-DD`.
	pub security_patch: Option<String>,
	/// Panel resolution, like `3664x1920`.
	pub display: Option<String>,
	/// Degrees Celsius.
	pub soc_temperature: Option<f32>,
	pub power: Option<Power>,
	/// `None` when Wi-Fi isn't connected.
	pub wifi: Option<WifiLink>,
	pub controllers: Vec<Controller>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
	Ac,
	Usb,
	Wireless,
	Dock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryHealth {
	Good,
	Overheat,
	Dead,
	OverVoltage,
	Cold,
	Other,
}

/// From `dumpsys battery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power {
	/// `None` when running on battery.
	pub source: Option<PowerSource>,
	/// The charger gives too little current to charge at full speed.
	pub weak_charger: bool,
	pub voltage_mv: Option<u32>,
	pub health: Option<BatteryHealth>,
}

/// The connected network, from `dumpsys wifi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiLink {
	pub ssid: String,
	/// dBm; closer to 0 is stronger.
	pub rssi: i32,
	pub link_mbps: Option<u32>,
	pub frequency_mhz: Option<u32>,
	/// The Wi-Fi generation, like 6.
	pub standard: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerSide {
	Left,
	Right,
}

/// A paired controller, from `dumpsys OVRRemoteService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
	pub side: ControllerSide,
	/// Percent, as of the last time the controller was on.
	pub battery: Option<u8>,
	pub firmware: Option<String>,
}

/// Performance overrides Quest headsets read from system properties. They
/// last until the next reboot.
/// OBB and data folders of an app that isn't installed any more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
	pub package: String,
	pub obb_bytes: Option<u64>,
	pub data_bytes: Option<u64>,
}

impl Leftover {
	pub fn total(&self) -> u64 {
		self.obb_bytes.unwrap_or(0) + self.data_bytes.unwrap_or(0)
	}
}

const OBB_ROOT: &str = "/sdcard/Android/obb";
const DATA_ROOT: &str = "/sdcard/Android/data";
/// Folders of system apps are never offered for removal, installed or not.
const SYSTEM_PREFIXES: [&str; 5] = [
	"com.oculus.",
	"com.meta.",
	"com.facebook.",
	"com.android.",
	"android.",
];

fn is_removable(name: &str) -> bool {
	crate::sideload::is_package_name(name) && !SYSTEM_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// `du -sk` lines: kilobytes, a tab and the path.
fn parse_du_sizes(text: &str) -> HashMap<String, u64> {
	text.lines()
		.filter_map(|line| {
			let (kb, path) = line.split_once(char::is_whitespace)?;
			Some((
				path.trim().to_owned(),
				kb.trim().parse::<u64>().ok()? * 1024,
			))
		})
		.collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tweak {
	RefreshRate,
	CpuLevel,
	GpuLevel,
}

impl Tweak {
	pub fn property(self) -> &'static str {
		match self {
			Self::RefreshRate => "debug.oculus.refreshRate",
			Self::CpuLevel => "debug.oculus.cpuLevel",
			Self::GpuLevel => "debug.oculus.gpuLevel",
		}
	}

	/// Values the UI offers; an empty string resets to the system default.
	pub fn choices(self) -> &'static [&'static str] {
		match self {
			Self::RefreshRate => &["", "72", "90", "120"],
			Self::CpuLevel | Self::GpuLevel => &["", "0", "1", "2", "3", "4", "5"],
		}
	}
}

fn quote(s: &str) -> String {
	shell_words::quote(s).into_owned()
}

impl Device {
	/// Folders in `Android/obb` and `Android/data` whose app isn't installed,
	/// largest first.
	pub async fn leftovers(&self) -> Result<Vec<Leftover>, AdbError> {
		let installed: std::collections::HashSet<String> =
			super::parse::parse_packages(&self.shell_ok("pm list packages").await?)
				.into_iter()
				.map(|p| p.name)
				.collect();
		let mut found: Vec<Leftover> = Vec::new();
		for root in [OBB_ROOT, DATA_ROOT] {
			let Ok(out) = self.shell(&format!("ls -1 {root}")).await else {
				continue;
			};
			for name in out.stdout.lines().map(str::trim) {
				if !is_removable(name) || installed.contains(name) {
					continue;
				}
				if !found.iter().any(|l| l.package == name) {
					found.push(Leftover {
						package: name.to_owned(),
						obb_bytes: None,
						data_bytes: None,
					});
				}
				let leftover = found.iter_mut().find(|l| l.package == name);
				if let Some(leftover) = leftover {
					if root == OBB_ROOT {
						leftover.obb_bytes = Some(0);
					} else {
						leftover.data_bytes = Some(0);
					}
				}
			}
		}
		if found.is_empty() {
			return Ok(found);
		}
		let paths: Vec<String> = found
			.iter()
			.flat_map(|l| {
				let obb = l.obb_bytes.map(|_| format!("{OBB_ROOT}/{}", l.package));
				let data = l.data_bytes.map(|_| format!("{DATA_ROOT}/{}", l.package));
				obb.into_iter().chain(data)
			})
			.collect();
		let quoted: Vec<String> = paths
			.iter()
			.map(|p| shell_words::quote(p).into_owned())
			.collect();
		let sizes = parse_du_sizes(
			&self
				.shell(&format!("du -sk {}", quoted.join(" ")))
				.await?
				.stdout,
		);
		for leftover in &mut found {
			if leftover.obb_bytes.is_some() {
				leftover.obb_bytes = sizes
					.get(&format!("{OBB_ROOT}/{}", leftover.package))
					.copied();
			}
			if leftover.data_bytes.is_some() {
				leftover.data_bytes = sizes
					.get(&format!("{DATA_ROOT}/{}", leftover.package))
					.copied();
			}
		}
		found.sort_by_key(|l| std::cmp::Reverse(l.total()));
		Ok(found)
	}

	/// Deletes the OBB and data folders of `packages`, refusing any that are
	/// installed or belong to the system.
	pub async fn remove_leftovers(&self, packages: &[String]) -> Result<(), AdbError> {
		let installed: std::collections::HashSet<String> =
			super::parse::parse_packages(&self.shell_ok("pm list packages").await?)
				.into_iter()
				.map(|p| p.name)
				.collect();
		let paths: Vec<String> = packages
			.iter()
			.filter(|p| is_removable(p) && !installed.contains(*p))
			.flat_map(|p| [format!("{OBB_ROOT}/{p}"), format!("{DATA_ROOT}/{p}")])
			.map(|p| shell_words::quote(&p).into_owned())
			.collect();
		if paths.is_empty() {
			return Ok(());
		}
		self.shell_ok(&format!("rm -rf {}", paths.join(" ")))
			.await?;
		Ok(())
	}

	pub async fn launch(&self, package: &str) -> Result<(), AdbError> {
		let out = self
			.shell(&format!(
				"monkey -p {} -c android.intent.category.LAUNCHER 1",
				quote(package)
			))
			.await?;
		if out.stdout.contains("No activities found") || !out.success() {
			return Err(AdbError::Server(
				t!("adb.not_launchable", package = package).into_owned(),
			));
		}
		Ok(())
	}

	pub async fn force_stop(&self, package: &str) -> Result<(), AdbError> {
		self.shell_ok(&format!("am force-stop {}", quote(package)))
			.await
			.map(drop)
	}

	/// `keep_data` keeps the app's private data (`pm uninstall -k`); shared
	/// storage (OBB, `Android/data`) is handled by the caller.
	pub async fn uninstall(&self, package: &str, keep_data: bool) -> Result<(), AdbError> {
		let flag = if keep_data { "-k " } else { "" };
		let out = self
			.shell(&format!("pm uninstall {flag}{}", quote(package)))
			.await?;
		if out.stdout.lines().any(|l| l.trim() == "Success") {
			Ok(())
		} else {
			Err(AdbError::Server(
				t!(
					"adb.uninstall_failed",
					package = package,
					detail = out.stdout.trim()
				)
				.into_owned(),
			))
		}
	}

	/// Deletes a folder on shared storage, e.g. an app's OBB folder.
	pub async fn remove_dir(&self, path: &str) -> Result<(), AdbError> {
		self.shell_ok(&format!("rm -rf {}", quote(path)))
			.await
			.map(drop)
	}

	pub async fn reboot(&self, mode: RebootMode) -> Result<(), AdbError> {
		if mode == RebootMode::PowerOff {
			// The connection drops as the headset goes down; that's success.
			let _ = self.shell("reboot -p").await;
			return Ok(());
		}
		let service = match mode {
			RebootMode::Normal => "reboot:",
			RebootMode::Recovery => "reboot:recovery",
			RebootMode::Bootloader => "reboot:bootloader",
			RebootMode::PowerOff => unreachable!(),
		};
		self.open(service).await.map(drop)
	}

	/// Makes adbd listen on `port` over Wi-Fi. The USB connection resets.
	pub async fn enable_tcpip(&self, port: u16) -> Result<String, AdbError> {
		let mut stream = self.open(&format!("tcpip:{port}")).await?;
		let mut message = String::new();
		let _ = timed(async { Ok(stream.read_to_string(&mut message).await?) }).await;
		Ok(message.trim().to_owned())
	}

	/// The headset's address on Wi-Fi, if it's connected to one.
	pub async fn wifi_ip(&self) -> Result<Option<String>, AdbError> {
		let out = self.shell("ip -f inet addr show wlan0").await?;
		Ok(parse_inet(&out.stdout))
	}

	/// A PNG of what the headset shows.
	pub async fn screenshot(&self) -> Result<Vec<u8>, AdbError> {
		let mut stream = self.open("exec:screencap -p").await?;
		let mut png = Vec::new();
		timed(async { Ok(stream.read_to_end(&mut png).await?) }).await?;
		if !png.starts_with(b"\x89PNG") {
			return Err(AdbError::Server(
				t!(
					"adb.no_screenshot",
					detail = String::from_utf8_lossy(&png[..png.len().min(120)]).trim()
				)
				.into_owned(),
			));
		}
		Ok(png)
	}

	pub async fn set_tweak(&self, tweak: Tweak, value: &str) -> Result<(), AdbError> {
		let value = if value.is_empty() { "\"\"" } else { value };
		if !value.bytes().all(|b| b.is_ascii_digit() || b == b'"') {
			return Err(AdbError::Protocol(format!("invalid value {value}")));
		}
		self.shell_ok(&format!("setprop {} {value}", tweak.property()))
			.await
			.map(drop)
	}

	/// Space each app's APK, OBB and `Android/data` folders take, by package.
	pub async fn app_usage(&self) -> Result<HashMap<String, AppUsage>, AdbError> {
		let (shared, apks) = tokio::join!(
			self.shell("du -sk /sdcard/Android/obb/* /sdcard/Android/data/* 2>/dev/null"),
			self.shell(APK_SIZES),
		);
		let mut usage = parse_du(&shared?.stdout);
		// Sizes without the APK are still worth showing.
		match apks {
			Ok(out) => {
				for (package, bytes) in parse_apk_sizes(&out.stdout) {
					usage.entry(package).or_default().apk = bytes;
				}
			}
			Err(e) => tracing::warn!("could not read APK sizes: {e}"),
		}
		Ok(usage)
	}

	pub async fn details(&self) -> Result<DeviceDetails, AdbError> {
		let props = self.shell_ok("getprop").await?;
		let props = parse_getprop(&props);
		let prop = |name: &str| props.get(name).filter(|v| !v.is_empty()).cloned();
		let (uptime, battery, memory, ip, usb) = tokio::join!(
			self.shell("cat /proc/uptime"),
			self.shell("dumpsys battery"),
			self.shell("cat /proc/meminfo"),
			self.shell("ip -f inet addr show wlan0"),
			self.shell("svc usb getFunctions"),
		);
		// The full Wi-Fi and thermal dumps are long; the device cuts them down.
		let (wifi, thermal, controllers, display) = tokio::join!(
			self.shell("dumpsys wifi | grep -m1 'mWifiInfo SSID'"),
			self.shell("dumpsys thermalservice | grep -m20 'Temperature{'"),
			self.shell("dumpsys OVRRemoteService | grep 'Paired device:'"),
			self.shell("wm size"),
		);
		let battery = battery.ok().map(|b| b.stdout);
		let (memory_total, memory_available) = memory
			.ok()
			.map(|m| parse_meminfo(&m.stdout))
			.unwrap_or_default();
		Ok(DeviceDetails {
			codename: prop("ro.product.device").unwrap_or_default(),
			build: prop("ro.build.display.id")
				.or_else(|| prop("ro.build.version.incremental"))
				.unwrap_or_default(),
			uptime: uptime.ok().and_then(|u| parse_uptime(&u.stdout)),
			battery_temperature: battery.as_deref().and_then(parse_battery_temperature),
			memory_total,
			memory_available,
			wifi_ip: ip.ok().and_then(|i| parse_inet(&i.stdout)),
			adb_tcp_port: prop("service.adb.tcp.port")
				.and_then(|p| p.parse().ok())
				.filter(|&p: &u16| p > 0),
			refresh_rate: prop(Tweak::RefreshRate.property()),
			cpu_level: prop(Tweak::CpuLevel.property()),
			gpu_level: prop(Tweak::GpuLevel.property()),
			mtp: usb
				.ok()
				.filter(|u| u.success())
				.map(|u| u.stdout.contains("mtp") || u.stderr.contains("mtp")),
			horizon_version: prop("ro.build.branch").and_then(|b| parse_horizon_version(&b)),
			build_number: prop("ro.build.version.incremental"),
			security_patch: prop("ro.build.version.security_patch"),
			display: display.ok().and_then(|d| parse_wm_size(&d.stdout)),
			soc_temperature: thermal.ok().and_then(|t| parse_soc_temperature(&t.stdout)),
			power: battery.as_deref().map(parse_power),
			wifi: wifi.ok().and_then(|w| parse_wifi(&w.stdout)),
			controllers: controllers
				.ok()
				.map(|c| parse_controllers(&c.stdout))
				.unwrap_or_default(),
		})
	}

	/// Switches USB between file transfer (MTP) and charging only. adb keeps
	/// working; the USB connection resets while the mode changes.
	pub async fn set_mtp(&self, enabled: bool) -> Result<(), AdbError> {
		let command = if enabled {
			"svc usb setFunctions mtp"
		} else {
			"svc usb setFunctions"
		};
		match self.shell(command).await {
			Ok(out) if !out.success() => Err(AdbError::Server(
				t!(
					"adb.command_failed",
					command = command,
					detail = out.stderr.trim()
				)
				.into_owned(),
			)),
			// Over USB the reset can cut the connection before the reply; that's success.
			_ => Ok(()),
		}
	}
}

/// `[key]: [value]` lines of `getprop`.
fn parse_getprop(text: &str) -> HashMap<String, String> {
	text.lines()
		.filter_map(|line| {
			let (key, value) = line.split_once("]: [")?;
			let key = key.trim().strip_prefix('[')?;
			let value = value.trim().strip_suffix(']')?;
			Some((key.to_owned(), value.to_owned()))
		})
		.collect()
}

/// `du -sk` lines: `<KiB>\t/sdcard/Android/<obb|data>/<package>`.
fn parse_du(text: &str) -> HashMap<String, AppUsage> {
	let mut usage: HashMap<String, AppUsage> = HashMap::new();
	for line in text.lines() {
		let Some((kib, path)) = line.split_once(char::is_whitespace) else {
			continue;
		};
		let Ok(kib) = kib.trim().parse::<u64>() else {
			continue;
		};
		let path = path.trim();
		let (kind, package) = if let Some(p) = path.strip_prefix("/sdcard/Android/obb/") {
			("obb", p)
		} else if let Some(p) = path.strip_prefix("/sdcard/Android/data/") {
			("data", p)
		} else {
			continue;
		};
		let entry = usage
			.entry(package.trim_end_matches('/').to_owned())
			.or_default();
		if kind == "obb" {
			entry.obb = kib * 1024;
		} else {
			entry.data = kib * 1024;
		}
	}
	usage
}

/// `<bytes> <package>` lines; a line without a size (no `stat` output) is skipped.
fn parse_apk_sizes(text: &str) -> Vec<(String, u64)> {
	text.lines()
		.filter_map(|line| {
			let (bytes, package) = line.trim().split_once(' ')?;
			let package = package.trim();
			if package.is_empty() {
				return None;
			}
			Some((package.to_owned(), bytes.parse().ok()?))
		})
		.collect()
}

/// `releases-oculus-14.0-v209` -> `v209`.
fn parse_horizon_version(branch: &str) -> Option<String> {
	let last = branch.rsplit('-').next()?;
	(last.starts_with('v') && last[1..].chars().all(|c| c.is_ascii_digit()) && last.len() > 1)
		.then(|| last.to_owned())
}

/// `Physical size: 3664x1920`; an override line, when there is one, wins.
fn parse_wm_size(text: &str) -> Option<String> {
	text.lines().rev().find_map(|l| {
		let v = l.split_once("size:")?.1.trim();
		(!v.is_empty()).then(|| v.to_owned())
	})
}

/// The SoC sensor from `Temperature{mValue=35.3, mType=3, mName=soc-usr, ...}` lines.
fn parse_soc_temperature(text: &str) -> Option<f32> {
	text.lines().find_map(|line| {
		let name = line.split("mName=").nth(1)?.split([',', '}']).next()?;
		if !name.starts_with("soc") {
			return None;
		}
		line.split("mValue=")
			.nth(1)?
			.split(',')
			.next()?
			.trim()
			.parse()
			.ok()
	})
}

/// Power source, charger strength, voltage and health from `dumpsys battery`.
fn parse_power(text: &str) -> Power {
	let value = |key: &str| {
		text.lines().find_map(|line| {
			let (k, v) = line.trim().split_once(':')?;
			(k.trim() == key).then(|| v.trim().to_owned())
		})
	};
	let on = |key: &str| value(key).as_deref() == Some("true");
	let source = if on("AC powered") {
		Some(PowerSource::Ac)
	} else if on("USB powered") {
		Some(PowerSource::Usb)
	} else if on("Wireless powered") {
		Some(PowerSource::Wireless)
	} else if on("Dock powered") {
		Some(PowerSource::Dock)
	} else {
		None
	};
	// BatteryManager.BATTERY_HEALTH_* values.
	let health = value("health")
		.and_then(|h| h.parse::<u8>().ok())
		.map(|h| match h {
			2 => BatteryHealth::Good,
			3 => BatteryHealth::Overheat,
			4 => BatteryHealth::Dead,
			5 => BatteryHealth::OverVoltage,
			7 => BatteryHealth::Cold,
			_ => BatteryHealth::Other,
		});
	Power {
		source,
		weak_charger: source.is_some() && on("Weak Charger"),
		voltage_mv: value("voltage").and_then(|v| v.parse().ok()),
		health,
	}
}

/// The `mWifiInfo SSID: "name", ..., RSSI: -21, Link speed: 2401Mbps, ...` line.
fn parse_wifi(text: &str) -> Option<WifiLink> {
	let line = text.lines().find(|l| l.contains("SSID:"))?;
	let field = |key: &str| {
		let rest = line.split(&format!("{key}: ")).nth(1)?;
		Some(rest.split(", ").next()?.trim().to_owned())
	};
	if field("Supplicant state").as_deref() != Some("COMPLETED") {
		return None;
	}
	let number = |key: &str| -> Option<i64> {
		let text = field(key)?;
		let digits: String = text
			.chars()
			.take_while(|c| *c == '-' || c.is_ascii_digit())
			.collect();
		digits.parse().ok()
	};
	Some(WifiLink {
		ssid: field("SSID")?.trim_matches('"').to_owned(),
		rssi: number("RSSI")? as i32,
		link_mbps: number("Link speed").and_then(|v| u32::try_from(v).ok()),
		frequency_mhz: number("Frequency").and_then(|v| u32::try_from(v).ok()),
		standard: number("Wi-Fi standard").and_then(|v| u8::try_from(v).ok()),
	})
}

/// `Paired device: id, Type:  Right, Model: ..., Firmware: 209.29.41, ..., Battery:  70%, ...`
fn parse_controllers(text: &str) -> Vec<Controller> {
	let mut controllers: Vec<Controller> = Vec::new();
	for line in text.lines().filter(|l| l.contains("Paired device:")) {
		let field = |key: &str| {
			let rest = line.split(&format!("{key}:")).nth(1)?;
			Some(rest.split(',').next()?.trim().to_owned())
		};
		let side = match field("Type").as_deref() {
			Some("Left") => ControllerSide::Left,
			Some("Right") => ControllerSide::Right,
			_ => continue,
		};
		if controllers.iter().any(|c| c.side == side) {
			continue;
		}
		controllers.push(Controller {
			side,
			battery: field("Battery").and_then(|b| b.trim_end_matches('%').trim().parse().ok()),
			firmware: field("Firmware").filter(|f| !f.is_empty()),
		});
	}
	controllers.sort_by_key(|c| c.side != ControllerSide::Left);
	controllers
}

fn parse_uptime(text: &str) -> Option<Duration> {
	let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
	Some(Duration::from_secs_f64(secs.max(0.0)))
}

/// `temperature: 312` in `dumpsys battery` is tenths of a degree.
fn parse_battery_temperature(text: &str) -> Option<f32> {
	text.lines().find_map(|line| {
		let value = line.trim().strip_prefix("temperature:")?;
		Some(value.trim().parse::<f32>().ok()? / 10.0)
	})
}

fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
	let field = |name: &str| {
		text.lines().find_map(|line| {
			let rest = line.strip_prefix(name)?.trim_start_matches(':').trim();
			let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
			Some(kib * 1024)
		})
	};
	(field("MemTotal"), field("MemAvailable"))
}

fn parse_inet(text: &str) -> Option<String> {
	text.lines().find_map(|line| {
		let rest = line.trim().strip_prefix("inet ")?;
		Some(rest.split(['/', ' ']).next()?.to_owned())
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn mtp_switches_usb_functions() {
		let fake = super::super::testing::FakeAdb::start().await;
		let device =
			super::super::AdbServer::with_port(Some("/no/adb".into()), fake.port).device("SER1");
		device.set_mtp(true).await.unwrap();
		device.set_mtp(false).await.unwrap();
		assert_eq!(
			fake.recorded.lock().unwrap().shell,
			["svc usb setFunctions mtp", "svc usb setFunctions"]
		);
	}

	#[test]
	fn getprop_lines() {
		let props = parse_getprop(
			"[ro.product.device]: [panther]\n[service.adb.tcp.port]: [5555]\n[empty]: []\n",
		);
		assert_eq!(props["ro.product.device"], "panther");
		assert_eq!(props["service.adb.tcp.port"], "5555");
		assert_eq!(props["empty"], "");
	}

	#[test]
	fn du_output() {
		let text = "3736504\t/sdcard/Android/obb/com.Armature.VR4\n\
			12\t/sdcard/Android/data/com.Armature.VR4\n\
			4\t/sdcard/Android/data/com.discord\n\
			junk line\n";
		let usage = parse_du(text);
		assert_eq!(
			usage["com.Armature.VR4"],
			AppUsage {
				apk: 0,
				obb: 3_736_504 * 1024,
				data: 12 * 1024
			}
		);
		assert_eq!(usage["com.discord"].obb, 0);
	}

	#[test]
	fn apk_sizes() {
		let text = "123456789 com.beatgames.beatsaber
			 com.broken
			28672 moe.shizuku.privileged.api
			junk
";
		assert_eq!(
			parse_apk_sizes(text),
			[
				("com.beatgames.beatsaber".to_owned(), 123_456_789),
				("moe.shizuku.privileged.api".to_owned(), 28_672),
			]
		);
	}

	#[test]
	fn headset_build_and_display() {
		assert_eq!(
			parse_horizon_version("releases-oculus-14.0-v209").as_deref(),
			Some("v209")
		);
		assert_eq!(parse_horizon_version("main"), None);
		assert_eq!(parse_horizon_version("x-v"), None);
		assert_eq!(
			parse_wm_size("Physical size: 3664x1920\n").as_deref(),
			Some("3664x1920")
		);
		assert_eq!(
			parse_wm_size("Physical size: 3664x1920\nOverride size: 1832x960\n").as_deref(),
			Some("1832x960")
		);
	}

	#[test]
	fn soc_temperature() {
		let text = "\tTemperature{mValue=33.678, mType=9, mName=nspss-0, mStatus=0}\n\
			\tTemperature{mValue=35.328003, mType=3, mName=soc-usr, mStatus=0}\n\
			\tTemperature{mValue=30.015001, mType=3, mName=pcb1-usr, mStatus=0}\n";
		assert_eq!(parse_soc_temperature(text), Some(35.328003));
		assert_eq!(parse_soc_temperature("nothing"), None);
	}

	#[test]
	fn power_from_battery_dump() {
		let text = "Current Battery Service state:\n  AC powered: false\n  USB powered: true\n\
			  Wireless powered: false\n  Dock powered: false\n  Weak Charger: true\n\
			  status: 5\n  health: 2\n  level: 100\n  voltage: 4417\n  temperature: 270\n";
		assert_eq!(
			parse_power(text),
			Power {
				source: Some(PowerSource::Usb),
				weak_charger: true,
				voltage_mv: Some(4417),
				health: Some(BatteryHealth::Good),
			}
		);
		let unplugged = "  AC powered: false\n  USB powered: false\n  Weak Charger: true\n";
		assert_eq!(parse_power(unplugged).source, None);
		assert!(!parse_power(unplugged).weak_charger);
	}

	#[test]
	fn wifi_link() {
		let text = "mWifiInfo SSID: \"Interra 6 5G\", BSSID: 52:ff:20:8c:53:b7, MAC: 78:c4:fa:45:c4:dc, \
			IP: /192.168.1.102, Security type: 4, Supplicant state: COMPLETED, Wi-Fi standard: 6, \
			RSSI: -19, Link speed: 2401Mbps, Tx Link speed: 2401Mbps, Frequency: 5180MHz, Net ID: 0";
		assert_eq!(
			parse_wifi(text),
			Some(WifiLink {
				ssid: "Interra 6 5G".into(),
				rssi: -19,
				link_mbps: Some(2401),
				frequency_mhz: Some(5180),
				standard: Some(6),
			})
		);
		let off = "mWifiInfo SSID: <unknown ssid>, Supplicant state: DISCONNECTED, RSSI: -127";
		assert_eq!(parse_wifi(off), None);
	}

	#[test]
	fn controllers() {
		let text = "  Paired device: 8141b2e33faacd2e, Type:  Right, Model: RUBYPRQ, HardwareRev: 0x9a, \
			Firmware: 209.29.41, ImuModel: ICM42686, Battery:  70%, isAttached: 0\n\
			  Paired device: e7ec8b9b83a6cb2d, Type:   Left, Model: RUBYPRQ, HardwareRev: 0x9a, \
			Firmware: 209.29.41, ImuModel: ICM42686, Battery:  60%, isAttached: 0\n";
		assert_eq!(
			parse_controllers(text),
			[
				Controller {
					side: ControllerSide::Left,
					battery: Some(60),
					firmware: Some("209.29.41".into()),
				},
				Controller {
					side: ControllerSide::Right,
					battery: Some(70),
					firmware: Some("209.29.41".into()),
				},
			]
		);
	}

	#[test]
	fn leftover_helpers() {
		let sizes = parse_du_sizes(
			"2595824\t/sdcard/Android/obb/com.rustltd.h3vr2\n12\t/sdcard/Android/data/com.x\n",
		);
		assert_eq!(
			sizes.get("/sdcard/Android/obb/com.rustltd.h3vr2"),
			Some(&(2595824 * 1024))
		);
		assert_eq!(sizes.get("/sdcard/Android/data/com.x"), Some(&(12 * 1024)));
		assert!(is_removable("com.rustltd.h3vr2"));
		assert!(!is_removable("com.oculus.browser"));
		assert!(!is_removable("com.meta.curio.toybox"));
		assert!(!is_removable(".nomedia"));
		assert!(!is_removable("../etc"));
	}

	#[test]
	fn small_parsers() {
		assert_eq!(
			parse_uptime("12345.67 5432.10\n"),
			Some(Duration::from_secs_f64(12345.67))
		);
		assert_eq!(
			parse_battery_temperature("  level: 76\n  temperature: 312\n"),
			Some(31.2)
		);
		assert_eq!(
			parse_meminfo(
				"MemTotal:        7753108 kB\nMemFree: 1 kB\nMemAvailable:    3170000 kB\n"
			),
			(Some(7_753_108 * 1024), Some(3_170_000 * 1024))
		);
		assert_eq!(
			parse_inet(
				"47: wlan0: <UP>\n    inet 192.168.1.50/24 brd 192.168.1.255 scope global wlan0\n"
			),
			Some("192.168.1.50".into())
		);
		assert_eq!(parse_inet("Device \"wlan0\" does not exist."), None);
	}
}
