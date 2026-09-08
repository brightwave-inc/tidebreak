# Task report: bounded Chrome cancellation safety (`thet/cu-chrome-cancel`)

Scope: `crates/tidebreak-server/src/code/chrome/runtime.rs` plus focused tests
in `crates/tidebreak-server/src/code/chrome/tests.rs`. No desktop adapter
edits. This file is a hand-off note for the integrating PR; drop it before
merge.

## Changes

1. **Bounded down/up cleanup (`InputHold`)** — every `keyDown`,
   `mousePressed`, and drag press arms a release guard *before* the down
   command is issued, and disarms only after the paired release succeeds. On
   error, fence cancellation, or the dispatch future being dropped by the
   desktop wrapper's `select!`, dropping the guard spawns one bounded task
   (per-command 2 s timeout, at most two commands) that sends the
   compensating `keyUp` / `mouseReleased` / `Input.cancelDragging` directly
   on the transport. Arming before the down covers the race where the down
   was written to the wire but its response was cancelled; the transport's
   ordered command queue guarantees a discarded down never reaches Chrome
   after its release. Drag cleanup releases at the press origin so it can
   never complete the drop. No replay, no unbounded work.
2. **Snapshot consumption on every attempted action** — `act` now consumes
   the stored snapshot immediately after the initial staleness check, before
   the first side effect, so a cancelled or dropped call can never leave a
   snapshot behind that authorizes a second action (previously the
   invalidation ran after the operation and was skipped when the future was
   dropped).
3. **Screenshot producer fitting** — captures now default to a 1440 px
   largest dimension and re-capture at a smaller `clip.scale` (bounded, 4
   attempts) until the PNG fits the shared transport budget
   (`MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES` = 1 MiB decoded,
   2 MiB encoded frame). The result data now reports delivered `width` and
   `height`. The native wrapper's ceiling stays as defense in depth.
4. **Nested-frame probe offsets** — the snapshot frame walk records each
   frame's embedding parent; probe now sums the frame-owner offset of every
   ancestor up to the top frame (owner queried in the parent frame's own CDP
   session), and refuses with a typed error when the chain to the top frame
   cannot be resolved, instead of risking a misplaced click.

## Tests (all in `code::chrome::tests`, scripted fake CDP server)

- `failed_key_down_still_releases_the_key_and_consumes_the_snapshot`
- `cancelled_key_down_response_still_releases_the_key`
- `dropped_act_future_releases_the_button_and_consumes_the_snapshot`
- `failed_drag_cancels_dragging_then_releases_at_the_origin`
- `completed_press_releases_the_key_exactly_once`
- `screenshot_refits_capture_scale_to_the_transport_budget`
- `nested_frame_click_sums_every_ancestor_offset`

`cargo test -p tidebreak-server-core --lib code::chrome`: 13 passed
(7 new + 6 pre-existing). `cargo clippy -p tidebreak-server-core --lib
--tests`: clean for this module (also fixed the pre-existing
guard-across-await warning in the tests file). The fake CDP server is the
module's in-process channel transport; this sandbox's loopback is restricted
(15080 is the egress proxy), so no real-socket fixture was added — the
ignored `tests/chrome_real.rs` lane still covers the real transport.

## Notes for review

- Nested-frame offsets assume CDP box-model coordinates are relative to the
  embedding frame per hop. If real-Chrome verification (Mac lane) shows
  same-process quads are already top-viewport relative, the ancestor loop
  should stop after the first hop for same-session frames; the refusal path
  for unresolved chains should stay either way.
- Cleanup releases intentionally bypass the fence (like the old drag cancel
  path): releasing held input is neutralization, not exercised authority.
