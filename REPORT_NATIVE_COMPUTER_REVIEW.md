# Native computer use — final pre-ship review (read-only)

Scope: `origin/main...thet/computer-use-parity` (~27k added lines), focused per the
review brief on `tidebreak-core/src/computer_use.rs` (schemas/mode contract), the
host-broker protocol/backend/policy (`protocol.rs`, `broker.rs`, `computer_use.rs`,
`blocklist.rs`, `consequential.rs`, `set_of_marks.rs`), the Swift helper
(`Control.swift`, `Capture.swift`, `Helper.swift`, `Windows.swift`, `AXTree.swift`),
and the desktop integration (`client_execution/computer_use.rs`,
`native_runtime_adapter.rs`, `computer_runtime_adapter.rs`, `computer_use_action.rs`,
`native_channel/`, `routes/code/native.rs`, `engine/internal/native_tools.rs`).
All of these were read in full, not from summaries.

Excluded per brief (already owned by in-flight local work): the WK shared
foreground gate and the Chrome browser-capability Stop remint fix
(`chrome_runtime_adapter.rs` internals were only skimmed at the multiplex seam).

Environment limits: this sandbox is offline and the branch's new `tidebreak-cli`
dependency (`image = "=0.25.10"`, correctly pinned to the lockfile) is absent from
the offline cargo cache, so **no Rust unit tests could be executed here**; the
findings are from code reading. No macOS validation was performed or is claimed.

## Verdict

The core invariants named in the brief all hold as implemented, with tests
covering each one (details in "Invariants verified" below). No P1 findings.
One P2 (in-diff, functional not security) and several P3s, two of which are
pre-existing on `main` but sit squarely in the surface being shipped.

---

## Findings

### P2-1 — The agent-cursor "running" activity is dead wiring: no producer ever emits it

*In this diff. Functional gap, not a safety hole.*

- `crates/tidebreak-desktop/src/computer_use_action.rs:156` (`activity_for_call`)
  builds a `ComputerUseActionEvent` with `phase: Running`, but **no call site ever
  emits it**. The only `emit_computer_use_action` caller in the tree is
  `finish_call_activity` (`computer_use_action.rs:226`), which emits a single
  terminal event with `visible_until_millis = now + 1_500`.
  Producers: `client_execution/computer_use.rs:1026`,
  `computer_runtime_adapter.rs:274`, `browser_runtime_adapter.rs:145` — all
  create-then-finish, never emit the running phase.
- No producer ever sets `point` / `viewport` / `target_bounds` / `capture_id`
  either. `AgentCursorOverlay.tsx:22-53` (`agentCursorPosition`) returns `null`
  whenever `point` or `viewport` is absent, and additionally requires
  `preview.windowId && preview.captureId` for native. So the cursor overlay can
  **never render in the product** — only in its DOM tests and Storybook, whose
  fixtures hand-craft geometry.
- The UI reducer (`computerUseAction.ts:120-127`) contains logic specifically to
  keep a terminal phase from being overwritten by a late `running` event — logic
  that is unreachable given no running events exist.

**Repro/trigger:** run any native, Chrome, or in-app-browser action; observe that
the only `computer-use-action` event is the terminal one, and the cursor overlay
never appears.

**Why it matters:** the branch's stated goal includes truthful, visible background
activity; as wired, the live-cursor affordance ships as dead code and the status
chip only flashes for 1.5 s after the fact. Either emit the running event (with
geometry where known) or delete the overlay + running plumbing so the next reader
doesn't trust a surface that cannot fire.

### P2-2 — A tree read silently renumbers the mark table out from under the last annotated screenshot

*Pre-existing on `main`; flagged because it is core to the surface being shipped.*

- The model contract says a `mark` is "a number from the last annotated
  screenshot" (`tidebreak-core/src/computer_use.rs:272-274`). But
  `client_execution/computer_use.rs` `map_result` → `CuReadAppContent`
  (lines 1855-1866) replaces the `(chat, app)` mark table with marks re-extracted
  from the fresh tree — and `computer_read_app_content` accepts model-chosen
  `max_depth`/`max_nodes`. A narrower read yields a smaller candidate set, so
  reading-order numbering shifts relative to the badges the model saw.
