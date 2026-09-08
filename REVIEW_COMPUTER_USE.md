# Independent review: computer use integration (`thet/computer-use-parity`)

- Reviewed SHA: `393a454e178e0bb9d51c5023d06ad4709dccc5e4` (branch tip at review time)
- Diff base: `dc23ca27a096534230bdbe00ce137251b1742ff1`
- Scope (as assigned): shared CLI/MCP native+Chrome channel, internal engine
  image handling, native authorization routes, lifecycle runtime workers,
  Chrome multiplex wrapper + adapter. Known native-adapter in-flight bugs
  (shared input ownership, pre-dispatch tombstones, cancellation races) are
  being fixed separately and are only noted, not counted.
- Method: full read of the in-scope diffs and their call paths. Local test
  execution was **not possible**: this sandbox's cargo registry is offline and
  missing crates the workspace lockfile pins (`image 0.25.10` fails to
  resolve), so every `cargo test` invocation dies at workspace resolution.
  All findings below are by code inspection; each states its trigger.

## Verdict

**Not ready to land as-is.** Two P1s: the Chrome screenshot path routinely
produces results the shared transport refuses end-to-end, and a desktop unit
test asserts a launch flag the code no longer emits (CI-red). Two P2s on
harness parity and error semantics. The channel/authorization core
(token registry, routes, scope binding, revocation, request-id recovery) is
solid — no owner/workspace/session confusion or auth bypass found.

---

## P1 findings

### P1-1. `chrome_screenshot` results exceed the native-channel transport budget; failures are permanent and misguided

- **Where:**
  - `crates/tidebreak-core/src/chrome_computer_use.rs:71` — `MAX_CHROME_SCREENSHOT_PNG_BYTES = 8 MiB`.
  - `crates/tidebreak-server/src/code/chrome/runtime.rs:1030` — screenshot accepts up to that 8 MiB and returns the original base64 unchanged.
  - `crates/tidebreak-desktop/src/computer_runtime_adapter.rs:168` — Chrome results are returned verbatim; unlike the native path (`native_runtime_adapter.rs:453-527` `map_output`), no 1 MiB-image / 2 MiB-frame enforcement happens at the producer.
  - Consumers: `crates/tidebreak-cli/src/computer_use/mod.rs:44-52,341-363` (2 MiB frame cap → `ToolFailed`), `crates/tidebreak-server/src/engine/internal/native_tools.rs:31-35,386-391` (hard 1 MiB per-image cap, **no recompression fallback**).
- **Trigger:** `chrome_screenshot` with default args on an ordinary 1440×900
  viewport of a dense page. `scale` stays 1.0 whenever the viewport is under
  4096 px (`runtime.rs:1012-1019`), so the PNG is frequently 1–3 MiB.
  - Internal engine session: any PNG > 1 MiB → the whole tool call fails.
  - CLI/MCP bridge: PNG > ~1.5 MiB (base64 > 2 MiB frame) → the HTTP body is
    refused mid-stream; the bridge's downscale-and-re-encode path
    (`fit_png_to_budget`) never runs because the frame never arrives.
- **Impact:** an advertised tool fails end-to-end for common pages, worse on
  the internal engine than external harnesses (1 MiB vs ~1.5 MiB threshold).
  The failure is sticky: the multiplexer journal
  (`computer_runtime_adapter.rs:65-71`) re-serves the oversized stored result
  for the same request id, so the bridge's transport retry hits the same
  refusal. The error guidance is wrong for Chrome — both consumers say
  "scope it to one app with app_id", which is a native-tool concept; the
  actual remedy is `max_width`/`max_height`.
  Secondary: when the CLI *does* recompress (1–1.5 MiB PNGs), the delivered
  pixel dimensions silently differ from what Chrome captured, and the result
  `data` carries no width/height for the model to notice (low impact only
  because Chrome actions are node_ref-based, not pixel-based).
- **Minimum fix:** enforce the shared budget at the producer — in
  `ChromeComputerUseService::screenshot`, pick `clip.scale` (or downscale and
  re-encode) so the encoded PNG fits `NATIVE_IMAGE_MAX_BYTES` (mirror the
  native adapter's `map_output` frame check in `DesktopComputerRuntime` for
  Chrome results as defense in depth), and make the oversized-capture
  guidance channel-aware.

### P1-2. Desktop unit test asserts a Chrome launch flag the code no longer emits (CI-red)

- **Where:** `crates/tidebreak-desktop/src/chrome_runtime_adapter.rs:1015` —
  `assert!(args.contains(&"--new-window".into()))` in
  `managed_arguments_keep_browser_security_and_visibility`, vs
  `managed_arguments` at `chrome_runtime_adapter.rs:805-816`, which emits
  `--no-startup-window` (the startup-nonintrusion change) and no
  `--new-window`.
