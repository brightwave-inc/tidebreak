# openwave-desktop

Tauri shell for OpenWave. On launch it binds `openwave-server` to an ephemeral
loopback port, mints a per-launch bearer token, and hosts the React chat UI in a
webview. The UI talks to that local API over HTTP + WebSocket (subprotocol auth).
The native host also owns the folder picker and a private host-broker sidecar;
the renderer receives opaque folder IDs and display names, never absolute paths.

## Prerequisites

- Rust toolchain (`rust-toolchain.toml` at the repo root)
- [pnpm](https://pnpm.io)
- [Tauri CLI 2](https://v2.tauri.app/start/prerequisites/):
  `cargo install tauri-cli --version "^2"`
- Platform WebView deps (macOS: Xcode CLT; Linux: WebKitGTK — see Tauri docs)

## Run locally (native window)

From this directory:

```sh
pnpm --dir ui install
cargo tauri dev
```

That starts Vite on `http://localhost:1420` and opens the OpenWave window. The
Rust host boots the in-process API; the webview calls `server_info` to learn the
base URL and token. The Tauri pre-dev command builds and stages the broker
sidecar for the current target automatically.

Create an installable bundle. The before-build hook compiles the target-specific
broker and the default Tauri configuration includes it automatically:

```sh
cargo tauri build
```

## Run locally (browser UI against `openwave serve`)

Useful when iterating on the React UI without rebuilding the native shell.

```sh
# terminal 1 — from the repo root
cargo run -p openwave-cli -- serve
# note the printed URL and token

# terminal 2
cp ui/.env.example ui/.env.local
# edit ui/.env.local: VITE_OPENWAVE_URL, VITE_OPENWAVE_TOKEN,
# and an explicit absolute VITE_OPENWAVE_SCRATCH on the machine running serve

pnpm --dir ui install
pnpm --dir ui dev
# open http://localhost:1420
```

`.env.local` is gitignored (Vite convention).

## Layout

| Path | Role |
| --- | --- |
| `src/` | Tauri host (server boot, sidecar lifecycle, native folder consent) |
| `ui/` | React + Vite frontend |
| `scripts/` | Cross-platform sidecar staging for Tauri dev/build |
| `binaries/` | Generated target-specific sidecar (gitignored) |
| `tauri.conf.json` | Window, CSP, sidecar bundle, and build commands |
| `icons/` | App icons (generated from the brand mark) |
| `capabilities/` | Tauri ACL (loopback remote URLs for the local API) |
