# Chrome computer use for coding harnesses

Chrome computer use is the third adapter behind Tidebreak's one
`computer_session` wire, alongside the in-app browser and native app control.
This document is for setup and for the parent integration owner. The Chrome
tool contracts live in `tidebreak_core::chrome_computer_use`; the host-owned
runtime lives in `tidebreak_server_core::code::chrome`.

## What the adapter does

The adapter drives Google Chrome over the DevTools Protocol using the exact
`tokio-tungstenite 0.30.0` stack already pinned in the workspace. It supports:

- target discovery on the approved connection and per-session target tables;
- open, close, activate, and navigate tabs;
- bounded semantic snapshots with stable per-snapshot refs and document epochs;
- epoch-bound screenshots returned as image bytes, never as model-facing text;
- click, double-click, type, fill, select, check, hover, press, scroll, and
  drag-bounds actions against re-resolved snapshot refs;
- bounded URL, load-state, text-present, and text-absent waits;
- bounded console/exception and network/error diagnostics for code testing;
- Stop/takeover cancellation through a shared ownership latch, with
  request-id outcomes and no replay of unconfirmed actions.

Chrome content is untrusted page data. The adapter never evaluates arbitrary
model JavaScript, never logs credential values, never exports cookies or
profiles, and never accepts a debugger endpoint, data-directory path, or raw
CDP method from a model.

## Connection model

There are two supported approval paths, both host-owned.

### 1. App-managed isolated Chrome (fully supported, Linux-validated)

The host helper launches Chrome with a fresh, isolated user-data directory:

```text
chrome --remote-debugging-port=0 --user-data-dir=<isolated profile>
```

Chrome writes `DevToolsActivePort` in that profile with the chosen loopback
port and `/devtools/browser/<id>` path. The helper must:

1. resolve and validate the executable (never an unpinned download);
2. create the profile directory itself (never from model input);
3. read `DevToolsActivePort`, bind only to `127.0.0.1`, and pass the derived
   `ws://127.0.0.1:<port><path>` endpoint into
   [`ChromeComputerUseService::install_connection`];
4. keep the connection id, profile directory, and process handle in the
   durable native state so restart can invalidate stale references.

This profile is not the user's existing Chrome profile. It is the app-managed
test browser; do not imply blank-profile access equals access to the user's
existing sign-ins or browsing history.

### 2. User-approved existing profile (Chrome 144+)

Chrome 144's official remote-debugging enablement (`chrome://inspect/#remote-debugging`)
lets the user allow incoming debugging connections for their existing default
profile without exposing it on a fixed remote-debugging port. Follow the
official Chrome DevTools flow, then read the same `DevToolsActivePort`
handshake. The minimum supported profile flow is:

1. Chrome 144+ is running with the user's existing profile.
2. The user opens `chrome://inspect/#remote-debugging` and manually enables
   incoming debugging connections for the profile; Chrome shows the official
   allow dialog before accepting any client.
3. The host discovers the resulting local debugging transport and derives the
   loopback endpoint. A bundled native-messaging extension can provide
   equivalent tab-sharing integration for profiles that do not expose
   `DevToolsActivePort`.
4. The native UI records an **explicit wider grant** before attaching: the
   agent can read pages, capture screenshots, navigate, act, and read bounded
   diagnostics across every site open in the selected profile. Chrome DevTools
   has no selective-domain isolation, so a per-site boundary cannot be
   promised over a direct CDP attachment. The consent copy must disclose that.

Do not connect to a manually opened `--remote-debugging-port` port for an
existing profile: Chrome refuses and requires a non-default user-data dir for
that flag, which is the app-managed path above.

## Endpoint safety rules

- The model never supplies a debugger endpoint, `ws://` URL, user-data
  directory, executable path, or full CDP method.
- `install_connection` accepts only loopback `ws://127.0.0.1`, `ws://localhost`,
  and `ws://[::1]` endpoints.
- Every call is bound to `{owner, workspace, session}` by a
  [`ChromeScope`] derived from durable host state; call arguments cannot widen
  it. The connection is bound to one workspace.
- Every target reference is minted inside the service target table and bound
  to one session. A different session or workspace gets no resolution.
- Navigation, new tabs, and screenshots re-check the live origin grant.
  Actions re-check origin consent, the session's snapshot, the document epoch,
  and the ownership latch. A denied origin has **no native fallback**.
- Stop/takeover trips the shared latch and invalidates nothing durable by
  itself; session revocation invalidates every target reference.

## Public entrypoints (parent integration)

Core contracts, exported from `tidebreak_core`:

- `chrome_computer_use_tool_specs() -> Vec<ToolSpec>` and
  `validate_chrome_computer_use_arguments(name, arguments) -> bool`;