- **Trigger:** `cargo test -p tidebreak-desktop --locked` — the CI `desktop`
  job runs exactly this on ubuntu (`.github/workflows/ci.yml:440-482`). The
  test module has no platform cfg. Deterministic failure. (Verified by
  inspection only; this sandbox cannot compile the workspace — see header.)
- **Impact:** branch cannot go green; also records an unresolved intent
  conflict between the old visible-window launch and the new background
  startup that the consent copy ("start and control Chrome in the
  background") promises.
- **Minimum fix:** update the assertion to `--no-startup-window` (keep the
  nonintrusive behavior). Note the actual macOS behavior — whether
  `Target.createTarget` on a windowless managed Chrome raises a window or
  steals focus — is unverifiable on Linux and should be covered by the macOS
  acceptance evidence.

---

## P2 findings

### P2-1. Grok: native channel minted and env injected, but never surfaced to the engine

- **Where:** `crates/tidebreak-server/src/code/runtime/workers.rs:286-310`
  mints the capfile for every harness when the runtime is available;
  `crates/tidebreak-harness/src/grok/session.rs:629` injects
  `TIDEBREAK_NATIVE_CAPFILE` into the child. But Grok print-mode has no MCP
  channel, and its prompt-instruction composer covers only the *browser* CLI
  fallback (`grok/session.rs:448-510`); nothing tells the engine that
  `tidebreak computer <tool> --json … [--output …]` exists.
- **Trigger:** start a Grok code session on a native-capable host; the model
  has no native computer-use tool surface, while Claude/Codex/OpenCode get
  the full MCP set.
- **Impact:** ADR 93's qualification — "Every supported harness receives the
  intended tool set and actual image content" — is unmet for Grok. The direct
  CLI `--output` image path, which the CLI module documents as existing
  precisely for Grok's `read_file` vision flow
  (`crates/tidebreak-cli/src/computer_use/mod.rs:12-16`), is unreachable.
  Also leaves live native authority (capfile + env) in a session whose tool
  surface doesn't disclose it; per-app desktop consent still gates actual
  use, so this is a parity/consistency gap, not an auth hole.
- **Minimum fix:** extend Grok's instruction composer with the computer CLI
  fallback (mirroring the browser block, including the `--output` +
  `read_file` handoff), or skip minting the native channel for Grok until
  that exists.

### P2-2. Request-budget exhaustion on the Chrome path maps to 501/"configuration required"

- **Where:** `crates/tidebreak-desktop/src/computer_runtime_adapter.rs:57-59`
  — `ChromeJournal::begin` returns `NativeRuntimeError::Unsupported(...)`
  when a session exceeds `MAX_REQUESTS`; the route maps `Unsupported` to
  `501 not_implemented` (`crates/tidebreak-server-api/src/routes/code/native.rs:160-162`),
  and both clients map 501 to `ConfigurationRequired`
  (`cli mod.rs:402`, `native_tools.rs:286`).
- **Trigger:** a long session's 4096th Chrome request.
- **Impact:** the model/user is told the host "does not support" Chrome
  control (a configuration problem) when the real state is a per-session
  budget exhaustion; the native adapter reports the identical condition as
  `Failed` with an actionable message (`native_runtime_adapter.rs:189-196`).
  Inconsistent taxonomy misleads recovery (a bridge may downgrade the whole
  channel instead of suggesting a new session).
- **Minimum fix:** return `NativeRuntimeError::Failed` with the same
  "start a new coding session" text the native adapter uses.

---

## P3 notes (concrete, lower severity)

1. **Nested-iframe click offset is one level deep.**
   `crates/tidebreak-server/src/code/chrome/runtime.rs:1077-1098` — probe
   coordinates for a non-root frame are offset by that frame's owner box
   only; a target inside an iframe-within-an-iframe gets coordinates missing
   the outer offset → misclick. Rare layout, real page state; fix by walking
   owner frames to the root.
2. **Journaled Chrome results retain full screenshot payloads in memory.**
   `computer_runtime_adapter.rs:24,65-71` keeps the last 16 results per
   session including image base64 (~10.7 MiB each at today's 8 MiB PNG cap →
   ~170 MiB/session worst case; still ~21 MiB after the P1-1 fix). Consider
   stripping images from journaled results and answering image-bearing
   duplicates as expired/unknown.
3. **Direct CLI silently drops images after the first.**
   `crates/tidebreak-cli/src/computer_use/mod.rs:880-896` — `--output` writes
   only `images[0]`; a multi-image result (`map_output` permits several)
   loses the rest without a note, contrary to the channel's
   no-silent-truncation stance. Emit `image_count`/a note naming dropped
   images.
4. **`recent_events()` clones the whole 4096-event history per diagnostics
   call** (`crates/tidebreak-server/src/code/chrome/cdp.rs:582-585`, up to
   ~4 KiB text each). Fine for now; consider a bounded filtered copy.
5. **Blocking file I/O (write + fsync) under the registry mutex on the async
   runtime** — `crates/tidebreak-server/src/code/native_channel/mod.rs:128-155`
   `issue()` is called from async session spawn. Mirrors the browser channel;
   acceptable, but worth a `spawn_blocking` if session creation latency shows.
6. **Interrupt semantics for Chrome:** an explicit turn interrupt
   (`crates/tidebreak-server/src/code/runtime/turns.rs:471-486`) cancels the
   session's Chrome stop token; resuming requires a fresh `chrome_connect`
   native consent dialog (`chrome_runtime_adapter.rs:299-301,360`). That
   satisfies decision 93's "interrupt releases ownership" but sits in tension
   with the adjacent comment that session capabilities "remain live for later
   turns" — every interrupt costs a modal. Flagging as a deliberate-looking
   choice to confirm, since it borders the cancellation work in flight.
7. **Consent-dialog volume bound:** a looping model can raise up to 128
   `chrome_connect` consent dialogs per session
   (`chrome_runtime_adapter.rs:36,333`) plus a focus dialog per
   `chrome_activate_tab`. Bounded but fatiguing; consider a per-session
   denial backoff.

## Known-bug adjacency (noted, not counted)

- The native adapter records request ids only at completion
  (`native_runtime_adapter.rs:151-163` `store()`), relying on the per-session
  gate for in-flight dedup; a dropped in-flight *acting* future is journaled
  Unknown only via the select-cancel branch — the pre-dispatch-tombstone and
  cancellation-race work already claimed by root covers this.
- `cancel_session` / `cancel_native_input` latch the **global** executor Stop
  (`client_execution/computer_use.rs` `cancel_native_input`) for a
  single-session interrupt — the shared-input-ownership fix root is making.

## What checks out (verified by reading the full paths)

- **Token/scope integrity:** capfile carries only version/endpoint/token;
  subject derived exclusively from the token registry; route re-validates
  session lifecycle + workspace binding per call
  (`routes/code/native.rs:81-116`); desktop adapter binds the first
  token-validated `{owner,workspace}` per session id and rejects mismatches
  forever; revocation tombstones on both native and Chrome sides; reissue
  holds the registry lock across write+commit; startup deletes stale capfiles
  synchronously.
- **Retry/replay:** the single transport retry reuses the request id; exact
  duplicates recover stored results; evicted duplicates answer
  unknown-outcome instead of replaying; id reuse with different arguments
  conflicts (order-insensitive fingerprints); Chrome mutating calls get a
  pre-dispatch Unknown receipt.
- **Endpoint hygiene:** both capfile clients enforce loopback http,
  exact `/code/native` path, explicit port, no credentials/query/fragment;
  CDP endpoints are host-derived, loopback-validated, and
  `DevToolsActivePort` parsing refuses symlinks, foreign owners, and
  non-`/devtools/browser/` paths; managed Chrome runs in an owner-private
  0700 temp profile, own process group, killed as a group on drop; no
  headless/no-sandbox/cert-bypass flags.
- **Image integrity (native path):** the desktop adapter enforces
  1 MiB/2 MiB at the producer with *no scaling*, so capture metadata
  (`width`/`height`/`coordinate_frame`) always describes the delivered
  pixels; both consumers validate PNG magic/dimensions and keep base64 out of
  model text and journals.
- **CDP correlation/eventing:** globally unique command ids make flat-session
  reply matching safe; event history is bounded (4096) and truncated (4 KiB);
  diagnostics filter to the tab's session id; frame trees are re-checked
  before and after snapshot/screenshot/act, and one snapshot authorizes at
  most one action.

## Unverified paths

- Anything requiring macOS: the Swift helper (`Capture.swift`,
  `Control.swift`), broker consent UI behavior, actual managed-Chrome window/
  focus behavior, `open -g` nonintrusion, coordinate correctness of real
  captures. Linux sandbox; no claims made.
- `crates/tidebreak-host-broker/*` (broker protocol/blocklist/helper) — out
  of assigned scope; the executor's use of it was read, its internals were
  not.
- CLI `browser.rs` lifecycle/ghost-cursor/DOM work and the browser routes —
  assigned to other workers.
- `tests/chrome_real.rs`, the smoke/probe scripts, and all Rust tests: not
  executed here (offline-registry sandbox); route/unit tests were reviewed as
  text only.
