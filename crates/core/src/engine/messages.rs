use super::*;

#[derive(Debug)]
pub enum Command {
	SaveSettings(Settings),
	/// Read the details and per-app storage the headset tab shows.
	RefreshHeadset(String),
	/// Answered with [`Event::Leftovers`].
	FindLeftovers(String),
	/// Delete the OBB and data folders of uninstalled apps, then look again.
	RemoveLeftovers {
		serial: String,
		packages: Vec<String>,
	},
	LaunchApp {
		serial: String,
		package: String,
	},
	StopApp {
		serial: String,
		package: String,
	},
	Uninstall {
		serial: String,
		app: AppInfo,
		back_up: bool,
		remove_obb: bool,
	},
	BackUpSaves {
		serial: String,
		app: AppInfo,
	},
	RestoreBackup {
		serial: String,
		backup: crate::saves::Backup,
		/// Where the saves go: the backed-up app, or another one such as its
		/// renamed copy.
		package: String,
	},
	DeleteBackup(crate::saves::Backup),
	ListBackups,
	/// Answered with [`Event::AppUpdate`] when GitHub has a newer release.
	CheckAppUpdate,
	/// Answered with [`Event::Downloads`].
	ListDownloads,
	/// Delete a release folder from the downloads folder by name, or what an
	/// unfinished download of it left.
	DeleteDownload {
		name: String,
		incomplete: bool,
	},
	/// After a signature mismatch: back up saves, uninstall, install again, restore.
	ReinstallKeepingSaves {
		serial: String,
		app: AppInfo,
		source: PathBuf,
		/// Delete `source` once the new build is installed; see
		/// [`SignatureConflict::remove_after`](task::SignatureConflict).
		remove_source: bool,
	},
	Reboot {
		serial: String,
		mode: crate::adb::RebootMode,
	},
	/// Turn on wireless ADB and connect to the headset over Wi-Fi.
	EnableWireless(String),
	Screenshot(String),
	/// Answered with [`Event::DirListing`].
	ListDir {
		serial: String,
		path: String,
	},
	/// Copy headset files or folders into the local folder `dest`.
	PullFiles {
		serial: String,
		paths: Vec<String>,
		dest: PathBuf,
	},
	/// Copy local files or folders into the headset folder `dir`.
	PushFiles {
		serial: String,
		local: Vec<PathBuf>,
		dir: String,
	},
	/// Create folder `name` in the headset folder `dir`, then list `dir` again.
	MakeDir {
		serial: String,
		dir: String,
		name: String,
	},
	/// Rename `from` to `to` inside the headset folder `dir`, then list it again.
	RenameFile {
		serial: String,
		dir: String,
		from: String,
		to: String,
	},
	/// Copy (or move) headset files or folders into the headset folder `dir`.
	PasteFiles {
		serial: String,
		paths: Vec<String>,
		dir: String,
		copy: bool,
	},
	/// Delete headset files or folders, then list `dir` again.
	DeleteFiles {
		serial: String,
		paths: Vec<String>,
		dir: String,
	},
	/// Show the headset's screen in a scrcpy window.
	MirrorScreen {
		serial: String,
		eye: crate::scrcpy::Eye,
	},
	/// USB file transfer (MTP) on or off, so the headset shows up in the
	/// computer's file manager.
	SetMtp {
		serial: String,
		enabled: bool,
	},
	SetAppSafety {
		serial: String,
		enabled: bool,
	},
	SetTweak {
		serial: String,
		tweak: crate::adb::Tweak,
		value: String,
	},
	RefreshDevice(String),
	/// `host` or `host:port`.
	ConnectWireless(String),
	/// Answered with [`Event::MdnsServices`].
	DiscoverWireless,
	/// Like [`Command::ConnectWireless`], for the headset of the last run: the
	/// headset may be off or have another address, so failing isn't an error.
	Reconnect(String),
	DisconnectWireless(String),
	/// Install an APK, a release folder or a ZIP on the headset.
	Install {
		serial: String,
		path: PathBuf,
	},
	ReviewScript {
		task: TaskId,
		decision: ReviewDecision,
	},
	CancelTask(TaskId),
	/// Download a release from the active mirror; with `serial`, install it
	/// on that headset afterwards.
	Download {
		game: Game,
		serial: Option<String>,
	},
	/// Upload an app from the headset to the active mirror. Only send this
	/// after the user agreed to share that app.
	Contribute {
		serial: String,
		package: String,
		name: String,
	},
	/// Stop offering this package for upload.
	IgnoreContribution(String),
	/// Moves a queued download by `delta` places (negative is earlier).
	MoveDownload {
		task: TaskId,
		delta: isize,
	},
	/// Download the active mirror's metadata again.
	SyncCatalog,
	/// Download adb from Google into the data folder and use it. Only send
	/// this after the user agreed to the Android SDK terms.
	InstallAdb,
	/// Download rclone from rclone.org into the data folder and use it.
	InstallRclone,
	/// Run a command typed in the console; output comes back as
	/// [`Event::Shell`] with the same `id`.
	Shell {
		id: u64,
		serial: String,
		command: String,
	},
	/// Stop a console command; the headset ends it when the connection closes.
	StopShell(u64),
	/// Work out what renaming a release would change; answered with
	/// [`Event::RenameAnalysis`] carrying the same `id`.
	AnalyzeRename {
		id: u64,
		path: PathBuf,
	},
	/// Read everything the Inspector tab shows about an APK or a release
	/// folder; answered with [`Event::Inspection`] carrying the same `id`.
	Inspect {
		id: u64,
		path: PathBuf,
	},
	/// Build a renamed copy of a release into the downloads folder.
	BuildCopy {
		source: PathBuf,
		new_package: String,
		/// The user's replacement for the Platform SDK loader.
		loader: Option<PathBuf>,
		debuggable: bool,
		title: String,
	},
}