- typed args/results: `ChromeListTabsArgs`, `ChromeNewTabArgs`,
  `ChromeTabRefArgs`, `ChromeNavigateArgs`, `ChromeSnapshotArgs`,
  `ChromeScreenshotArgs`, `ChromeActArgs`, `ChromeWaitArgs`,
  `ChromeDiagnosticsArgs`, and matching result structs;
- grant vocabulary: `ChromeConnectionGrant`, `ChromeOriginScope`,
  `ChromeGrantCapability`.

Server runtime, exported from `tidebreak_server_core::code::chrome`:

```text
pub struct ChromeScope {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub cancel: CancelToken,
}

pub struct ChromeConnectionSpec {
    pub connection_id: String,
    pub workspace: WorkspaceId,
    pub endpoint_label: String,
    pub websocket_endpoint: String,   // loopback only, host-derived
    pub grant: ChromeConnectionGrant, // Origin(scope) or DeveloperAllSites
    pub managed_isolated: bool,
}

ChromeComputerUseService::dispatch(
    &self,
    scope: &ChromeScope,
    call: &ComputerUseCall,
) -> ChromeCallOutcome   // wraps ComputerUseResult

ChromeComputerUseService::install_connection(spec, CdpSession)
ChromeComputerUseService::uninstall_connection(connection_id)
ChromeComputerUseService::revoke_session(session)
ChromeComputerUseService::state(&ChromeScope) -> ChromeAdapterState
ChromeComputerUseService::ownership() -> ChromeOwnership   // trip/resume
```

Required native wiring to emit the Chrome tool set:

1. Construct `ChromeComputerUseService` once per host.
2. When the user approves Chrome in native settings, derive
   `ChromeConnectionSpec` from durable consent and
   `CdpSession::connect(host_derived_endpoint)`, then call
   `install_connection`.
3. **Before** advertising tools, read `service.state(scope)`; advertise only
   when `available` is true and the grant label matches the disclosure the
   user accepted.
4. Per call, derive `ChromeScope` from the session token/registry, pass
   `call.arguments` through core validation, call `dispatch`, and forward the
   `ChromeCallOutcome`'s `ComputerUseResult` to the shared service. Screenshot
   image bytes ride `result.images`; do not serialize base64 into text.
5. On Stop/takeover, call `ownership().trip()`; on human resumption,
   `ownership().resume()`. On session termination, call `revoke_session`.
6. Action calls (**chrome_act**) must be Sensitive in the transport approval
   vocabulary and card at act time; the parent decides product copy, but the
   disclosure above is required for `DeveloperAllSites`.

## Diagnostics bounds

`chrome_diagnostics` returns at most 200 console entries and 200 network
entries per call, keeps a 512-entry ring per target, truncates any field to
4096 characters, and never returns cookies, request bodies, or response
bodies. The target's own frames are included; other tabs' traffic is not.

## Setup for app-managed isolated Chrome

Linux (parent qualifies macOS and packaged UI separately):

```bash
CHROME=/usr/bin/google-chrome  # parent resolves the real path
PROFILE="$(mktemp -d)"
"$CHROME" --remote-debugging-port=0 \
  --user-data-dir="$PROFILE" &
# Wait for "$PROFILE/DevToolsActivePort", then pass
# ws://127.0.0.1:<port><path> to install_connection.
```

Windows uses `%LOCALAPPDATA%\Tidebreak\chrome-profiles\<id>`; macOS uses
`~/Library/Application Support/Tidebreak/Chrome Profiles/<id>`. Never read a
profile path from a model argument.

## Existing-profile extension (local setup)

A bundled native-messaging extension (`chrome-extension/` under this repo's
future packaging tree) can bridge tabs from a user's existing profile into the
same `install_connection` path when Chrome's own `DevToolsActivePort` flow is
unavailable. For local development:

1. `chrome://extensions`, enable Developer mode, Load unpacked, and select the
   extension directory.
2. The extension connects to the Tidebreak native messaging host and shares a
   per-request tab reference; the host resolves that reference only after the
   same `DeveloperAllSites` consent.
3. Publication to the Chrome Web Store is a separate product decision; local
   install is enough for qualification.

## Real-browser fixture

`crates/tidebreak-server/tests/chrome_real.rs` launches a headless Chromium
fixture when `TIDEBREAK_CHROME_BIN` points at a Chrome/Chromium binary. Mock
CDP protocol tests in `crates/tidebreak-server/src/code/chrome/tests.rs` run
without a browser and cover attachment, target isolation, navigation epochs,
stale refs, cancellation, and unknown outcomes.
