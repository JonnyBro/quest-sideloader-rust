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
}

/// Performance overrides Quest headsets read from system properties. They
/// last until the next reboot.
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
			battery_temperature: battery
				.ok()
				.and_then(|b| parse_battery_temperature(&b.stdout)),
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