/// An app on the headset, as the UI knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
	pub package: String,
	pub name: String,
	pub version_code: Option<u64>,
}

impl From<AppInfo> for headset::AppRef {
	fn from(app: AppInfo) -> Self {
		Self {
			package: app.package,
			name: app.name,
			version_code: app.version_code,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbStatus {
	Starting,
	Running {
		version: u32,
	},
	Unavailable(String),
	/// The adb binary isn't there; it can be installed with [`Command::InstallAdb`].
	Missing(String),
}

#[derive(Debug)]
pub enum Event {
	SettingsSaved(Settings),
	SettingsSaveFailed(String),
	AdbStatus(AdbStatus),
	DevicesChanged(Vec<DeviceEntry>),
	DeviceInfo {
		serial: String,
		info: std::result::Result<DeviceInfo, String>,
	},
	TaskUpdated(TaskSnapshot),
	/// The active mirror's catalog; `None` when there is no mirror or it was never synced.
	CatalogLoaded(Option<Arc<Catalog>>),
	CatalogStatus(CatalogStatus),
	/// install.txt has lines that need the user's decision; answer with
	/// [`Command::ReviewScript`].
	ScriptReview {
		task: TaskId,
		title: String,
		analysis: ScriptAnalysis,
	},
	HeadsetDetails {
		serial: String,
		details: std::result::Result<
			(
				crate::adb::DeviceDetails,
				HashMap<String, crate::adb::AppUsage>,
			),
			String,
		>,
	},
	Backups(Vec<crate::saves::Backup>),
	AppUpdate(crate::app_update::AppRelease),
	Leftovers {
		serial: String,
		result: std::result::Result<Vec<crate::adb::Leftover>, String>,
	},
	/// Headsets announci0ng wireless adb on the network.
	MdnsServices(std::result::Result<Vec<crate::adb::MdnsService>, String>),
	DirListing {
		serial: String,
		path: String,
		result: std::result::Result<Vec<crate::adb::RemoteEntry>, String>,
	},
	/// Something changed the stored backups; ask again with [`Command::ListBackups`].
	BackupsChanged,
	/// A sync failed because there is no rclone; it can be installed with
	/// [`Command::InstallRclone`].
	RcloneMissing,
	/// Release folders in the downloads folder.
	Downloads(Vec<crate::downloaded::DownloadedRelease>),
	Shell {
		id: u64,
		output: ShellEvent,
	},
	RenameAnalysis {
		id: u64,
		result: std::result::Result<crate::rename::Analysis, String>,
	},
	Inspection {
		id: u64,
		result: std::result::Result<Box<crate::rename::Inspection>, String>,
	},
	Notice(String),
	Error(String),
}
