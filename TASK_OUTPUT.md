# TASK_OUTPUT — native computer use across coding harnesses

Branch: `thet/cu-harness-service` (no PR opened; parent reviews/integrates)
Final SHA: `334d1e6847fd7f04600f987877f7bfd82815b89c`

Commits, in order:

1. `83422087` — server-core native channel completion (registry endpoint
   `/code/native`, `NativeChannelBinding` + bind-glue threading, decoupled
   `native_bridge_command`, `is_available` gating at mint time) and
   server-api `/code/native/execute` + `/code/native/result` routes with the
   token-derived subject ladder, duplicate-recovery, conflict, and
   unknown-outcome mapping, plus `tests/code_native.rs`.
2. `ce4b8451` — CLI `computer` / `computer-mcp` (dynamic tool set from
   `computer_use_tool_specs()`, real MCP image blocks, 1 MiB decoded /
   2 MiB frame budgets with one bounded downscale, `--output PATH` private
   PNG for Grok's `read_file` vision path) and tb-native MCP injection for
   Claude, Codex, and OpenCode.
3. `99c8ae83` — desktop `native_runtime_adapter` over the shared
   computer-use executor (inline `CaptureDelivery`, session context helper,
   grant purge on revoke), server interrupt → `cancel_session` plumbing,
   internal-engine native toolset over the token-scoped loopback routes.
4. `334d1e68` — review fixes: first-triple scope binding +
   `validate_scope` for the parent's wrapper, tombstoned request dedup
   (evicted-exact → unknown, budget exhaustion refuses), acting-aware
   cancel through the shared `cancel_native_input` Stop latch (no
   auto-resume), process-exit shutdown hook, 1 MiB / 2 MiB result budgets
   in `map_output` (no scaling, so capture metadata always matches
   delivered pixels), streamed capped reads in the internal engine, and
   the `StoredResolution::Cancelled` → `computer_use_cancelled` arm.

## Checks run here (Linux gVisor sandbox)

- `cargo check`: clean for `tidebreak-server-core`, `tidebreak-server`
  (server-api), `tidebreak-cli`, `tidebreak-harness`.
- `cargo fmt --check`: clean for all touched crates including desktop.
- `cargo test -p tidebreak-cli computer_use` — 14/14 pass.
- `cargo test -p tidebreak-harness` — 358 pass; 1 pre-existing failure in
  `child::unix_process_tree_tests::cancelling_output_collection_kills_a_pipe_owning_descendant`
  (process-tree kill semantics under gVisor; untouched code).
- `cargo test -p tidebreak-server-core native` and `engine::internal` —
  all pass (native_channel, native_tools, adapter-side units).

## Exact blockers in this sandbox — run these on macOS

1. **Loopback HTTP is blocked** (known gateway-sandbox limit; even the
   pre-existing `code_browser` route tests fail on connect timeout). Run:
   - `cargo test -p tidebreak-server tests::code_native`
   - `cargo test -p tidebreak-cli browser` (pre-existing suite, same cause)
2. **Desktop crate cannot build**: `glib-sys v0.18.1` build script fails
   (no pkg-config/GTK dev headers, no root to install). Run:
   - `cargo check -p tidebreak-desktop`
   - `cargo test -p tidebreak-desktop native_runtime_adapter client_execution::computer_use`
   The desktop code was written against verified APIs but has never been
   type-checked here; expect the first macOS compile to be the real gate.

## Remaining integration notes for the parent

- `cancel_native_input` in `client_execution/computer_use.rs` is the single
  shared cancellation seam: it latches the executor Stop and drains the
  acting dispatch. Insert `BrokerClient::cancel_native_actions()` there (or
  in `ComputerUseState::halt`) so every path — human Stop, session
  interrupt, revoke-with-work-in-flight, shutdown — cancels long
  drags/types broker-side. Nothing in this branch auto-resumes; resume is
  only the trusted `resume_computer_use_control` path.
- Cancellation granularity: a session's cancel only latches the global Stop
  when that session's own *acting* operation is live
  (`in_flight_acting`); idle/queued/read-only cancels just bump the
  generation. True per-input-owner cancellation needs the broker-side
  owner handle the helper worker is adding.
- The internal engine (`engine/internal/native_tools.rs`) and CLI still use
  `computer_use_tool_specs` / `validate_computer_use_arguments` module
  paths; cherry-pick your combined
  `computer_session_tool_specs`/`validate_computer_session_arguments` over
  them. The tool lists are fully dynamic, so new primitives (launch,
  hover, drag, resize, condition waits) flow through untouched.
- `DesktopNativeRuntime::validate_scope` is `pub(crate)` for the
  `computer_runtime_adapter` wrapper; the concrete
  `Arc<DesktopNativeRuntime>` is also in Tauri state (`app.manage`) for
  the exit hook and your wrapper. Chrome names and multiplexing were left
  entirely to you.
- Internal-engine **browser** tools remain unwired (`spec.browser` is still
  unused by the internal engine): browser-specific work belongs to the
  browser-owning worker per the ownership split. The native half no longer
  ignores its spec.
- The `execution_mode: background|foreground` / ghost-cursor schema was
  deliberately not modeled here; the adapter adds no activation of its own
  and reuses the canonical executor, so your wire-schema addition composes.
- Consent copy (`computer_use_consent_message`) was left untouched per your
  instruction; `native_consent_choice` unmodified.
- Qualification items that need real macOS hardware (packaged evidence,
  end-to-end click/type/drag flows, Chrome debugging adapter) are outside
  this branch by assignment.
