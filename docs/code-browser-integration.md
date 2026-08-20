# Code-mode browser integration

## Product role

The browser is a first-class center-editor tab, alongside files and diffs. It
is not an inspector destination and it does not replace the user's default
browser. Its job is to keep local development servers, documentation, and pull
requests in the same workspace as the task that produced them.

The browser chrome stays deliberately compact:

- back and forward;
- reload while idle, stop while loading;
- one address and search field;
- a security label derived from the actual URL;
- bounded per-tab history;
- open in the system browser.

Each browser tab owns one durable browser session. Closing a tab destroys that
session. Merely selecting another tab hides its native view so cookies, form
state, and the page's own history can survive normal workspace navigation.

## Architecture

### Editor ownership

The existing URL-backed layout remains authoritative for tab order, active
group, splits, and restoration. The browser panel should be represented as:

```ts
{ type: "browser"; browserId: string }
```

`browserId` is identity, not the current URL. The URL changes often and must
not turn one tab into a new tab. Browser session state is stored separately by
that ID.

Browser tabs participate in the same primary and secondary editor groups as
files and diffs. Main agent remains a persistent tab. Inspector navigation
remains separate.

### Browser state

The browser-specific state layer stores:

- the last committed URL and address text;
- title;
- loading, ready, and failure state;
- a bounded navigation history and current index;
- the last meaningful browser notice;
- the workspace that owns the tab.

Transient native handles and DOM bounds are never persisted. Storage is
best-effort and versioned. Invalid or oversized stored data is discarded.

### Native rendering

An iframe is not the product implementation. Tidebreak's main content-security
policy intentionally permits frames only from loopback, many real sites deny
framing, and cross-origin frames cannot provide trustworthy navigation state.

The desktop host instead creates a Tauri child webview over the browser tab's
content rectangle. Browser-specific IPC owns create, navigate, reload, stop,
back, forward, bounds, visibility, snapshot, and close. The native host emits
navigation, title, popup, and download events back to the main app webview.

The remote page receives no Tidebreak capability. External URL validation is
performed in both the renderer and native host. Only HTTP and HTTPS are
accepted, credentials in URLs are rejected, and non-web schemes are blocked.
In development, both loopback spellings of the Vite origin are rejected so a
browser tab cannot enter the one remote origin that receives debug capability.

### Native overlay behavior

A child webview is a native sibling of the app webview, not a DOM child. DOM
portals cannot draw over it. `CodeBrowserTab` therefore accepts an `obscured`
prop and hides the native view while a workspace dialog, command palette, or
other app-owned overlay covers the editor area. The aggregate must include tab
context menus and workspace menus whose portals can overlap the editor, not
only modal dialogs. The browser also hides itself while its own history menu is
open.

The content rectangle is measured in logical CSS pixels and updated through a
coalesced `ResizeObserver`. This covers editor splits and resizable side rails
without a per-frame layout loop.

## Owner-file integration signatures

The sidecar deliberately does not edit the current workspace owner files. The
integration PR needs these narrow changes:

1. Add `{ type: "browser"; browserId: string }` to `PanelContent`, key it as
   `browser:${browserId}`, and encode it as `browser.${browserId}`.
2. Treat browser panels as editor tabs in `codeChrome.ts`. Generalize
   `openCodeEditor` from file/diff to file/diff/browser.
3. Render a browser icon and browser title in `CodeCenterTabs.tsx`. The title
   can be supplied by `storedBrowserTitle(browserId)` and falls back to
   `Browser`. Keep an in-memory title map updated from `CodeBrowserTab`'s
   `onTitleChange` callback so title changes repaint the strip without polling
   local storage.
4. Add an `openBrowser(url?: string, preferredRegion?)` command in the
   workspace owner. It creates an opaque browser ID, calls
   `seedBrowserSession({ browserId, workspaceId, initialUrl: url })`, and opens
   the panel through the existing editor-layout path.
5. Render
   `<CodeBrowserTab workspaceId browserId obscured={workspaceOverlayOpen} onTitleChange={...} />`
   from the editor-panel switch. The component resets itself when `browserId`
   changes, so the shared active-panel slot cannot carry one tab's React state
   into another browser tab. `workspaceOverlayOpen` must aggregate quick-open,
   dialogs, and any portaled menu that can cross the browser content rectangle.
6. Before removing browser panels for explicit single-tab, close-other,
   close-right, close-all, or workspace teardown actions, call
   `closeCodeBrowser(browserId)` for every browser ID the layout mutation will
   remove. Switching tabs or collapsing/merging a split must not close native
   sessions that remain in the resulting layout.
7. Route links from transcripts, pull requests, and an eventual `Open in`
   control through `openBrowser`; retain `Open externally` as an explicit
   alternate action.

## Failure and recovery

- Invalid address input stays in the toolbar with a precise inline error.
- Native creation or command failures replace the web surface with a retry
  state and preserve the requested URL.
- A long load keeps the page visible and reports that it is taking longer,
  rather than destroying an authenticated or slow session.
- Popups and downloads are denied by the embedded host. The toolbar exposes
  safe HTTP(S) popup URLs in the current tab and safe download URLs externally.
  Unsafe blocked navigation never offers a retry action.
- On remount, the renderer asks the native host for a snapshot. If the native
  view no longer exists, it is recreated from persisted state.
- A surviving native view keeps using its real back/forward stack. If the
  desktop process lost that view, restored history entries navigate directly
  until the new native stack is trustworthy, avoiding a visible URL change
  backed by a no-op `window.history.back()`.
- A plain browser build shows a desktop-only fallback and retains Open
  externally; it never attempts an iframe.

## Platform constraints

- Tauri 2.11 child webviews require the `unstable` Cargo feature. The project
  pins Tauri exactly, which limits API drift risk.
- Child-webview creation runs through an async Tauri command. The pinned API
  documents a WebView2 deadlock when child views are created from synchronous
  commands on Windows.
- Child webviews are desktop-only. This feature does not provide a mobile
  implementation.
- Hidden-webview background throttling is only configurable on macOS 14+ and
  iOS 17+; Windows and Linux ignore that setting. The browser must not promise
  background execution.
- Native child webviews sit above DOM overlays on macOS, Windows, and Linux.
  The explicit `obscured` contract is required for dialogs and palettes.
- Browser-engine behavior follows the platform runtime: WKWebView on macOS,
  WebView2 on Windows, and WebKitGTK on Linux. Cross-platform validation must
  cover authentication, redirects, history, popup denial, and process restart.
- Downloads are denied in this slice because there is no consented destination
  or product-owned download shelf yet.
- A popup that starts as `about:blank` and navigates later cannot be recovered
  after denial. OAuth flows built around that bootstrap need a deliberate
  popup-host design or an explicit Open externally fallback in a later slice.
