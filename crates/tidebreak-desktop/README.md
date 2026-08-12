# tidebreak-desktop

Tauri shell for Tidebreak. On launch it binds `tidebreak-server` to an ephemeral
loopback port, mints a per-launch bearer token, and hosts the React chat UI in a
webview. The UI talks to that local API over HTTP + WebSocket (subprotocol auth).
The native host also owns the folder picker and a private host-broker sidecar;
the renderer receives opaque folder IDs and display names, never absolute paths.

The Documents surface derives its corpus from the authoritative current chat:
project chats use that project's documents and loose chats use the unscoped
corpus. A native file picker
accepts text and Markdown files, opens the selected regular file once without
following a final symlink, applies the upload bound to the open handle, and sends
the bytes directly to the in-process API. The webview receives only a safe
catalog/search projection: bounded display titles, processing states, and plain
passages. Source paths, bytes, index metadata, and generation identities remain
native/server-side.
Canonical document routes also require the native executor credential in this
embedding, so the renderer's ordinary bearer cannot bypass that projection.

When an agent needs a folder outside the current context, the chat UI renders a
bounded consent card from the local API's authoritative pending-work list. The
user can decline or ask the native host to open a picker. Native code then owns
the fenced claim, broker registration, and durable recovery receipt. A broker
mutation is marked durably before its single bounded dispatch and is never
replayed after an ambiguous response. Background and restart recovery only query
its exact operation receipt and publish a known result. Native mutation routes
require a second credential that is never exposed to the webview. Closing the
picker is a normal decline, and paths remain confined to app-private native state.

## Prerequisites

- Rust toolchain (`rust-toolchain.toml` at the repo root)
- [pnpm](https://pnpm.io)
- [Tauri CLI 2](https://v2.tauri.app/start/prerequisites/):
  `cargo install tauri-cli --version "^2"`
- Platform WebView deps (macOS: Xcode CLT; Linux: WebKitGTK — see Tauri docs)

## Run locally (native window)

From anywhere in the repo:

```sh
scripts/dev.sh
```

That installs the UI dependencies and opens the window. Arguments are forwarded
to `cargo tauri dev`. The long way, from this directory, is the same two steps:

```sh
pnpm --dir ui install
cargo tauri dev
```

That starts Vite on `http://localhost:1420` and opens the Tidebreak window. The
Rust host boots the in-process API; the webview calls `server_info` to learn the
base URL and token. The Tauri pre-dev command builds and stages the broker
sidecar for the current target automatically.

Debug builds run under the identifier `io.brightwave.tidebreak.dev` (instead of
`io.brightwave.tidebreak`), so `cargo tauri dev` keeps its own single-instance
lock and app-data directory and can run alongside an installed release build.
This means dev and release profiles do not share state: the first dev run after
this change starts from an empty data directory.

Create an installable bundle. The before-build hook compiles the target-specific
broker and the default Tauri configuration includes it automatically:

```sh
cargo tauri build
```

## Run locally (browser UI against `tidebreak serve`)

Useful when iterating on the React UI without rebuilding the native shell.

```sh
# terminal 1 — from the repo root
cargo run -p tidebreak-cli -- serve
# note the printed URL and token

# terminal 2
cp ui/.env.example ui/.env.local
# edit ui/.env.local: VITE_TIDEBREAK_URL and VITE_TIDEBREAK_TOKEN

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
