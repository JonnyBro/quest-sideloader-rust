# Quest Sideloader

Desktop sideloader for Meta Quest headsets, written in Rust with egui.

## Layout

- `crates/core` — everything that is not UI: settings, the background engine, an
  async client for the ADB server protocol and, later, the mirror catalog,
  downloads, extraction and installation.
- `crates/app` — the egui desktop application. It sends commands to the engine
  and renders the events it gets back.

## Requirements

`adb` (Android platform-tools) on `PATH`, or its path set in Settings. On Linux the
headset also needs udev rules (for example the `android-udev` package).

## Library mirrors

The library is optional. Add a mirror in Settings → Mirrors:

- a VRP-style server config, `{"baseUri": "https://.../", "password": "<base64>"}`;
- an rclone config with a remote that has a `Quest Games` folder (needs `rclone`).

Both kinds can send extra HTTP headers, such as an `X-API-Key` the mirror
requires. For rclone they are passed through `RCLONE_HEADER`, not the command line.

The mirror's `meta.7z` is unpacked under the data directory; game notes come
only from it.

## Build

```bash
cargo run --release
```

Logs go to the platform data directory (`~/.local/share/quest-sideloader/logs` on Linux);
set `RUST_LOG` to change verbosity.

## License

GPL-3.0-only, see [LICENSE](LICENSE).