- `resolve_mark` then returns the *new* table's entry for "mark N" — a live,
  self-consistent `(element_id, fingerprint)` pair — so the helper's act-time
  fingerprint re-check **passes** and the click lands on a different element than
  badge N on the screenshot. The stale-element defense cannot catch this because
  the staleness is in the number→element binding, not the element itself.

**Repro/trigger:** `computer_capture_screen` (annotated) → note badge N →
`computer_read_app_content` with `max_nodes: 100` → `computer_click {mark: N}`
acts on whatever the truncated tree numbered N. The consequential gate still
guards send/delete-labeled targets, so the blast radius is a wrong benign click
inside the granted app — but it silently violates the documented mark semantics.

**Suggested direction:** only let annotated captures write the mark table, or
version the table (capture id) and refuse a mark minted by a different snapshot,
the way `stale_mark` already refuses cross-app/cross-chat marks.

### P3-1 — Capture badges 81–100 are drawn but unactionable by number

*Pre-existing on `main`.*

`MAX_CAPTURE_MARKS = 100` (`host-broker/src/broker.rs:103`) exceeds the model
contract's `MAX_MARK = 80` (`tidebreak-core/src/computer_use.rs:60`), which
`target_is_well_formed` enforces. A dense screen produces badges 81–100 in the
image and in the `marks` result array, but `computer_click {mark: 85}` fails core
validation with the generic "The computer-use request was not available."
Recoverable via `element_id`, but a confusing dead end. Clamp
`MAX_CAPTURE_MARKS` to `MAX_MARK` (or raise `MAX_MARK`).

### P3-2 — Unfiltered `cu_list_windows` discloses blocked apps' window titles, contradicting its own comment

*Pre-existing on `main`.*

`broker.rs` `cu_list_windows` (line 2642) authorizes a screen-wide listing off
any app-scoped read-or-better grant, with the comment "the listing adds only
other apps' titles, which the blocklist still protects." Nothing enforces that:
the helper's `Windows.list(bundleId: nil)` enumerates every on-screen window
including `com.apple.keychainaccess` / Tidebreak's own, the broker passes the
result through unfiltered, and the desktop's `map_result` only truncates to 64
rows. Window titles can carry sensitive content (document names, subjects).
Either filter `is_blocked_control_bundle` owners out of the unfiltered listing
or correct the comment so the disclosure is a documented decision.

### P3-3 — Stop latch treats native reads and Chrome reads differently

*In this diff.*

After the user presses Stop, native observation ops (capture / read_app_content /
list_windows / wait-condition) still execute — `dispatch_broker` only
short-circuits `acts_on_host` calls — while the Chrome path refuses **every**
tool when halted (`computer_runtime_adapter.rs:251-253`). Both behaviors are
defensible alone; together they are inconsistent, and a user who pressed Stop may
not expect screenshots to keep flowing to the provider. If intentional, a
comment/doc note on `dispatch_broker` would prevent the next reader "fixing" it
in either direction.

### Minor observations (no action required to ship)

- `Control.swift:416`: the error on the foreground scroll fallback path reads
  "background scrolling requires element_id or x/y" but is only reachable in
  foreground mode (when `CGEvent(source:)` fails). Message nit.
- `Control.swift:273-279`: foreground `type_text` AXValue set reports success on
  the AX return code alone; the background path additionally reads the value back
  to verify retention. Harmless asymmetry, but the verification would be equally
  cheap in foreground.
- `key_press_needs_confirmation` confirms **every** chord, including pure
  navigation chords (Cmd+F, Cmd+A). Deliberate per the decision-0013 calibration,
  but worth watching for over-ask fatigue in dogfooding.

---

## Invariants verified (with the load-bearing code)

1. **Background default never secretly foreground.** Absent `execution_mode`
   deserializes to `Background` at every layer (core schema default, broker wire
   `#[serde(default)]`, helper `executionMode != .foreground` branches). The
   helper's single `post()` for synthesized events carries
   `precondition(request.executionMode == .foreground)` (`Control.swift:1217-1221`),
   and every background path is AX-only (`AXPress`, `AXValue`, AX scrollbar) or
   refuses with `requires_foreground`. `key_press`, `hover`, `drag`,
   `focus_window`, and `return_to_tidebreak` hard-require foreground; focus and
   return refuse the background default *before* any broker round-trip
   (`build_action`, `client_execution/computer_use.rs:1199-1266`). The desktop
   maps broker `RequiresForeground` to a non-retryable refusal, never a consent
   card (test at `computer_use.rs:3074`).

