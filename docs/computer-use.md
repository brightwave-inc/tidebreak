# Computer use in Code mode

Tidebreak gives local coding sessions access to its in-app browser, native macOS
apps, and Google Chrome. Codex, Claude Code, and OpenCode use bundled MCP
servers. Grok uses the bundled CLI and reads screenshot files. The internal
engine calls the same host services through session-scoped tools.

The desktop owns permissions and input. A harness receives a private capability
for its session; it cannot choose another session, an arbitrary Chrome debugger
endpoint, or a broader grant. A headless server without an attached desktop does
not provide access to your computer.

## Start a browser task

1. Open your app or website in Tidebreak's browser beside your code.
2. Choose **Share with agent** and review the origin and screenshot disclosure.
3. Ask your coding agent to inspect the page, reproduce a problem, edit the code,
   and repeat the flow after rebuilding.

Your session can open more tabs for a shared origin. A first visit to an unshared
origin needs sharing through the desktop. The agent can close tabs it opened.
Showing an agent tab preserves the code editor's input focus and selection.

Browser actions run in background mode by default. They use DOM actions and show
a temporary ghost cursor at the observed action position. They do not move your
hardware pointer. DOM input is synthetic; sites that require trusted keyboard or
pointer events can refuse it. A foreground browser action asks permission to use
keyboard focus in Tidebreak. It does not silently replace a background action.

## Use native macOS apps

Native capture and control require macOS 14 or later, the packaged helper,
Accessibility permission, and Screen Recording permission where applicable.
Tidebreak asks for app access through a native dialog. Read and screenshot access do not authorize control. A whole-display screenshot
needs its own grant.
The disclosure explains that screenshots and visible content can reach your
selected model and provider.

Background native control uses accessibility actions. Supported apps can accept
button presses, text changes, scrolling, and resizing without activation. Native
hover, drag, key chords, menus, and inaccessible targets may return
`requires_foreground`. To proceed with an action that needs your pointer or
focus, the agent must request foreground mode and obtain separate approval.
Some applications can raise their own windows in response to accessibility
actions, so background mode cannot guarantee focus retention in every app.

To click a numbered screenshot target, the agent uses the badge from the most
recent annotated capture of that app. Reading a narrower accessibility tree does
not renumber that capture. Changed elements fail validation before input.

## Use Google Chrome

The agent requests `chrome_connect` with one of two modes:

- `managed` starts Chrome in the background with a temporary isolated profile.
  Use it to test local apps without your signed-in browser state.
- `existing` requests access to your existing Chrome profile. Tidebreak explains
  the wider tab scope, and Chrome must allow remote debugging. The host discovers
  the endpoint; the agent cannot supply one.

Chrome uses the debugging protocol for page input and screenshots. New tabs open
in the background. `chrome_activate_tab` explicitly brings a tab forward and
requires native approval. Disconnecting a managed browser closes it and removes
its temporary profile. Disconnecting an existing browser leaves it open.

## Stop and recover

The native/Chrome activity indicator distinguishes a pending request from a
completed action. Choose **Stop** to cancel input. Foreground native, Chrome, and
in-app browser actions share one input owner, so two sessions cannot take over
focus at once. A stopped foreground operation drains before ownership changes.

The browser's agent **Stop** halts every tab owned by that coding session. Opening
a replacement tab or rotating a capability does not clear Stop. To resume, share
a retained tab again through the desktop. If every owned tab has been closed,
start a new coding session. Other sessions keep their own access.

The native/Chrome indicator's **Resume** needs native approval. It does not
restore foreground takeover approval. Native observation remains available under
its existing read/capture grants after control stops. Chrome stops its whole
connection, including observation, and requires a newly approved connection.
Use **Stop sharing** or revoke the app grant to withdraw observation access.

If a response is lost after an action starts, Tidebreak records an unknown
outcome. The agent inspects the current state before proposing another action.
Reusing the original request does not repeat uncertain input.

## Development qualification

Run focused checks for the layers you change:

```sh
cargo test -p tidebreak-desktop --lib computer_use
cargo test -p tidebreak-desktop --lib browser
cargo test -p tidebreak-server-core --lib code::chrome
cargo test -p tidebreak-server-core --lib engine::internal::
cargo test -p tidebreak-cli --bin tidebreak browser::tests
cargo test -p tidebreak-cli --bin tidebreak computer_use::tests
cargo test -p tidebreak-harness --lib grok::
```

Mock transport tests do not establish native input behavior. Before releasing a
computer-use change, test the packaged desktop with a real issued session and
normal native grants. Observe the hardware pointer, foreground app, editor DOM
node, and selection while the agent opens tabs, clicks, fills and clears text,
selects, scrolls, captures, and stops. Verify that actual screenshot bytes reach
the next model request through each supported harness. Repeat a code change,
rebuild, and the UI flow. Record the tested source revision and any unsupported
actions. A locked Mac cannot qualify these checks.

Use an isolated development app and profile when testing Tidebreak itself.
The controlling Tidebreak app and operating-system authentication surfaces remain
blocked. See [decision 94](decisions/0094-computer-use-for-coding-harnesses.md)
for the architecture and permission contract.
