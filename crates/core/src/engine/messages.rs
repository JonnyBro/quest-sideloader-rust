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
		/// The backed-up app, or another one such as its renamed copy.
		package: String,
	},
	DeleteBackup(crate::saves::Backup),
	ListBackups,
	/// Answered with [`Event::AppUpdate`] when GitHub has a newer release.
	CheckAppUpdate,
	/// Answered with [`Event::Downloads`].
	ListDownloads,
	/// Or what an unfinished download of it left.
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
		streaming: bool,
	},
	RetryWithoutStreaming {
		serial: String,
		source: PathBuf,
		remove_source: bool,
	},
	Reboot {
		serial: String,
		mode: crate::adb::RebootMode,
	},
	EnableWireless(String),
	Screenshot(String),
	/// Answered with [`Event::DirListing`].
	ListDir {
		serial: String,
		path: String,
	},
	PullFiles {
		serial: String,
		paths: Vec<String>,
		dest: PathBuf,
	},
	PushFiles {
		serial: String,
		local: Vec<PathBuf>,
		dir: String,
	},
	/// Then lists `dir` again.
	MakeDir {
		serial: String,
		dir: String,
		name: String,
	},
	/// Then lists `dir` again.
	RenameFile {
		serial: String,
		dir: String,
		from: String,
		to: String,
	},
	PasteFiles {
		serial: String,
		paths: Vec<String>,
		dir: String,
		copy: bool,
	},
	/// Then lists `dir` again.
	DeleteFiles {
		serial: String,
		paths: Vec<String>,
		dir: String,
	},
	MirrorScreen {
		serial: String,
		eye: crate::scrcpy::Eye,
	},
	/// So the headset shows up in the computer's file manager.
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
	/// Failing isn't an error: the headset may be off or have another address.
	Reconnect(String),
	DisconnectWireless(String),
	Install {
		serial: String,
		path: PathBuf,
		streaming: bool,
	},
	ReviewScript {
		task: TaskId,
		decision: ReviewDecision,
	},
	CancelTask(TaskId),
	/// With `serial`, installs it on that headset afterwards.
	Download {
		game: Game,
		serial: Option<String>,
		streaming: bool,
	},
	/// Only send this after the user agreed to share the app.
	Contribute {
		serial: String,
		package: String,
		name: String,
	},
	IgnoreContribution(String),
	HoldUpdates {
		package: String,
		hold: bool,
	},
	/// Star (`on`) or unstar a package in the catalog.
	SetFavorite {
		package: String,
		on: bool,
	},
	/// Hide (`on`) or show again a package in the catalog.
	SetHidden {
		package: String,
		on: bool,
	},
	/// Negative is earlier.
	MoveDownload {
		task: TaskId,
		delta: isize,
	},
	SyncCatalog,
	/// Only send this after the user agreed to the Android SDK terms.
	InstallAdb,
	InstallRclone,
	/// Output comes back as [`Event::Shell`] with the same `id`.
	Shell {
		id: u64,
		serial: String,
		command: String,
	},
	/// The headset ends it when the connection closes.
	StopShell(u64),
	/// Answered with [`Event::RenameAnalysis`] carrying the same `id`.
	AnalyzeRename {
		id: u64,
		path: PathBuf,
	},
	/// Answered with [`Event::Inspection`] carrying the same `id`.
	Inspect {
		id: u64,
		path: PathBuf,
	},
	/// Into the downloads folder.
	BuildCopy {
		source: PathBuf,
		new_package: String,
		/// The user's replacement for the Platform SDK loader.
		loader: Option<PathBuf>,
		debuggable: bool,
		title: String,
	},
}

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
	/// `None` when there is no mirror or it was never synced.
	CatalogLoaded(Option<Arc<Catalog>>),
	CatalogStatus(CatalogStatus),
	/// Sent whenever it changes.
	MirrorHealth(crate::catalog::MirrorHealth),
	/// Answer with [`Command::ReviewScript`].
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
	/// Headsets announcing wireless adb on the network.
	MdnsServices(std::result::Result<Vec<crate::adb::MdnsService>, String>),
	DirListing {
		serial: String,
		path: String,
		result: std::result::Result<Vec<crate::adb::RemoteEntry>, String>,
	},
	/// Ask again with [`Command::ListBackups`].
	BackupsChanged,
	/// rclone can be installed with [`Command::InstallRclone`].
	RcloneMissing,
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
