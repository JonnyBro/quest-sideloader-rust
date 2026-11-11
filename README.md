# Quest Sideloader

Desktop sideloader for Meta Quest headsets, written in Rust with egui.

## Layout

- `crates/core` — everything that is not UI: settings, the background engine and,
  later, ADB, the mirror catalog, downloads, extraction and installation.
- `crates/app` — the egui desktop application. It sends commands to the engine
  and renders the events it gets back.

## Build

```bash
cargo run --release
```

Logs go to the platform data directory (`~/.local/share/quest-sideloader/logs` on Linux);
set `RUST_LOG` to change verbosity.

## License

GPL-3.0-only, see [LICENSE](LICENSE).
