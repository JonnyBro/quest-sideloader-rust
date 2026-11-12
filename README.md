# Quest Sideloader

[![CI](https://github.com/JonnyBro/quest-sideloader-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/JonnyBro/quest-sideloader-rust/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/JonnyBro/quest-sideloader-rust)](https://github.com/JonnyBro/quest-sideloader-rust/releases/latest)

Desktop sideloader for Meta Quest headsets, written in Rust with egui.

## Layout

- `crates/core` - everything that is not UI: settings, the background engine, an
  async client for the ADB server protocol, the mirror catalog, downloads,
  7z extraction and installation.
- `crates/app` - the egui desktop application. It sends commands to the engine
  and renders the events it gets back.

## Library mirrors

The library is optional. Add a mirror in Settings -> Mirrors:

- a VRP-style server config, `{"baseUri": "https://.../", "password": "<base64>"}`;
- an rclone config with a remote that has a `Quest Games` folder (needs `rclone`).

Both kinds can send extra HTTP headers, such as an `X-API-Key` the mirror
requires. For rclone they are passed through `RCLONE_HEADER`, not the command line.

The mirror's `meta.7z` is unpacked under the data directory; game notes come
only from it.

### Going easy on mirrors

Mirrors are run by volunteers, so the app limits how hard it hits them:

- games are downloaded one at a time on request; there is no "download all",
  and at most 10 downloads can be queued;
- at most 5 downloads run at once (2 by default), with an optional total speed
  limit in KiB/s or KB/s in Settings;
- the game list is refreshed at most once a day automatically, and a manual
  sync is refused if the last one was less than 10 minutes ago;
- interrupted downloads resume instead of starting over, and network errors are
  retried only a few times with growing pauses; refused requests (403, 404) are
  not retried;
- rclone requests are rate-limited with `--tpslimit`.

### Sharing new app versions

When a headset has an app the library doesn't have, or a newer version of one,
the Library tab offers to upload it through the mirror's own upload config
(`.meta/upload.config` in its metadata). The mirror's blacklist and the
headset's system apps are skipped, and each app can be dismissed for good.

Nothing is uploaded without a click on that app's Upload button. An upload
contains the APK, the app's OBB files, the headset model and an anonymous
headset id (SHA-256 of the serial number). The headset is only read from.

## Building from source

You need:

- **Rust 1.95 or newer**, installed with rustup; check with `rustc --version`.
- **A C compiler**, used to build the TLS library: GCC or Clang on Linux, Xcode
  command line tools on macOS (`xcode-select --install`), and the MSVC build tools
  on Windows (installed together with Rust by rustup).

No other system libraries are needed at build time. Then:

```bash
git clone https://github.com/JonnyBro/quest-sideloader-rust.git
cd quest-sideloader-rust
cargo build --release
```

The binary is `target/release/quest-sideloader` (`quest-sideloader.exe` on Windows).
`cargo run --release` builds and starts it in one step.

To run the tests and lints:

```bash
cargo test
cargo clippy --all-targets
```

Tests use fake adb servers and never talk to a connected headset. A few of them
also exercise the real `7z` and `rclone` binaries and are skipped when those
are not installed.

### Running

- `adb` (Android platform-tools) on `PATH`, or its path set in Settings.
- Linux: a desktop with Wayland or X11, a Vulkan or OpenGL driver, and
  `xdg-desktop-portal` for file dialogs. The headset needs udev rules for USB
  access (for example the `android-udev` package).
- `rclone`, only for rclone mirrors.

Logs go to the platform data directory (`~/.local/share/quest-sideloader/logs` on Linux);
set `RUST_LOG` to change verbosity.

## License

GPL-3.0-only, see [LICENSE](LICENSE).
