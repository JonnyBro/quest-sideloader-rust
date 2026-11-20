# Quest Sideloader

[![CI](https://github.com/JonnyBro/quest-sideloader-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/JonnyBro/quest-sideloader-rust/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/JonnyBro/quest-sideloader-rust)](https://github.com/JonnyBro/quest-sideloader-rust/releases/latest)

Desktop sideloader for Meta Quest headsets, written in Rust with egui.

## Layout

- `crates/core` has everything that is not UI: settings, the background engine,
  an async client for the ADB server protocol, the mirror catalog, downloads,
  7z extraction and installation.
- `crates/app` is the egui desktop application. It sends commands to the engine
  and draws the events it gets back.

## Headset tab

The Headset tab shows the connected headset's build, battery temperature,
memory, Wi-Fi address and wireless ADB state. Each installed app can be started,
stopped, backed up or uninstalled; uninstalling can back up the saves first and
remove the app's OBB files.

A save backup is a copy of the app's `Android/data` folder. When the app allows
`run-as` (debuggable builds), its private data in `/data/data` is copied too,
and so are files in `Android/data` that only the app itself may read. Without
`run-as` such files are left out, and the backup task says so; when one of
them looks like a save, the warning says that the game's saves aren't in the
backup. Caches, including Unreal's shader cache, are skipped. Backups are kept under the data directory and can be
deleted, or restored if the app is installed. A backup can also be restored
into another installed app, for example a renamed copy of the game; file
names that carry the original package, such as Unity's
`<package>.v2.playerprefs.xml`, get the new package on the way.

The tab can also turn on wireless ADB in one click, switch USB to file
transfer (MTP) so the headset opens in the computer's file manager, take
screenshots (saved to your Pictures folder), restart or power off the headset, and set the refresh
rate and CPU and GPU levels until the next restart.

If an update fails because the installed app has a different signature, the
failed task offers to reinstall it keeping the saves: back up, uninstall,
install, restore.

## Downloads and Tasks tabs

Downloads lists the release folders in the downloads folder with their size
and date. Releases stay there after installing when "Keep downloaded files" is
on in Settings, and each can be installed again, opened or deleted. Tasks lists
the downloads, installs, uploads and backups started in this session, running
or finished.

## Rename tab

The Rename tab makes a second copy of a game under another package name, so
it can be installed beside the original. Releases are added there directly or
with Make a copy on the Downloads tab. The tab first reads the APK and shows
what the copy changes: the package and other names in the manifest, resource
strings, OBB file names and install.txt lines. It warns about package names
inside the code, which the copy keeps, and about Firebase set up for the
original package.

Build copy writes a new release folder, `<release> (<new package>)`, into the
downloads folder, where it can be installed like any other release. The
original isn't changed, and OBB files are shared with it through hard links
when both are on the same drive. A `rename-report.txt` in the new folder lists
every change.

The copy is made without Java or rebuilding the APK: only the manifest, a few
resource strings and the signature change, and every other file is copied as
it is. Java class names stay the same, since native code looks classes up by
those names. Each copy's package gets its own signing key, kept in the data
folder under `signing`. Android only installs an update over an app signed
with the same key, so without that file a copy can't be updated, only
uninstalled and installed again. Settings lists these keys and can export
each one to a file and import it back, for example on another computer.
Building a copy of a newer version with the same package uses the same key,
so it installs over the older copy.

Copies are never offered for upload on the Library tab, and the app refuses
to upload a package it has a key for.

If the game has the Platform SDK loader (`libovrplatformloader.so`), it can be
replaced with a patched one, so the game starts on older headset firmware. The
app doesn't download that file; you choose one you got yourself, and the
report lists the SHA-256 of both.

## Console tab

The Console tab runs shell commands on the selected headset, the same as
`adb shell <command>` (a leading `adb shell` is dropped). Output appears as it
arrives, so `logcat` works and can be stopped. Up and Down go through earlier
commands, and the last 20,000 lines are kept. Commands run as typed and can
change or delete anything the shell user can reach.

## Library mirrors

The library is optional. Mirrors are added in the Mirrors section of Settings:

- a VRP-style server config, `{"baseUri": "https://.../", "password": "<base64>"}`;
- an rclone config with a remote that has a `Quest Games` folder.

Both kinds are read through `rclone`; a server config uses its `http` backend,
since some servers only answer rclone. Official builds send an API key to
mirrors as an `X-API-Key` header. rclone gets it through `RCLONE_HEADER`, so it
doesn't appear on its command line, and the logs only say whether a key was
sent. Uploads go to the mirror's upload server without it.

The mirror's `meta.7z` is unpacked under the data directory, and game notes
come only from it.

The Library tab shows the games as a list or as a grid of covers, sorted by
any column (click a header to sort, click again to reverse). The Popularity
column ranks games by the mirror's download counts, `#1` being the most
downloaded. With a headset
connected it also shows the installed version and warns when a game is larger
than the free space. `Ctrl+F` jumps to the search, `Enter` there selects the
first result, the arrow, Page Up/Down and Home/End keys move the selection, and
`Escape` closes the details or clears the search. Right-clicking a game opens
a menu with the download actions and copies of its name, release and package.
`Ctrl+Plus`, `Ctrl+Minus` and `Ctrl+0` change the interface scale, which can
also be set in Settings.

### Going easy on mirrors

Mirrors are run by volunteers, so the app limits how hard it hits them:

- games are downloaded one at a time on request; there is no "download all",
  and at most 10 downloads can be queued;
- at most 5 downloads run at once (2 by default), with an optional total speed
  limit in KiB/s or KB/s in Settings;
- the game list is refreshed at most once a day automatically, and a manual
  sync is refused if the last one was less than 10 minutes ago;
- interrupted downloads resume where they stopped; network errors are retried
  a few times with growing pauses, and refused requests (403, 404) not at all.

### Sharing new app versions

When a headset has an app the library doesn't have, or a newer version of one,
the Library tab offers to upload it through the mirror's own upload config
(`.meta/upload.config` in its metadata). The app ignores an upload config
that could reach files on this computer or run programs. The mirror's
blacklist and the headset's system apps are skipped, and each app can be
dismissed for good.

Nothing is uploaded until you press that app's Upload button. An upload
contains the APK, the app's OBB files, the headset model and an anonymous
headset id (SHA-256 of the serial number). The app only reads from the headset
here and changes nothing on it.

## Building from source

You need:

- Rust 1.95 or newer, installed with rustup; check with `rustc --version`.
- A C compiler for the TLS library: GCC or Clang on Linux, the Xcode command
  line tools on macOS (`xcode-select --install`), and the MSVC build tools on
  Windows (rustup installs them together with Rust).

The build needs no other system libraries. Then:

```bash
git clone https://github.com/JonnyBro/quest-sideloader-rust.git
cd quest-sideloader-rust
cargo build --release
```

The binary is `target/release/quest-sideloader` (`quest-sideloader.exe` on Windows).
`cargo run --release` builds and starts it in one step.

A build made this way has no API key, so mirrors that require one refuse it.
The key is never in the repository: official builds get it at compile time
from the `API_KEY` environment variable, set from a CI secret. For local builds
it can also live in an `api_key.txt` in the repository root, which git ignores;
after creating that file, run `touch crates/core/build.rs` once so cargo
notices it.

To run the tests and lints:

```bash
cargo test
cargo clippy --all-targets
```

Tests use fake adb servers and never talk to a connected headset. The mirror
and download tests run the real `rclone`, and a few use `7z`; they are skipped
when those aren't installed.

### Windows builds

On Windows, `cargo build --release` is enough. `.cargo/config.toml` links the
C runtime statically, so the exe runs without the Visual C++ Redistributable.
The same file hides linker warning LNK4099 (missing `.pdb` files for prebuilt
libraries), which only affects debugging symbols.

From Linux, build with [cargo-xwin](https://github.com/rust-cross/cargo-xwin),
which downloads the MSVC CRT and Windows SDK on first use (this means
accepting Microsoft's license for them). It also needs `clang` and `lld`:

```bash
rustup target add x86_64-pc-windows-msvc
cargo install --locked cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

The exe is `target/x86_64-pc-windows-msvc/release/quest-sideloader.exe`. The
API key is picked up the same way as in a native build.

### Running

To run the app you need:

- `adb` from Android SDK Platform-Tools, on `PATH` or with its path set in
  Settings. If the app can't find adb, it offers to download Platform-Tools
  from Google into its data folder (Windows, x86-64 Linux and macOS), or to
  open the download page.
- Windows: Meta's Quest ADB driver, so the headset shows up over USB. The
  Visual C++ Redistributable isn't needed, because the C runtime is built into
  the exe.
- Linux: a desktop with Wayland or X11, a Vulkan or OpenGL driver, and
  `xdg-desktop-portal` for file dialogs. The headset needs udev rules for USB
  access (for example the `android-udev` package).
- `rclone`, for any mirror. If a sync fails because
  rclone is missing, the app offers to download the official build from
  rclone.org and checks it against the published SHA-256 sums before using it.

On the first start, a short setup asks for the language, theme and downloads
folder and explains how to connect the headset and add a mirror. It saves
the settings whether you finish it or skip it, and doesn't open again.

Logs go to the platform data directory (`~/.local/share/quest-sideloader/logs`
on Linux). The Logs button in the status bar shows them in the app and turns red
after an error. Debug builds log in more detail, and `RUST_LOG` sets the level
in any build. The app sends a desktop notification when a task finishes while
its window is in the background; this can be turned off in Settings.

Settings and data go to the platform's user folders: on Windows the settings
are in `%APPDATA%\quest-sideloader\config` and everything else in
`%APPDATA%\quest-sideloader\data`. The window size, the library's sort
and filter, and the address of the last headset connected over Wi-Fi are
kept in `ui.ron` next to the settings. On start the app tries that address
once; if the headset is off or has another address, nothing is shown except
a line in the log. For a portable copy, put an empty
`portable.txt` next to the executable. The app then keeps everything, including
screenshots, in a `userdata` folder beside it. Paths in the settings are stored
relative to that folder, so the copy still works from another drive letter or
computer. The window title shows "(portable)" in this mode.

The interface is in English and Russian. It follows the system language by
default and can be switched in Settings without a restart. Translations are in
`crates/*/locales/*.yml`, and a string without a translation is shown in
English.

## License

GPL-3.0-only, see [LICENSE](LICENSE).