2. **App grant vs foreground consent.** Foreground is a separate per-(chat, app)
   host-side approval (`foreground_takeovers`, `computer_use.rs:210`), populated
   only by the trusted native dialog in `ensure_foreground_takeover`, cleared by
   Stop, never restored by Resume (test at line 3057). A `ControlApp` grant
   alone can never produce a foreground dispatch.

3. **Real shared input owner.** All acting dispatches — chat executor,
   session-native, confirmations, return-to-Tidebreak — serialize through the
   single `acting_dispatch` gate with an owner recorded in `DispatchState`;
   Stop either lands before dispatch (and prevents it) or after it completed
   (tests at `computer_use.rs:2423-2593`). A queued session's cancel cannot
   signal another owner's helper (test at 2596).

4. **Cancel helper before future drop.** `stop_session` writes the "stopped"
   generation while holding the dispatch-state lock, before any waiter wakes;
   `await_session_operation` retains a cancelled owner's future until the broker
   round-trip completes (`native_runtime_adapter.rs:496-522`, drop-order test at
   845). The helper re-reads the generation file before every synthesized event
   (`ensureNotCancelled`, per-keystroke / per-drag-step / per-poll).

5. **Stop revision defeats old Resume.** `resume_computer_use_control` snapshots
   `stop_revision` before showing the dialog; `resume_with` refuses on any
   mismatch, and a Resume waiting on the dispatch gate is invalidated by a new
   Stop (test at `computer_use.rs:2643`). Resume also rotates the helper
   generation, so pre-Stop in-flight input can never ride a new generation.

6. **No uncertain replay.** Chat receipts: an interrupted `DispatchStarted`
   receipt terminalizes as `computer_use_interrupted` instead of re-firing
   (`execute_receipt`, line 785). Session-native: request ids are reserved with
   an `Unknown` tombstone before dispatch, evicted exact repeats answer
   unknown-outcome, id reuse with different arguments conflicts, and the budget
   exhausts closed (`native_runtime_adapter.rs`, tests at 705-831). The engine
   client only retries transport failures with the *same* request id and answers
   unknown-outcome by fetching the stored record (`native_tools.rs:140-153`).

7. **Screenshot provenance coordinates.** The helper reports the capture crop as
   a global top-left logical `coordinate_frame`; mark badges are drawn with the
   exact inverse of the mapping the desktop's `coordinate_note` documents
   (`Capture.swift pixelRect` vs `finish_capture` note), and downscaling scales
   both uniformly. Display-scoped captures pin `sourceRect`/`destinationRect` so
   SCK cannot zoom and break the mapping. Handoffs are single-use, staged
   owner-only, evicted with their files, and never cross the agent channel.

8. **Confirmation integrity.** A held consequential action is re-authorized at
   confirm time (blocklist, live grant), its element re-described with label +
   fingerprint match plus a defense-in-depth re-classification, and it keeps the
   held execution mode — a confirmation can never escalate background to
   foreground (`perform_confirmed_action`, broker.rs:2036-2126).

9. **Session-native channel security.** Per-session bearer tokens in 0600
   capfiles, atomic reissue under the registry lock, loopback-only endpoint
   validation, scope binding first-triple-wins with mismatches rejected forever,
   revocation tombstones + broker subject purge, and image/frame budgets enforced
   on both producer and consumer sides.

The blocklist (dotted-boundary matching, helper mirror, blocked-before-consent
ordering), the consequential lexicon, the mark scoping (per chat × app, refused
cross-chat/app — `stale_mark`), and the platform gating (macOS 14+, codesign
verify before advertising the runtime) are all consistent between layers and
covered by tests.

---

*Review conducted read-only on `thet/computer-use-parity` @ `c4cfb0a1`; no code
changes were made. Unit tests could not be executed in this offline sandbox (the
new `image` dependency is absent from the local cargo cache); CI remains the
authority for green.*
