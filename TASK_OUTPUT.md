# Task report: internal-engine in-app browser channel wiring

- Branch: `thet/cu-internal-browser` (from `75e49391`)
- Implementation commit: `1d1285540dae541fbc64fc40e6d882393bfc695e`
- Branch head (this report): `48ef37a545813b2b4c64551734f5772b9ff15826` + this fixup
- Package: `tidebreak-server-core`

## What changed

The internal engine consumed `spec.native` but ignored `spec.browser`; a code
session on the internal harness therefore had no in-app browser tools while
every external harness got the MCP bridge. Files owned by this task:

- `crates/tidebreak-server/src/engine/internal/browser_tools.rs` (new) —
  session-scoped Rust `Tool` adapter over the issued `BrowserChannelSpec`
  capability file and the existing loopback `/code/browser` routes.
  - Capfile read is bounded (64 KiB, regular file), version-1 only,
    `deny_unknown_fields`; endpoint must be a loopback HTTP URL with an
    explicit port and exact path `/code/browser` (no credentials, query, or
    fragment); token must be canonical `tbreak_bt_<UUID>`. Fails closed; no
    token or endpoint in error text; the client never derives `Debug`.
  - Tool set from canonical `tidebreak_core::browser` specs and validators:
    `browser_list`, `browser_navigate`, `browser_snapshot`, `browser_wait`,
    `browser_screenshot` always; `browser_act` gated on capfile
    `semantic_actions`; `browser_open`/`browser_close`/`browser_activate`
    gated on `lifecycle`; `browser_diagnostics` gated on
    `developer_diagnostics`. Approval classes match the CLI bridge
    (navigate/act/open/close/activate sensitive, the rest read-only).
  - Transport: bounded response frames per route (64 KiB–4 MiB; error bodies
    8 KiB), one retry after transport failure for read-only operations only;
    a state-changing operation with an unknown outcome refuses with guidance
    to re-snapshot before acting again. Server error messages are scrubbed
    (bearer patterns and UUIDs redacted). Status mapping mirrors the native
    adapter (400/409/422 invalid-arguments, 401/403/501
    configuration-required, 404 not-found).
  - Screenshots decode into real `ToolOutput` image blocks
    (`ImageRef`/`ImageData`) within `MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES`;
    base-64 pixels never enter model text or structured data; oversized or
    mislabeled captures are refused with `max_width`/`max_height` guidance.
- `crates/tidebreak-server/src/engine/internal/session.rs` — launch now
  builds the session tool surface from both channels via
  `build_session_tools(spec.native, spec.browser)`; an unreadable or
  untrusted channel fails the launch instead of silently dropping tools.
  Tools stay session-scoped: they are merged into the per-turn registry by
  the existing `LegDriver::run_turn` seam; no global registry is touched.
- `crates/tidebreak-server/src/engine/internal/mod.rs` — registers the new
  private module.

`native_tools.rs` was not modified.

## Tests

Command: `cargo test -p tidebreak-server-core engine::internal::`
(28 passed, 0 failed). New tests and results on this machine (Linux):

- `engine::internal::browser_tools::tests::the_toolset_tracks_the_capfile_capability_flags` — ok
- `engine::internal::browser_tools::tests::a_malformed_capfile_fails_launch_without_leaking_secrets` — ok
- `engine::internal::browser_tools::tests::only_read_only_operations_retry_after_a_transport_failure` — ok
- `engine::internal::browser_tools::tests::screenshots_decode_into_image_blocks_and_never_into_text_or_data` — ok
  (real PNG bytes through the actual decode path; asserts the image block,
  hydrated pixels beside the `ImageRef`, no base-64 in text, no data payload,
  budget and mime-mismatch refusals)
- `engine::internal::browser_tools::tests::the_channel_round_trips_results_and_images_over_loopback` — ok,
  but **skipped its live section in this sandbox** (see below)
- `engine::internal::session::tests::session_tools_come_from_both_channels_and_fail_closed` — ok

Also run: `cargo check -p tidebreak-server-core --tests` (clean),
`cargo clippy -p tidebreak-server-core --tests` (no warnings in the files
this task owns; two pre-existing warnings elsewhere), `cargo fmt`.

### Blocked locally

The live loopback test (fake axum `/code/browser` server exercising bearer
auth, typed round-trips, revoked-token 401, oversized-capture refusal, and
screenshot-to-image-block transport) could not bind a routable port in this
sandbox: the network policy routes exactly one loopback port (15003) and a
sibling suite held it for the whole session, while ephemeral loopback ports
accept binds but not connections. The test binds ephemeral-first (which CI
routes normally), falls back to 15003, and skips with an explicit stderr
message when neither is reachable — so it runs fully in CI and never
false-fails in the sandbox. The decode path it shares is covered by the
non-network image test above.

## Caveats

- This is a Linux sandbox run; nothing here claims macOS native or packaged
  acceptance.
- The comments reference decision 94 (computer use for coding harnesses) per
  the parent's rebase; this branch itself still carries the doc as 0093 and
  cherry-picks cleanly onto the rebased parent.
