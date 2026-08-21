# 54. Agent Browser Uses an Engine-Neutral Contract and Ships on WKWebView First

- Status: Proposed
- Date: 2026-08-20
- Owners: desktop, agent runtime
- Related: [`0013-computer-use-screen-capture-and-app-control.md`](0013-computer-use-screen-capture-and-app-control.md),
  [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0033-code-mode-approvals.md`](0033-code-mode-approvals.md),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md),
  [`docs/code-browser-integration.md`](../code-browser-integration.md),
  [`docs/deferred.md`](../deferred.md) ("A browser without ambient computer control")

## Context

Tidebreak now has first-class browser tabs, but they are viewing surfaces rather
than agent capabilities. The native host can create, navigate, resize, hide,
show, and close one child webview per tab. It reports navigation and title
events, blocks unsafe schemes, and keeps remote pages outside Tidebreak IPC.
The renderer persists a bounded navigation projection in `localStorage`.

That is enough for a person to use a local preview or read documentation. It is
not enough for an agent. An agent needs a bounded way to inspect the current
document, address a named target, act on it, wait for a deterministic result,
and prove that the target did not change between inspection and action. It also
needs the same browser from both foreground chat and code harnesses. Building a
different browser API for each agent surface would preserve the exact split
decision 48 is removing elsewhere.

The browser is also a sharper trust boundary than an ordinary tool. Page text
can contain prompt injection. A browser profile can contain authenticated
cookies. Form input can publish data or commit an external effect. File inputs
and downloads can become ambient filesystem access if their destinations are
not capabilities. The page, renderer, model, and harness therefore cannot own
browser authority.

Three engine strategies were evaluated for the first production release.

### Extend the current platform webview

On macOS Tidebreak already embeds WKWebView through pinned Tauri and Wry
versions. Tauri's `with_webview` hook exposes the native WKWebView on the main
thread. The pinned stack supports document-start scripts, native JavaScript
evaluation with completion handlers, app-private website data, screenshots,
focus and bounds, and native input constrained to the child view. It preserves
the browser UI and lifecycle that already shipped.

The limitation is semantic reach. Same-origin documents can be inspected
directly. Cross-origin frames cannot be walked from the top document without
additional native frame APIs, and JavaScript-dispatched input is not a trusted
user gesture. The macOS adapter must therefore combine an isolated inspection
script with input directed at a freshly resolved native element rectangle, and
must report an opaque frame as unsupported rather than guess inside it.

WKWebView's named persistent data stores are available only on macOS 14 and
later in the pinned Wry implementation. Tidebreak still supports macOS 10.15.
On macOS 10.15 through 13 the browser therefore uses Tidebreak's app-private
default website data store, which remains separate from Safari and every other
browser profile but is not a separately named store inside the Tidebreak app.
Reset on those systems must delete browser-origin website data deliberately;
it cannot clear the whole app data store.

### Replace the whole Tauri runtime with CEF

Tauri's `feat/cef` branch was evaluated at commit
`f5bf953fe2a259f2d176491f50ec2930fb73e03d` from 2026-08-19. It contains an
experimental `tauri-runtime-cef` backed by CEF 150, persistent request contexts,
child views, and direct DevTools Protocol send/listen methods. This is the best
long-term semantic automation shape: Chromium and CDP give one implementation
for DOM/AX inspection, cross-origin frames, screenshots, input, waits, console,
and network diagnostics.

It is not a browser-tab-only dependency. Selecting it changes the runtime of
the whole Tidebreak desktop application and requires unreleased Git versions of
Tauri and related plugins. It also changes macOS signing and hardened-runtime
requirements and materially increases the application bundle.

The evaluated branch still has open runtime issues, including a macOS menu
deadlock, Linux DevTools IPC failure, and a reproducible macOS arm64 CEF crash
after submitting real Google credentials:

- <https://github.com/tauri-apps/tauri/issues/15888>
- <https://github.com/tauri-apps/tauri/issues/15764>
- <https://github.com/tauri-apps/cef-rs/issues/456>

Those failures do not make CEF the wrong destination. They make an unreleased
whole-app migration the wrong prerequisite for the first useful agent browser.

### Run an isolated Chromium sidecar

A separately launched Chromium process supplies CDP without changing the Tauri
runtime. It also gives the strongest process and profile isolation. The missing
piece is the product surface: Tidebreak would have to parent an external native
window or stream frames into a canvas and translate input back over CDP.
Parenting is platform-specific and recreates much of a CEF integration;
screencasting adds latency, CPU cost, accessibility loss, text/IME problems,
and a visible mismatch between what a person and the agent control.

A headless sidecar may be a good future background-browser engine. It is not
the best engine for the foreground, shared, in-app browser this decision covers.

### Qualification snapshot

This record does not treat an API's existence as a production pass. The table
records the evidence available on 2026-08-20 and the disposition downstream
work must use.

| Gate | WKWebView in the pinned app | Tauri CEF at the evaluated commit | Chromium sidecar |
| --- | --- | --- | --- |
| Tidebreak menus, plugins, protocols, dialogs, deep links, updater, IPC | **Pass:** this is the runtime already shipping; browser tabs landed in #2287 without replacing it | **Not demonstrated:** CEF is an alternate whole-app runtime and open integration bugs remain | **Pass for the app runtime:** the sidecar would not replace Wry |
| Child-view lifecycle, split, resize, hide/show, focus | **Pass:** #2287 and focused UI/native tests cover the existing lifecycle | **Implemented but not release-qualified:** the branch exposes child views; no Tidebreak packaged pass exists | **Fail for v1 UX:** a separate window or pixel stream would replace the current child-view contract |
| Persistent app-private profile | **Pass with documented version split:** named data store on macOS 14+, app-private default store on 10.15-13 | **Implemented but unverified in Tidebreak:** CEF request contexts accept persistent cache paths | **Feasible:** a dedicated Chromium user-data directory is straightforward |
| Snapshot, screenshot, semantic input, waits | **Feasible for supported v1 pages:** native WKWebView callbacks and screenshots exist; implementation evidence belongs to the semantic-driver issue | **API pass, product unverified:** CDP send/listen is present | **API pass, product fail:** CDP works, but shared visible rendering remains unresolved |
| Cross-origin frame semantics | **Fail by default:** v1 returns `unsupported_frame` rather than guessing | **Expected pass through CDP, not yet exercised in Tidebreak** | **Expected pass through CDP** |
| Real credential-submission stability | **Pass by existing platform engine posture; no new embedded engine is introduced** | **Fail:** cef-rs issue 456 reproduces a macOS arm64 crash after real Google credential submission | **Unverified:** would require a pinned Chromium build and packaged flow |
| Signing, notarization, universal build, updater | **Pass:** current release and staging pipelines already build this runtime | **Not demonstrated:** new CEF frameworks, subprocesses, entitlements, and bundle layout have no Tidebreak release pass | **Not demonstrated:** Chromium distribution, signing, and updater ownership are undefined |
| Bundle and idle-memory delta | **Pass:** no second engine is added | **Not measured:** no production candidate is admitted while the stability gate fails | **Fail for v1 default:** a second browser engine is necessarily bundled or provisioned |

The result is decisive for the milestone: WKWebView is the selected macOS v1
adapter, CEF is a future replacement candidate, and a sidecar is reserved for a
separate background-browser decision. A later CEF proposal must replace every
"not demonstrated" cell with packaged Tidebreak evidence; it cannot reopen the
choice from API shape alone.

## Decision

**The product contract is engine-neutral.** Browser identity, profile policy,
semantic snapshots, target refs, actions, waits, grants, controller ownership,
and audit use Tidebreak-owned types. Engine-specific handles and capabilities
stay behind a `BrowserEngine` boundary in the native desktop host.

The minimum contract is:

- session and tab lifecycle: list, open, close, activate, navigate, back,
  forward, reload, stop;
- observation: URL, title, load state, bounded semantic snapshot, screenshot;
- semantic action: click, fill/type, select, check, focus, hover, key press,
  scroll;
- deterministic wait: URL, load state, text presence or absence, element state,
  bounded timeout;
- controlled popup creation and profile reset;
- explicit engine capability reporting and typed unsupported results.

**macOS v1 ships on the existing WKWebView runtime.** It uses the current
visible child webview, a native evaluation callback for bounded snapshots, and
native input constrained to a target resolved from the latest snapshot. A
cross-origin frame that the adapter cannot inspect returns `unsupported_frame`.
It never falls back to a coordinate guessed by the model.

**CEF qualification runs in parallel, not on the critical path.** CEF may
replace WKWebView before release only if all of these pass on the exact pinned
commit proposed for production:

1. existing menus, Tauri plugins, custom protocols, dialogs, deep links,
   updater, and application IPC;
2. child-view create, split, resize, hide, show, focus, and recovery;
3. isolated persistent profile and reset;
4. semantic snapshot, screenshot, trusted input, deterministic waits, popup,
   and cross-origin frame behavior;
5. repeated real Google sign-in without a browser-process crash;
6. hardened-runtime signing, notarization, universal build, and updater
   packaging;
7. recorded bundle and idle-memory deltas;
8. no known release-blocking runtime issue.

Failure of any gate leaves WKWebView selected. It does not block the shared
contract or the developer-web milestone.

**The control loop is snapshot, act, re-snapshot.** A snapshot carries a
`BrowserSnapshotId` and `DocumentEpoch`. Interactive nodes receive compact refs
such as `@e12`. Every targeted action supplies the browser, tab, snapshot,
document epoch, and ref. The host re-resolves the ref immediately before input.
Navigation, replacement, ambiguity, or changed identifying content returns
`stale_target` without input synthesis.

**One visible browser is shared by person and agent.** Acting brings the tab to
the foreground. The UI shows the controlling session and current action. Stop
is a native latch checked before every action; human input pauses queued agent
actions and transfers control. Hidden tabs may be observed only where the
engine guarantees a current snapshot, and are never acted on in v1.

**The browser profile belongs to Tidebreak, not to an agent.** It is persistent
inside Tidebreak app data, resettable, and never imports Safari, Chrome, or a
password manager. Cookie presence is not authority: each controller is still
scoped to browser ids and granted origins. On macOS 14 and later the browser
uses a stable named WKWebsiteDataStore. Older supported macOS versions use the
app-private default store with targeted browser-origin deletion for reset.

**Both agent surfaces use the same runtime.** Foreground chat reaches it through
validated client-executed tools and the existing claim/heartbeat/resolve path.
Code harnesses receive a session-scoped capability through a typed
`BrowserChannelSpec` and the universal `tidebreak browser ... --json` CLI.
Adapters may additionally expose the same commands through authenticated MCP;
there is no provider-specific browser implementation.

**Page content is untrusted data.** Snapshot text, attributes, console output,
network data, and downloads never become instructions or authority. Password
and one-time-code values are omitted, and v1 requires human takeover to enter
them. Uploads resolve logical Tidebreak resource ids; downloads land in an
app-owned staging area and require an explicit export capability.

Developer console, network, and arbitrary evaluation are not baseline browser
tools. They may be enabled later as an explicit developer capability on an
engine that can isolate and audit them.

## Alternatives Considered

**Wait for Tauri CEF to stabilize before building any browser control.**
Rejected. The developer-web workflow is useful on WKWebView now, and the
engine-neutral contract makes later replacement deliberate rather than a
rewrite.

**Expose raw JavaScript evaluation as the agent tool.** Rejected. It gives the
model a large, hard-to-audit capability, makes prompt injection more powerful,
and cannot express stale-target safety. Evaluation remains an engine
implementation detail or a separately enabled developer capability.

**Drive the browser through general computer use.** Rejected for the same
reason decision 13 excluded it: browser identity, origin grants, profile state,
DOM semantics, file transfer, and navigation need a browser-specific authority.
Accessibility control of the user's everyday browser is not a managed browser.

**Implement three platform-webview adapters before shipping.** Rejected. The
contract is cross-platform, but the first release is macOS-first. Requiring
WebView2 and WebKitGTK parity would delay the validated workflow and multiply
engine-specific code before the contract is proven.

**Persist browser authority in renderer state.** Rejected. A compromised page
or renderer could forge ownership, target another workspace, or redeem its own
approval. The renderer receives only a bounded projection.

## Consequences

The first macOS adapter will have an explicit cross-origin-frame limitation
that Chromium may later remove. Tool results and UI must make the limitation
legible; no test may pass because the adapter silently clicked a guessed point.

The native host gains platform-specific WebKit code. That code is confined to
one adapter and the Tauri/Wry versions remain pinned exactly, so upgrades can be
reviewed against the browser fixture.

macOS 10.15 through 13 cannot have a named persistent WKWebsiteDataStore in the
pinned stack. Their profile remains app-private but reset is more expensive and
must be origin-targeted. Raising the minimum macOS version would simplify this;
this decision does not raise it.

CEF remains a credible destination rather than a permanently rejected option.
The go/no-go gates intentionally include real sign-in stability even though
arbitrary signed-in SaaS is not promised in v1: a normal browser navigation
must not crash the desktop process.

Background browser execution remains separate. A future durable background
agent may prefer a headless Chromium sidecar, but it must define visibility,
credential, approval, and artifact semantics of its own instead of inheriting
foreground authority.

Revisit this decision when Tauri publishes a stable CEF runtime that passes the
listed gates, when cross-origin frames become necessary for the supported
developer workflow, when Windows or Linux browser work receives an owner, or
when a background-browser decision is proposed.

## Validation

- A deterministic local fixture exercises SPA navigation, dynamic stale
  targets, same- and cross-origin frames, popup, redirect, delayed content,
  ordinary and credential fields, upload, download, console failure, and a
  page-authored prompt-injection string.
- Contract tests prove refs are scoped to one browser, tab, snapshot, and
  document epoch, and that an invalid ref produces no engine action.
- A real macOS run completes snapshot -> action -> wait -> re-snapshot against
  the fixture while the tab remains visible.
- Human input and Stop both win before the next native dispatch.
- The fixture and its endpoint tests run without external services or package
  dependencies.
- Any proposed CEF switch supplies results for every gate above against the
  exact dependency commit and packaged app configuration.
