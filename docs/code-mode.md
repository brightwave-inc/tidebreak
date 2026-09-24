# Code mode

Status: supported and enabled by default. Code mode's decision records start at
[`0030`](decisions/0030-code-mode-separate-surface.md) and continue through the
current numbering — [`docs/decisions/`](decisions) is the live list, and each
record carries its own status line. This page carries the working design detail in one place.
Where this page and a decision record disagree, the record wins. The first
version ships the full surface described here — repos, workspaces, sessions,
approvals, checkpoints and review, auxiliary terminals, the git/PR flow, the
updates channel, and adapters for Claude Code (reference tier), Codex CLI,
opencode, and Grok CLI (the pinned 1.0.13 adapter uses ACP and structured
approvals for Auto, Ask, and Allow, and refuses Plan — see
[`0038`](decisions/0038-auto-is-a-declared-capability.md) and
[`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md)) — with the
deliberately parked scope recorded in [`docs/deferred.md`](deferred.md).

Code mode is Tidebreak's second product surface and is available without an
experimental opt-in: pick a local git repository,
spin up isolated **workspaces** (one worktree + branch each), and run
**sessions** — durable conversations with external coding-agent engines
(Claude Code as the reference tier, plus Codex CLI, opencode, and Grok CLI) —
supervised through a structured UI: conversation, tool activity, approvals,
per-turn diffs, and a pull-request flow. The two-mode split is a delivery
strategy: the destination is one surface where context selects behavior — a
conversation bound to a workspace behaves code-like, one without is ordinary
chat — and the adapter contract below is the runtime interface every engine,
eventually including Tidebreak's own internal loop, sits behind
([`0030`](decisions/0030-code-mode-separate-surface.md)).

## Vocabulary

| Noun | Meaning |
|---|---|
| repo | A registered local git repository: root path, default base ref, branch prefix, setup/archive scripts, quick actions. |
| workspace | One isolated unit of work on a repo. Owns exactly one git worktree and one branch for life; carries PR state. |
| session | One durable conversation with one engine inside a workspace. A workspace holds several, plus at most one watch session; their turns are serialized on the shared worktree ([`0055`](decisions/0055-multiple-sessions-per-workspace.md)). |
| turn | One user→agent cycle in a session, ending in a checkpoint. |
| engine | The external coding-agent CLI being driven (never "provider"). Internally this is still a `harness`. |

Repository registrations can be exported with MCP server definitions as a
portable workspace configuration. See [Portable workspace configuration](mcp-servers.md#portable-workspace-configuration).

## Crate and module layout

Dependencies flow downward per [`docs/crates.md`](crates.md):
`tidebreak-core` ← `tidebreak-harness` ← `tidebreak-server` ← clients.

```
crates/tidebreak-core/src/code/
  mod.rs         workspace/session domain types and HarnessKind
  event.rs       Event, SequencedEvent
  caps.rs        HarnessCaps, CapLevel, HarnessTier
  external_input.rs, remote_task.rs
crates/tidebreak-core/src/attention.rs       shared chat/code attention types
crates/tidebreak-core/src/id.rs              RepoId, WorkspaceId, SessionId, TurnId,
                                             ApprovalId, CodeTerminalId
  (permission)   PermissionMode { Plan, Ask, Auto, Allow }, shared with chat
                 (decision 48 step 2), and its per-mode contract

crates/tidebreak-core/src/db/entities.rs      24 `code_*` tables plus the shared
                                             session, turn, event, and approval tables
crates/tidebreak-core/src/db/ops/code/        repo.rs, workspace.rs, session.rs,
                                              turn.rs, journal.rs, approval.rs,
                                              watch.rs, queued.rs, trigger.rs,
                                              pull_request.rs, session_image.rs,
                                              recovery.rs

crates/tidebreak-harness/                     protocol translation only
  src/lib.rs       adapter/session contract and shared protocol types
  src/wiring.rs    adapter registry and production wiring
  src/probe.rs     interactive-login shell resolution + env capture (0034),
                   version detection, auth observation
  src/pin.rs       the pinned npm package version per engine; the user's PATH is
                   not the engine (0045)
  src/launch.rs    launch-plan composition and the permission-bypass denylist
  src/child.rs     engine-child bookkeeping: the pid while a turn is in flight,
                   and how the child ended
  src/browser_channel.rs  the capability-file contract adapters hand an engine child
  src/budget.rs    bounded stream-parse budgets
  src/text.rs      byte caps that never split a character
  src/claude/      mod.rs, parse.rs, session.rs, approvals.rs, browser.rs
  src/codex/  src/opencode/  src/grok/        mod.rs, parse.rs, session.rs each
  src/bin/capture.rs                          dev-only, feature = "capture"
  fixtures/<harness>/<version>/               *.ndjson + manifest.toml + *.expected.json

crates/tidebreak-server/src/code/   (the spine, not the whole directory)
  mod.rs             wiring
  runtime/           process-wide adapters, workers, sessions, workspaces, and recovery
  session_worker.rs  per-session task: lease + spawn-epoch, adapter session, event pump
  worktree.rs        git shell-out: repo validation, worktree add/remove/prune/self-heal
  worktree_root.rs   the configured root new worktrees land under (0053)
  clone.rs, clone/external.rs   local and forge-backed clone jobs
  checkpoint.rs      hidden refs, synthetic commits via temp index, bounded diffs
  checkpoint/restore.rs, checkpoint/revert.rs   restore to a checkpoint; revert
                     a file or hunk; discard uncommitted changes
  setup_script.rs    setup/archive hooks, failure-preserves-checkout
  recovery.rs, worktree_orphans.rs   boot recovery, fencing, orphan probe, reap
  attention.rs       server-side attention computation, digest publication
  approval_bridge.rs the loopback approval-prompt endpoint glue and decision routing
  apps_bridge.rs     the loopback connected-apps MCP bridge external engines mount as `tb-apps`
  gh.rs              gh CLI shell-out for repository and pull-request operations
  pr_fetch.rs        pull-request and checks reads through gh
  forge_rest.rs      brokered forge REST operations and auto-merge GraphQL mutation
  ci_logs.rs         check-log retrieval
  pr_facts.rs        post-turn pull-request fact detection and attribution (0062)
  delivery/           install-wide pull-request and run reads, guarded actions
  trigger.rs         the sweep that turns pull-request facts into claimed fires (0060)
  watch.rs           the watch-and-fix sweep (0050)
  fork.rs            a parent transcript written outside the worktree for a sibling agent
  self_drive.rs      internal-engine session tools
  harness_install.rs warm installs of the pinned engine binaries, off the create path
  harness_llm.rs     session-scoped inference relay for engine children (0071)
  browser_runtime.rs the server↔desktop browser adapter boundary (0054)
  terminal.rs        PTY shells, ring buffers, cursor reads (0036)
  bus.rs             per-session broadcast + install-wide updates channel
crates/tidebreak-server-api/src/routes/code/  repos, workspaces, sessions,
                                              session_events, updates, approvals,
                                              git, terminals, harnesses, triggers,
                                              delivery, analytics, usage, browser, llm
crates/tidebreak-server/src/scripted_harness.rs   debug-only fake adapter
                                              (the scripted_provider.rs pattern)

crates/tidebreak-desktop/ui/src/code/         the UI family (below)
```

New dependencies, all exact-pinned and lockfile-matching: one Rust
pseudo-terminal crate (terminals only — the harness crate must not depend on
it, enforced by a dependency check), the terminal-emulator UI package pair,
and the Monaco editor pair (`@monaco-editor/react`, `monaco-editor`) driving the
file viewer and its editor. Nothing else: no git library and no diff library (git
produces diffs; the UI styles them). `@tanstack/react-virtual` virtualizes the
delivery pull-request and workflow-run lists through `code/delivery/VirtualRows.tsx`.

## Data model

A schema change here is an appended migration, per
[`0061`](decisions/0061-schema-changes-are-migrations.md). The tables below
describe the current schema, not the frozen baseline.

- **`code_repo`** — `id`, `owner`, `root_path` (unique, canonical toplevel),
  `display_name`, `default_base_ref`, `branch_prefix`, `setup_script`,
  `archive_script`, `quick_actions` (JSON array of
  `{name, command, auto_run_on_create}`), `created_at`, `removed_at`,
  `cloned_from`.
  `cloned_from` records the remote Tidebreak cloned the checkout from, and is
  null when the user registered a directory that already existed. It is what
  makes `?reclaim_checkout=true` safe: both paths register identically, and
  the clone parent is a setting that moves, so no path test stays honest.
  Only a checkout Tidebreak made is Tidebreak's to delete.

  Removal is soft: `DELETE /code/repos/{id}` stamps `removed_at` and hides the
  registration, keeping every archived workspace and transcript that hangs off
  it reachable. Deleting the row would strand that history on SQLite, which
  does not enforce the workspace foreign key, and fail outright on PostgreSQL,
  which does. Reclaiming the checkout on disk is a separate, explicit act.
- **`code_workspace`** — `id`, `owner`, `repo_id`, `title`, `worktree_path`,
  `branch_name`, `base_ref`, `status`
  (`Creating | SetupFailed | Active | Archiving | Archived | Released`), `pr` (JSON digest:
  number, url, state, checks summary; nullable), `created_at`,
  `archived_at`, `released_at`, `released_tip`, `bundle_bytes`.

  Archive is the single cleanup flow. It removes a local workspace's worktree,
  bundles its commits, drops its branch, and records it as Released; remote
  workspaces remain Archived because their branch lives on the remote runtime.
  Restore recreates a local branch from the bundle. Transcripts are untouched:
  the row and its journal outlive the workspace bytes.

  Non-force archive also protects ignored files. Inspection lists an ignored
  directory as one path, so a generated tree does not exhaust the scan budget.
  To exclude a generated directory from that scan, configure its exact
  repository-relative path with
  `git config --add tidebreak.archiveDisposablePath <directory>`. Archive
  fails closed when the remaining scan or its configured paths exceed the
  safety budget.
- **`session`** — `id`, `owner`, `workspace_id`, `kind`
  (`Interactive | Watch`, per
  [`0050`](decisions/0050-watch-and-fix-is-a-durable-task.md)), `harness_kind`,
  `harness_version` (observed at launch), `harness_resume_ref` (the
  harness's own session/thread id for resume), `permission_mode`, `model`,
  `reasoning_effort`, `fast_mode`,
  `lifecycle` (`Created | Idle | Running | Fenced | Ended`), `fence_reason`
  (JSON, nullable), `child_pid`, `child_process_identity`, `spawn_epoch`,
  `attention_state` (JSON), `attention_source`, `unrecognized_event_count`,
  `subagents` (bounded, per
  [`0052`](decisions/0052-harness-subagents-as-child-rows.md)), `created_at`,
  and the conversation columns `project_id`, `title`, `network_policy`, and
  `attachment_revision`.

  A chat is a session (decision 48 step 5): the row is the conversation row,
  and every chat-side table (`message`, `tool_call`, and the rest) hangs
  off it by `chat_id`. Turns live on `turn`. A chat has no
  `workspace_id`, runs the `internal` harness, and keeps the code-owned
  columns at rest until a session worker attaches. The chat routes read
  only those rows; the code routes and the runtime's boot recovery and
  sweeps read only rows with a workspace or a worker that has attached at
  least once (`spawn_epoch > 0`, or a lifecycle other than `idle`), so a
  conversation only the chat routes have touched is never enumerated as a
  code session. Chat attention stays derived from `turn` and the
  inbox projection; the stored `attention_state` on such a row is the idle
  placeholder.

  `reasoning_effort` is null when the engine's own default is in force, which
  no level on the ladder is equivalent to. `fast_mode` buys output speed at a
  higher price per token, so it is a spend decision rather than a quality one.
- **`turn`** — `id`, `session_id`, `ordinal`, `status`
  (`queued | running | waiting | cancelling | waiting_for_client |
  waiting_for_agent_run | cancelling_client | resuming | retry_wait |
  completed | failed | interrupted`), `user_input` (inline or blob
  reference when large), `checkpoint_ref`, `diffstat` (JSON), `usage`
  (JSON, as reported by the harness), `narrative` (nullable; filled
  asynchronously, never blocks lifecycle), `started_at`, `ended_at`, plus
  the lane columns: `attempt_count`, `max_attempts`, `claim_count`,
  `model_steps`, token counters, `available_at`, `lease_token`,
  `lease_expires_at`, `fingerprint`, `input_message_id`,
  `output_message_id`, `invoked_skills`, `voice_input_used`,
  `steer_revision`.
- **`code_turn_claim`** — one claim token per attempt; the fencing token
  every journal write and heartbeat carries. Renamed from `turn_claim`
  rather than folded into `turn`, so the seven composite foreign keys
  that make append idempotence and heartbeat fencing a property of the
  schema stay referential.
- **`event`** — `(session_id, seq)` primary key, `owner`, `event`
  (JSON), `created_at`, and the chat turn lane's recovery receipts
  `turn_id`, `lease_token`, `attempt_event_ordinal`, `scan_token`, and
  `terminal`. The one journal: every engine's rows, in the `Event`
  vocabulary. A session worker's appends are epoch-fenced — a write
  carrying a stale `spawn_epoch` is rejected, so a superseded worker cannot
  corrupt the stream — and the chat lane's appends are lease-fenced by the
  receipt columns instead: `(lease_token, attempt_event_ordinal)` makes a
  retried append idempotent, `terminal` marks the one row that resolves a
  turn, and `scan_token` marks a terminal row the claim scanner wrote. The
  receipt FKs name `turn` and `code_turn_claim`. Both
  writers take the same session row lock, so their sequences interleave.
  The chat routes read the same rows through `tidebreak_core::chat_journal`,
  the projection that gives each row its chat reading; rows only an external
  engine writes have none and are skipped. The chat journal fixture
  (`fixtures/journal-events.json`) pins that projection.
- **`approval`** — `id`, `session_id`, `turn_id`, `kind` (JSON,
  normalized classification), `harness_raw` (JSON, size-capped),
  `native_call_id`, `worker_epoch`, the decision claim, `state`
  (`Pending | Approved | Denied | Abandoned`), `feedback`, `requested_at`,
  `decided_at`, and `auto_judge_status` (the internal engine's judge
  marker). The one approval surface: a consent card, a questions card, and
  a plan proposal are each one row on every engine, with one
  `ApprovalRequested` and one `ApprovalResolved` journal row. On the
  internal engine the row's id is the tool call id, `kind` is the exact
  preview and grant ladder (`ToolUse`), the questions (`Questions`), or the
  mode a plan proposes (`Plan`, with the plan body on `harness_raw`), and
  `harness_raw` on a consent card is the engine's own request — tool name,
  consent kind, and the standing grant that authorized it when one did.
  `turn_id` names whichever lane parked the row; it is not a foreign key
  until slice D4 merges the turn lane.
- **`code_watch`** — `id`, `workspace_id`, `session_id` (the watch's
  dedicated `kind = watch` session), `pr_number`, `state`
  (`Watching | Fixing | Blocked | Done | Stopped | Failed`), `detail`,
  `last_fix_head`, `cycles`, `created_at`, `updated_at`. Driven by a
  try-based sweep that reads active rows every tick, so restarts resume
  watches with no extra recovery state
  ([`0050`](decisions/0050-watch-and-fix-is-a-durable-task.md)).

Boot recovery (`code/recovery.rs`, per
[`0032`](decisions/0032-code-workspaces-worktrees-checkpoints.md)) checks
sessions recorded as `Running` against their recorded process identity. A
dead external process closes its open turn as `Interrupted`; the transcript
stays intact. Internal-engine checkpoints keep their existing durable resume
path.

Routine recovery runs automatically. A verified leftover local process can
be stopped before a replacement worker attaches. If an engine rejects a
stored resume reference, recovery drops that reference and starts a fresh
engine session. Neither path resubmits the interrupted input. Recovery leaves
queued work paused so an interrupted session does not start another turn
without a fresh action.

Process ownership remains a hard boundary. Never signal a process without its
recorded creation identity. A reused process ID belongs to another process,
and an ambiguous probe must stay blocked. Automatic attempts are bounded and
serialized with manual recovery. Repeated turn failures require the underlying
problem to be fixed rather than an automatic restart loop.

Remote recovery also requires proof that the previous sandbox stopped and
that its terminal events reached the journal. Existing checkpoint and spend
limits still apply. Automatic recovery never waives
missing output or treats a best-effort cancellation as proof of termination.

`Fenced` remains an internal lifecycle value. The UI stays quiet during brief
recovery, shows “Reconnecting…” when it takes longer, and shows the specific
problem when recovery cannot proceed. An existing manual attention pin remains
intact. The recovery notice still exposes the blocking reason and any explicit
retry action. Session digests and live digest notices carry an optional
`fence_reason` so a manual pin cannot hide a recovery failure. Older clients
can ignore the added field.

A resume reference is persisted only after the engine commits it.
`HarnessSession::resume_ref` does not report a token merely because the engine
names it. For example, Codex does not persist a thread before its first turn,
so a bare `thread/start` ID is not reused after a restart.

## The adapter contract

The current `HarnessAdapter` and `HarnessSession` contracts live in
`crates/tidebreak-harness/src/lib.rs`. Read that source for the exact methods,
including model and reasoning-effort discovery, permission-mode relaunch and
switching, durable turn resume, steering, process bookkeeping, and shutdown.

The session worker folds the unrecognized count onto `session`
(`unrecognized_event_count`) at the end of every turn, adding the delta
since the last flush so the row accumulates across engine restarts. The
workspace header shows it per session and the doctor page sums it per
harness: a stream this build only partly understood has to say so, because
a partly-read transcript is otherwise indistinguishable from a complete one.

Process models the trait absorbs:

- **Claude Code** — one long-lived print-mode child per session
  ([`0057`](decisions/0057-one-claude-child-per-session.md)): streamed JSON
  output and input, stdin held open so each turn is one user line, partial
  message deltas on, resuming by the session id the stream reports whenever a
  child has to be replaced. A turn ends on the stream's `result` line, and a
  stop is a `control_request` the engine answers rather than a signal.
  Approvals via the permission-prompt tool over a loopback MCP endpoint with a
  session-scoped token.
- **Codex CLI** — a long-lived `codex app-server --stdio` child driven over
  JSON-RPC, which is the richer of the two channels the CLI offers: its
  approval methods carry structured requests the JSONL exec mode does not.
  The prompt never appears in the argv.
- **opencode** — a long-lived server child driven over HTTP with its event
  stream; permissions over its permission API.
- **Grok CLI** — the pinned 1.0.13 adapter runs a long-lived ACP session with
  structured approvals; it supports Auto, Ask, and Allow and refuses Plan (see
  [`0038`](decisions/0038-auto-is-a-declared-capability.md) and
  [`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md));
  capabilities honestly `Unsupported` or `Unknown` where its surface does not
  carry them.

Every external engine also mounts Tidebreak's connected apps. The server
serves every MCP server it has mounted — the gateway endpoints an
organization entitles plus locally configured servers — as one HTTP MCP
server at `/code/mcp/connected-apps`, and each adapter wires it in as
`tb-apps` with a session-scoped token: Claude Code through `--mcp-config`,
Codex through an `mcp_servers` override with the bearer in
`TIDEBREAK_APPS_TOKEN`, opencode through its config overlay, Grok through the
ACP `mcpServers` list. The in-process engine reads the MCP runtime directly,
so a chat sees the same tools on every engine. Gateway bearers and
attestation stay inside the server; the child only ever holds the loopback
token. Claude Code reads its MCP config from a file only the session's user
can read, never from its arguments, because any local account can read a
process's arguments.

Session-long children spawn lazily on the first turn, and an idle session's
child is parked — stopped, then respawned and resumed by the next turn
([`0064`](decisions/0064-idle-engine-children-are-parked.md)). Resident
engine processes therefore track sessions doing work, not sessions that
exist.

All children are pipe-based `tokio::process` with `kill_on_drop`, the
user's environment minus Tidebreak internals
([`0034`](decisions/0034-harness-discovery-credentials.md)), and bounded
read budgets (fixed-size chunks, capped per tick, overflow counted and
surfaced — parsing must be O(new bytes) and must never fall behind the
terminal-rendering path).

The exact flag strings, event schemas, and approval payload shapes per
harness are established by the fixture-capture spike and recorded in the
fixture manifests — deliberately not transcribed here, so this page cannot
drift from captured reality
([`0031`](decisions/0031-harness-adapter-boundary.md)).

### Repository trust

A repository can carry its own engine config: Claude Code's
`.claude/settings.json` hooks and `.mcp.json` servers, Codex's
`.codex/config.toml`, opencode's `opencode.json` and `.opencode/` plugins,
and the instruction files each engine reads. A headless engine loads them
without asking, as you, outside Tidebreak's approvals and sandbox. So every
engine launches with its project-level config turned off until you trust the
repository (`ProjectConfig` on the launch spec):

- Claude Code: `--setting-sources user --strict-mcp-config`, plus
  `CLAUDE_CODE_DISABLE_CRON=1` for `.claude/scheduled_tasks.json`.
- Codex: `-c projects={"<worktree>"={trust_level="untrusted"}}`, which wins
  over any trust the main checkout has in `~/.codex/config.toml`.
- opencode: `OPENCODE_DISABLE_PROJECT_CONFIG` and
  `OPENCODE_DISABLE_EXTERNAL_SKILLS`.
- Grok: Grok keeps its own folder trust, which Tidebreak never grants over
  ACP; an untrusted launch also turns off `.envrc` evaluation.

The decision is the `tidebreak.trusted` key in the repository's own git
config, so every worktree reads the same answer and nothing needs a
migration. A missing or unreadable key is undecided, and undecided launches
without the config. A session with no workspace involves no repository and
launches as the engine would on its own.

Before the first session in an undecided repository, the desktop reads
`GET /code/workspaces/{id}/trust`. When the worktree carries config the
chosen engine would load, it shows a sheet listing each file and what it
does, and records the answer; otherwise it starts without asking. The
repository's settings show the decision with a switch that revokes it. A
changed decision restarts idle sessions of the repository at once, and a
working session after its turn ends. The scan lives in
`crates/tidebreak-harness/src/project_config.rs` and each engine's switch in
its adapter; both were checked against the pinned engine versions.

### The internal engine

Decision 48 step 5 puts Tidebreak's own agent loop behind the same contract.
`HarnessKind::Internal` is registered by
`crates/tidebreak-server/src/engine/internal/`, and a session created with
no workspace (`POST /sessions`) selects it; the workspace-bound create
path refuses it. The engine probes as found with no binary, needs no pin,
and takes its inference from the server's own provider resolution.

Its durable state is the session row itself: a session with no workspace is
a chat, readable through `/chats/{id}` by the same id, and the engine gives
it the foreground coordinator run its turn lane admits against on first
launch. The lane journals the turn straight into the session's `event`
rows — the one journal — and publishes each row on the session bus, so the
engine translates nothing. `run_turn` admits the message to the lane and
follows the journal for the turn; the session worker's sink applies the
side effects of what it reports (the turn row closes with the usage the
lane recorded) and writes no row a second time.

Approvals are one surface. When the lane parks a tool call for consent it
inserts the `approval` row itself — id the call id, `native_call_id`
the same, `worker_epoch` the session's current epoch — and journals one
`ApprovalRequested` whose `request` carries the card's facts (tool name,
class, consent kind, grant ladder, action preview) so the chat surface
replays the card from the row. A questions card or a plan proposal parks
the same way (`Questions`, `Plan`), and the engine ends the leg as
`TurnOutcome::Parked` on the row the lane minted; the answer or plan
decision resumes it through `resume_turn`. An accepted plan re-postures the
session: the worker calls `set_permission_mode` with the mode the approval
proposed before it resumes the turn. A call a standing grant covers mints
its row already approved, naming the grant, and journals nothing: the
reader was never asked.

Every decision settles the row through the one settle operation in
`db/ops/code/approval.rs` and journals one `ApprovalResolved` there: the
chat routes (`POST /chats/{id}/approvals/{call}`, `/questions/{call_id}/answer`,
`/plans/{call}/decision`), the session route (`POST
/approvals/{id}/decision`, which claims the row, delivers the decision
to `decide`, and settles on acknowledgement), the internal engine's
Auto-mode judge, and the turn lane's cancellation and terminal sweeps. The
agent loop journals no decision of its own; it reads the settled row and
continues. The chat routes for questions and plans are therefore aliases
over the approval decision path, and the pending-questions, pending-plan,
and pending-approvals reads are reads of pending rows by kind.

The capability vector is what carries the difference from an external
engine: `durable_parks`, `user_questions`, and `standing_grants` are
`Supported`, so the decision route offers answers, plan decisions, and the
grant ladder only here. Standing grants are the internal engine's own
ladder, kept on `standing_tool_grant` keyed by the session (its `chat_id`
column) or the project: the chat route's approve-and-remember and the
session route's `approve_with_grant` mint into the same table, `GET /grants`
and `DELETE /grants/{call}` read and revoke the same rows, and a session
whose worker stops keeps its cards — `durable_parks` means the recovery
sweep that abandons an external engine's orphaned requests leaves them
decidable. External engines keep decision 33's posture: verbatim
`harness_raw`, a server-guessed kind, no standing grants. Routes and workers
speak only sessions, turns, the journal, and approvals to it; nothing
reaches the loop another way.

The foreground engine exposes `spawn_sandbox_agent` and `wait_for_agents`.
A background run belongs to the session and its foreground coordinator.
The server's startup admission decision selects in-process or container execution
for that run, as it does for chat turns. An ordered wait stores its child ids
on the turn. The background worker settles the wait after those children finish;
the session worker resumes the same turn with their results. The wait survives
a session worker restart. These bounded background runs do not create separate
child sessions or workspaces.

### Self-drive child sessions

The internal engine exposes native tools that let one conversation start and drive
independent workspace sessions (decision [0094](decisions/0094-repository-optional-conversations-on-the-internal-engine.md)):

- `code_repos` — repositories the conversation's personal or bot identity
  can reach, plus the registered local set when no forge lender exists.
- `code_session_create` — start a child session in a new workspace on the
  named `owner/name` repository, with a stable `request_key` reused on
  retries. The configured GitHub identity controls repository access across
  channels, so no channel repository approval is needed. A retry after `repository_preparing`
  observes the same owner-scoped clone job rather than starting another.
- `code_run_turn` — a follow-up to one of this conversation's children,
  with the same request-key reuse rule.
- `code_wait` — read a bounded list of children, results in requested
  order, with their pending approvals in the snapshot. Children that settle
  within twenty seconds answer inline. Otherwise the parent's turn parks on
  exactly those children and resumes with their results once every one of
  them finishes, ends, fails, or is fenced. The park is durable, so a server
  restart does not lose it and a killed child does not strand the parent.
- `code_sessions` — list this conversation's direct children.

In the desktop and hosted web app, open the parent session to see its
children. Each row shows the child's status and execution location
(Sandbox or This machine). Select Open to view that child's session or
workspace. Child status and active waits update as work progresses.

The updates rail nests child conversations beneath their parent. It limits
indentation so you can still reach deeply nested children at narrow widths.

Children inherit the parent's owner, grant, and forge identity. Machine children
retain the parent's permission mode; configured sandbox children use Allow under
sandbox confinement. Each child appears in its own workspace. These tools expose
children only to the parent that created them. Creating a child is `Sensitive`;
reading and waiting are `ReadOnly`. Revocation refuses discovery, creation, and reads.
Snapshots include the latest top-level answer, truncation, and failure information.

Decision [0094](decisions/0094-repository-optional-conversations-on-the-internal-engine.md)
keeps the current list of work that remains for self-drive child sessions.

## The event vocabulary

`Event` (journal payload; internally tagged, `#[non_exhaustive]`,
bounded):

| Variant | Carries |
|---|---|
| `SessionStarted` | harness kind, version, resume ref |
| `TurnStarted` | turn id |
| `TurnResumed` | turn id — a parked turn continued after a worker restart |
| `AssistantDelta` / `AssistantMessage` | streamed or whole assistant text |
| `ReasoningDelta` | streamed thinking text where the harness reports it |
| `ToolStarted` | call id, name, `ToolDetail` (`Command {cmd, cwd}` \| `FileEdit {path}` \| `FileRead {path}` \| `Search {query}` \| `Other {summary}`) |
| `ToolCompleted` | call id, outcome, bounded preview, optional corrected `ToolDetail`; internal engine adds the whole `output` and the action and result previews |
| `FileChanged` | path, change kind, diffstat |
| `ApprovalRequested` | approval id (hint; body loads from the approvals route); internal engine adds `request` — the consent card's tool name, class, kind, grant ladder and preview, or the turn a questions or plan park resumes |
| `ApprovalResolved` | approval id, decision (`approve`, `deny`, `approved_with_grant`, `answered`, `plan_decided`, `abandoned`) |
| `UserSteered` | the user's mid-turn message; internal engine adds the message row's id |
| `TurnCompleted` | usage, checkpoint info; internal engine adds the stop reason |
| `TurnFailed` | bounded error; internal engine adds the error's kind |
| `TurnInterrupted` | usage up to the interruption, when the engine reports it |
| `CheckpointRecorded` | turn id, diffstat |
| `CheckpointRestored` | restore id, target (before a turn, or before an earlier restore), diffstat, the actor when it was not the owner, and a status: `started` before any file moves, then `completed`, `failed` (nothing changed), or `partial` (its Undo puts back what it replaced), with the reason for the last two. Journaled by the restore route, never by an engine |
| `ReviewFinished` | review id, the reviewing engine and model, the turn reviewed (absent for the working tree), the outcome (`completed`, `failed`, `cancelled`, `timed_out`), and how many findings. Journaled by the review runner in the conversation the review was started from, never by an engine |
| `HarnessNotice` | level, message — the visible-degradation channel |
| `CredentialRefused` | provider and refusal message |

The internal engine's own rows, which no external adapter writes:

| Variant | Carries |
|---|---|
| `TurnRefused` | usage, the model's refusal outcome |
| `StreamInterrupted` | — (discard partial deltas since the last boundary) |
| `ToolArgsDelta` | call id, a fragment of the call's JSON arguments |
| `TaskPlanUpdated` | call id, turn id (a hint; the steps load from the task-plan route) |
| `ContextTruncated` | tokens before and after fitting the context window |
| `CompactionStarted` / `CompactionFinished` | — / whether a checkpoint was stored |

Engines open a tool call before its arguments finish streaming. A supervisor
reads the call while it runs, so an adapter waits for the first view that
carries the arguments and starts the call there — still before the engine
runs the tool. Claude Code assembles them at `content_block_stop`, and
opencode publishes them on the tool part's `running` state.

`ToolCompleted` carries the detail rebuilt from the complete arguments as a
correction for a call that started with nothing to say, and the renderer
takes it unless it says less than what the line already shows. An adapter
whose completion payload carries no arguments leaves it unset.

## Server API surface

The block below is the spine, not the whole router. `crates/tidebreak-server-api/src/lib.rs`
carries the complete list, and the generated wire types are what clients bind
to; transcribing every route here only buys a page that drifts.
Sessions, updates, and approvals use the unprefixed routes shown here.

```
POST/GET        /code/repos                GET/PATCH/DELETE /code/repos/{id}
GET/PUT         /code/repos/{id}/trust     {trusted}  engines load the repo's own config
GET             /code/repos/sources        POST /code/repos/clone    GET /code/repos/clone/{job}
POST/GET        /sessions                  a session with no workspace (internal engine)
GET             /sessions/{id}
GET             /code/harnesses            doctor    POST /code/harnesses/refresh
POST            /code/harnesses/{kind}/install       warm the pinned install (0045)
GET             /code/harnesses/{kind}/models
GET/PUT         /code/worktree-root        {root}    where new worktrees land (admin)

POST/GET        /code/workspaces           {repo_id, base_ref?, title?}
GET/PATCH       /code/workspaces/{id}
POST            /code/workspaces/{id}/archive        {force?}
POST            /code/workspaces/{id}/restore        back from a reclaim tier (0059)
POST            /code/workspaces/{id}/retry-setup    run setup again on the same worktree
GET             /code/workspaces/{id}/trust          the repo's trust and the worktree's engine config
POST            /code/workspaces/{id}/sessions       {harness, permission_mode,
                                                     model?, reasoning_effort?, fast_mode?}
POST/GET        /sessions/{id}/turns                 {message}  (queued while running —
                                                     the chat product's queue-default rule;
                                                     steering is the explicit alternative,
                                                     available only where the adapter's
                                                     mid_turn_steering capability carries it)
GET             /sessions/{id}/queued                the durable queue (0069)
PATCH/DELETE    /sessions/{id}/queued/{queued_id}
PUT             /sessions/{id}/queue-paused
POST            /sessions/{id}/queued/send-now
POST            /sessions/{id}/steer                 one mid-turn message
POST            /sessions/{id}/interrupt | /reap | /fork
POST            /sessions/{id}/mode | /effort | /fast-mode | /attention
WS              /sessions/{id}/events?after=         snapshot → replay → live
                                                     (replay is capped and flags truncation;
                                                     assistant deltas ride the same socket
                                                     as live-only frames — record 0058)
WS              /updates                             digests, restated on connect

GET             /approvals?state=pending
POST            /approvals/{id}/decision             {decision: approve | deny | approve_with_grant | answers | plan_decision, ...}
POST            /code/mcp/approval-prompt            loopback approval endpoint (0033)
POST            /code/mcp/connected-apps             loopback MCP bridge over every mounted MCP server, for external engines
                                                     (both answer loopback peers only)

GET             /code/workspaces/{id}/files          changed files vs base, per-turn filter
GET             /code/workspaces/{id}/diff?turn=&file=   bounded unified diff
GET/POST        /code/workspaces/{id}/checkpoints/restore   ?turn= | ?restore= preview, then
                                                     {target, expected_tree?} restore
POST            /code/workspaces/{id}/revert         {path, turn_id?, hunk?}  undo a file or a hunk
POST            /code/workspaces/{id}/discard        {paths, expected_tree?}  back to the last commit
GET/POST        /code/workspaces/{id}/reviews        list, newest first, only the newest with its result; {session_id, harness, model?, turn_id?, instructions?} starts a read-only review
GET             /code/workspaces/{id}/reviews/{review_id}   progress, then findings or why it failed
POST            /code/workspaces/{id}/reviews/{review_id}/cancel
GET             /code/workspaces/{id}/tree | /search | /blob   the file viewer; /blob carries a hash
PUT             /code/workspaces/{id}/file           {path, content, base_hash}  save one text file
POST            /code/workspaces/{id}/git/commit | /git/push | /git/pr   commit takes {message?, expected_tree?}
GET             /code/workspaces/{id}/pr             PR + checks digest (gh; graceful absence)
POST            /code/workspaces/{id}/pr/check-logs
GET             /code/workspaces/{id}/pull-requests
POST            /code/workspaces/{id}/pr/refresh | /pr/merge | /pr/ready
GET             /code/workspaces/{id}/pr/comments
POST/DELETE     /code/workspaces/{id}/watch          durable watch-and-fix task (0050)
POST            /code/workspaces/{id}/actions/{name} quick action; output journaled
GET/POST        /code/repos/{id}/triggers            durable rules on PR facts (0060)

GET             /code/analytics            GET /code/usage
POST            /code/delivery/pull-requests/query | /detail | /action
POST            /code/delivery/runs/query  | /detail | /action
POST            /code/workspace-title      generate a workspace title

POST/GET        /sessions                      shared conversation collection
GET/POST        /sessions/{id}/...        turns, queue, events, controls, access
GET             /approvals                     pending approvals
POST            /approvals/{id}/decision       settle an approval
WS              /updates                       shared updates stream

POST/GET         /code/workspaces/{id}/terminals
DELETE          /code/workspaces/{id}/terminals/{tid}    close one
GET             /code/workspaces/{id}/terminals/{tid}/read?cursor=
POST            /code/workspaces/{id}/terminals/{tid}/write | /resize
```

The session worker is the only journal writer for an external engine's
session, under a lease and the spawn epoch; for the internal engine the
chat turn lane writes the journal under its own lease. Routes submit work
and read state, they never write the journal directly.

`POST /sessions/{id}/turns` answers `202` as soon as the message is accepted.
A session that was idle answers with the started turn, still `running`: its
row exists and the session reads running. A busy session answers with the
queue row. Neither waits for the engine. Clients follow the turn on the
session's event socket.

### Where worktrees live

A workspace's worktree is created at
`<root>/<repo-slug>/<workspace-slug>-<short-id>/`, where `<root>` is the
`code_worktree_root` setting, or — with none stored — the visible default the
embedding named (`~/Tidebreak/workspaces` on the desktop) or
`<data_dir>/code/worktrees` for a headless deployment. The readable name leads
because people read these paths; the id trails to keep two same-named
workspaces apart. A multi-user deployment inserts the same per-owner segment
clones use.

The root decides where the *next* worktree is created. Every existing workspace
keeps the absolute `worktree_path` on its row: git records absolute paths in
both the worktree's `.git` file and the repository's `.git/worktrees/*` entry,
so moving one is a `git worktree repair` pass rather than a rename. Moving the
root therefore never touches a checkout already on disk.

Pull-request operations use two paths: local workspace operations shell out to
the user's `gh`, while delivery actions use `ForgeAction::Rest` with a brokered
forge credential ([`0063`](decisions/0063-hosted-machines-borrow-forge-credentials.md),
[`0065`](decisions/0065-hosted-git-acts-as-the-person.md)). Absent or signed-out local `gh` degrades to copyable
instructions, never to a broken button.

## UI

Routes (code-defined, hash history, in `ui/src/router.tsx`): `/code` (repo
list, doctor-driven empty state), `/code/w/$workspaceId` (the main
surface; files, diffs, browsers, and terminals open as center tabs
through the existing panel system; git, pull-request state, and comments
live in a review sidebar), `/code/archive` (workspaces at a reclaim tier),
`/code/analytics`, and the two install-wide delivery pages,
`/code/delivery/pull-requests` and `/code/delivery/runs`. A repo has no page
of its own: registering one opens the new-workspace dialog, and picking one on
`/code` does the same.

`ui/src/code/`:

- `CodeSidebar.tsx` on the existing sidebar frame and primitives: one
  "Workspaces" header carrying list settings, add repo, and new workspace,
  then workspace cards with attention badges, and the mode switch back to
  chat.
- `CodeUpdatesStore.ts` — one singleton store fed by `/updates`;
  everything list-shaped reads from it.
- `CodeSessionRegistry.ts` — `Map<sessionId, {store, controller, refCount}>`
  of per-session stores from a `createCodeSessionStore()` factory; only
  mounted session views hold event sockets. This is the chat store factory
  generalized from one pinned instance to N.
- `CodeSessionReducer.ts` — pure
  `(state, SequencedCodeEvent) => {state, effects}` in the chat reducer's
  exact shape.
- Transcript from shared components: existing markdown rendering for
  assistant text, existing tool-card chrome for tool events with a
  code-mode `ToolDetail` renderer; new `CodeApprovalCard` (chat's approval
  visual language, deny opens a feedback field), `TurnReviewCard`
  (diffstat, duration, async narrative slot), `CodeComposer` (text,
  permission-mode selector, interrupt), `CodeInspector`, `WorkspacePrList`,
  and `pullRequestPresentation.ts` (pull-request state and presentation),
  `DiffPanel`/`FilesPanel` (server-produced unified diffs drawn by the shared
  `DiffView`, per-file grouping, per-turn anchoring, Revert file and Revert
  hunk, and line comments; see [Reviewing the diff](#reviewing-the-diff)),
  `DiffOverview` (the changed-file list with each file's revert and
  discard), `CommitBox` (Source control's commit),
  `worktreeUndo.tsx` (the confirmations and flows for restore, revert, and
  discard, behind one shared dialog),
  `FileViewer` (Monaco over the tree/search/blob routes, with the editor
  described in [Editing files](#editing-files)),
  `TerminalPane` (ephemeral renderer over the cursor-read
  API; replays recent bytes on mount; chunked writes on a frame budget).
- Settings: one new section, "Coding engines" — the doctor.
- Repository trust: `RepositoryTrustStore.tsx` asks before the first session
  in an undecided repository with `RepositoryTrustSheet`, a dialog mounted
  once in the shell; `RepositoryTrustSettings` shows and revokes the decision
  in the repository's settings.
- Wire: generated types plus hand-written validators in
  `ui/src/code/parsers.ts`, per [`docs/wire-types.md`](wire-types.md).

## Reviewing the diff

`code/diff/DiffView.tsx` draws the workspace diff, a turn's diff, and a pull
request's files, so all three read the same way.

- Syntax color comes from highlight.js, the highlighter the transcript and
  the output viewer already ship. Grammars load on first use, one chunk per
  language. Each side of a hunk is highlighted as one run: removed and
  unchanged lines for the old side, added and unchanged lines for the new.
  A comment or string that spans lines inside a hunk parses as it does in
  the file, and a hunk that starts inside one can misread only up to its own
  end. A file over 10,000 diff lines, a hunk over 2,500 lines (its removed,
  added, and unchanged lines together), and a hunk with a line over 1,000
  characters stay plain. The highlighter's core loads with the first
  grammar, not with the app. Recently highlighted hunks are kept by a hash
  of their text, within a budget of 2,000,000 characters, so a refresh
  recalls the hunks it did not change; a hunk over a cap keeps nothing.
- Word emphasis pairs each removed line with the added line in the same
  place in its change, and marks the tokens that differ. Lines with less
  than 30% of their visible characters in common keep only the row tint.
- Hide whitespace is worked out from the diff rather than asked of git
  again, so it also works on a pull request's diff, which GitHub produced.
  Inside each change, a removed and an added line that differ only in
  whitespace become one context row, as `git diff -w` shows them, and a hunk
  left with no change goes. Whitespace means what it means to `-w`: spaces,
  tabs, carriage returns, and a final newline. A no-break space or a
  byte-order mark is a change. Reverting a hunk with whitespace hidden still
  reverts the hunk git wrote, and the confirmation says how many hidden
  lines go back with it.
- Split view is the reader's remembered choice. Below 800 pixels the view
  draws unified.
- A long diff mounts one chunk of 60 rows on its first frame and one more
  each frame after, as a transition React renders in slices. Each chunk
  colors its own hunks, and a refresh recalls the hunks it did not change.
  The `Code/Diff review/Very long file` story measures a 5,000-line diff in
  the browser.
- J and K move to the next and previous file, as on GitHub (`]` and `[`
  work too, as on GitLab); on one file's diff they show the next changed
  file in the same tab. W hides whitespace changes.

Clicking a line number, or dragging across several in one hunk, opens a
comment editor under them; the keyboard does the same with the arrows,
Shift, and Enter. Comments wait in the workspace's pending review
(`code/diff/pendingReview.ts`), kept in local storage so a reload or a
restart keeps them. The code composer counts them, and a message can be the
comments alone. The next message from any of the workspace's conversations
carries them in one `<review_comments>` block after the typed text: for
each comment, the path, the diff it was written on (the working tree, or a
turn), the new and old line spans, the quoted lines with diff markers, and
the comment. A quote stops at 200 lines and says so. The send claims the
comments in the step that reads them, so two conversations sending at once
never both carry them, and drops them once the server accepts the message;
a refusal anywhere on the way leaves them pending. The transcript reads the
block back and folds it into one compact list.

A comment holds on to its code, not its line numbers
(`code/diff/commentAnchor.ts`). Each refresh looks for the quoted lines;
where they appear more than once, the three lines on each side when the
comment was written pick the place. When an agent adds a line above, the
comment moves with its line and the review records the new numbers. When
the quoted code changes or leaves the diff, the comment moves to the top of
the file as outdated, with its original quote, and the block marks it
`outdated`. An open comment editor follows its lines the same way and keeps
what was typed.

While a turn runs, comments never hold a message back. A steer or a queued
follow-up goes without them, and the composer says they wait for the next
turn, unless the reader adds them to that message. A queued message that
carries comments shows them as a count in the queue tray; its edit box
edits only the text, and deleting it puts the comments back in the review.

Each comment records who wrote it: the person, or an engine that reviewed
the changes.

### Review changes

Review changes, in the diff's header and the workflow actions, asks another
engine to review the workspace's changes, or one turn's, read-only
(`code/review/`, server `code/review/`). The form starts on an engine that
did not write the changes, shows the model, and runs nothing until Start
review. The review reviews the changes as they are when it starts; later
edits are not part of it.

A review is one hidden turn of an engine session Tidebreak starts and
discards. It has no session row and never takes the workspace's turn lock,
so the agents in the workspace keep working while it runs. It stops after
20 minutes, and a stop takes a few seconds while the engine winds down.
Reviews live in memory: after a restart the server no longer knows one that
was running, and the desktop says it stopped.

Read-only holds in layers:

- The engine loses its tools for writing files, running commands, and
  reaching the network, whatever the person's own rules, hooks, or MCP
  servers allow. Claude Code runs in plan mode with only Read, Grep, and
  Glob (`--tools`), with Bash, Edit, Write, NotebookEdit, WebFetch, and
  WebSearch also denied by name, no MCP servers, and the person's hooks off
  (`disableAllHooks`); the rest of the person's settings, such as the env a
  gateway endpoint needs, still apply. Codex runs in its read-only OS
  sandbox, which also keeps commands off the network, with web search off
  and each MCP server in the person's Codex config turned off by name; a
  Codex that cannot list its servers is refused. opencode runs its plan
  agent with session rules that deny every tool but reading, the person's
  own MCP servers' tools and `webfetch` included, since opencode has no
  switch that keeps a configured MCP server from loading. Grok CLI is
  listed but not offered: it can't turn off network access. Its `read-only`
  sandbox profile does not keep a command the person's own Grok rules allow
  off the network on macOS, the MCP servers in its own config still load,
  and its web search can't be turned off for `grok agent`. An engine with
  neither a plan mode nor approvals Tidebreak can refuse is not offered
  either.
- Every approval the engine asks for is refused, with feedback to report the
  change as a finding instead. The reviewer gets no connected apps, browser,
  computer use, SSH agent, or forge credentials, and the repository's own
  engine config stays unloaded.
- The reviewer works in a disposable copy under the data folder, never in the
  worktree: its own git repository with the reviewed files, uncommitted
  changes included, and `HEAD` at the state before them. Links are copied as
  plain files holding their target, so a write never follows one out. The
  copy's git has no credential helper, hooks, or transport, and the
  reviewer's git reads an empty global config and never prompts. A review
  copies every file in the tree, including files a sparse checkout leaves
  out, and a workspace over 200,000 files or 1 GB is refused before anything
  runs.

What a reviewer can read: whatever the person's account can read, as the
coding engines can, with one exception. opencode's reviewer reads only its
copy, since its rules deny reading outside the working directory. Claude
Code confines reads to the working directory only in `--restricted` mode,
which also drops the person's settings, so reviews do not use it. Codex's
read-only sandbox allows reads everywhere. What leaves the machine: only
what the engine sends its own model provider.

The reviewer answers with findings in JSON, read strictly. A finding on lines
the diff shows becomes a proposed comment by the reviewer, anchored like a
person's; one on other lines becomes a comment above the files naming its
file and lines. Nothing goes with a message until the person keeps or edits
it. A kept finding reaches the agent as a note from the named engine to
check against the code, not as the person's words or an instruction; one the
person rewrote is theirs. A result carries at most 1 MiB of diff, and the
review list carries only the newest review's result.

## Editing files

The file viewer edits one existing text file at a time. The contract is a
compare-and-swap on the file's bytes: a save lands only over the version the
editor loaded, so it does not overwrite an agent's edit nobody has seen.

- `GET /code/workspaces/{id}/blob` returns `hash`, the SHA-256 of the file's
  bytes as lowercase hex, when `content` is the whole file as exact UTF-8 in a
  local worktree. A cut-short, binary, lossily decoded, or sandbox view has no
  hash, and the viewer offers no Edit for it.
- `PUT /code/workspaces/{id}/file` takes `{path, content, base_hash}` and
  writes `content` byte for byte, line endings included, only while the file
  on disk still hashes to `base_hash`. Otherwise it answers `409` with kind
  `file_changed` and `current_hash`, the hash on disk now. Saving against
  `current_hash` is how the editor's Overwrite lands.
- The path resolves the way the read resolves it
  (`worktree::locate_worktree_file`), and the save refuses anything outside
  the worktree or under `.git` (checked on the path and on where its links
  lead), a directory, a file the viewer reads as binary or cuts short, text
  that is not UTF-8, a read-only file, and text over 512 KiB (`413`). It never
  creates a file. A sandbox workspace answers `409 workspace_remote`, and a
  caller who may only view a shared session's workspace gets `404`, the gate
  commit and push use.
- The write goes to a temporary file beside the original, takes the
  original's permissions, and replaces it with one rename, under the
  workspace write lock that terminal writes and archive also take. It does
  not wait for a running turn: the hash is the guard. The hash check and the
  rename are not atomic against other processes, so an agent write that lands
  in the milliseconds between them is lost.
- A save belongs to no turn, so it writes nothing to a session journal.
  Instead it publishes `files_changed` on `/updates` to the owner and the
  caller, and the workspace page adds that count to the session's content
  revision, so the file list, the diff, and the changed-file count refresh the
  way they do after an agent's edit.

In the desktop, `CodeFileDraftStore` holds each unsaved buffer by workspace
and path, so the text outlives the viewer when you switch tabs. The center
tabs read it to mark a file with unsaved changes, and `useUnsavedFilesGuard`
reads it to ask before a navigation drops one: the layout lives in the URL,
so closing a tab, leaving the workspace, and going back are all navigations
it sees. A browser reloading or closing the page gets the same question
through the unload handler. `unsavedCodeFiles` answers it for every
workspace, for a later quit confirmation.

A reload that finds the file changed under an unsaved buffer keeps the buffer
and raises a notice with Reload and Keep my changes. A refused save raises the
same notice with Compare, a Monaco diff of the version on disk against yours,
and Overwrite. Markdown files (`.md`, `.mdx`, `.markdown`) open as a rendered
preview through the app's markdown renderer, with a toggle back to the source
while reading or editing.

Out of scope: creating, renaming, or deleting files; editing in a sandbox
workspace; multi-file find and replace; editing inside the diff panel. The
desktop's View > Reload menu item reloads from the native side, so it does
not ask about unsaved files yet; the quit confirmation will need the same
native handshake.

## Undo in the worktree

Three operations change a workspace's live checkout on a person's behalf:
restore to a checkpoint, revert a file or a hunk, and discard a file's
uncommitted changes. Record 32's amendment covers why restore works the way
it does. One invariant holds for all three: an undo never overwrites or
removes anything the person did not pick, and a restore keeps everything it
replaces so its Undo can bring it back. They share one gate in
`code/runtime/undo.rs`:

- They run between turns only. The worktree turn lock is tried, not waited
  for, and a held lock answers `409 turn_running`: the person asked to undo
  the worktree they see now, not the one a finished turn will leave. A
  session recorded as `Running` without a live lock answers the same.
- A session fenced for an engine that may still be alive in the checkout
  answers `409 workspace_fenced`, the rule turns already follow (record 55).
- They take the workspace write lock first, then try the turn lock, the
  order auto-recovery uses, so none of them can deadlock with a turn.
- A sandbox workspace answers `409 workspace_remote`. A caller who may only
  view a shared session's workspace gets `404`, as for commit and push.
- The routes run each one on a task of its own, so a client that goes away
  mid-request cannot stop the checkout halfway through the worktree.
- Each one publishes `files_changed` on `/updates`, like a save, so every view
  of the worktree reads it again.

**How files move** (`checkpoint/worktree.rs`). No undo runs `git checkout`,
`read-tree`, or `checkout-index`. Each operation snapshots the worktree into
a private index, works out the tree the worktree should hold next, and moves
the paths that differ itself, one at a time, under the worktree lock:

- `inspect` lists the paths with `diff-tree --no-renames`, records what the
  worktree holds at each (kind, size, mode, and modification time), then
  hashes each file and checks it against the snapshot. It also stores each
  file it would replace or remove exactly as its bytes stand, with
  `hash-object -w --no-filters`: a snapshot holds what the clean filters
  made of a file, and a lossy filter drops the rest. It names what is in the
  way: an ignored or excluded file, a folder holding one, and a nested
  repository, submodule, or gitlink the operation would remove or replace. A
  snapshot holds only a nested repository's commit, never its files, so no
  undo could bring them back.
- It also refuses a change to a path that opens the same file on this disk
  as another path the snapshot holds. With `core.ignorecase` or
  `core.precomposeunicode` off on a disk that folds names, git keeps
  `readme.md` and `README.md`, or two Unicode forms of one name, apart, and
  changing one changes the other. The refusal names both. A case-only
  rename, one spelling removed and the other added, passes.
- `apply` removes paths first, deepest first, then writes, parents first.
  It writes each new version, through the repository's checkout filters, to
  a temporary file in the worktree's own git folder (`tidebreak-tmp`, under
  a short name of fixed length). A crash mid-write leaves that file where
  git never lists it, and the next change, or the next boot, clears it; a
  file whose name is as long as the disk allows still gets written. When the
  git folder is on another disk, the file goes beside its path under a short
  name instead. Right before it moves into place, the path must still hold
  what `inspect` recorded, or still be empty, or the apply stops, so an
  ignored file that appears after the check is never overwritten. The file
  lands with a rename, or with a hard link where nothing stood, so each path
  holds its old content or its new content, never a mix. It never writes
  through a symlink: a symlink or a file where a folder must be stops it.
- When a path cannot move, every path already moved goes back as its exact
  bytes, newest first, and each is checked. A path someone changed after the
  apply left it stays as they left it. Only when every moved path checks out
  may the caller say nothing changed.
- A sparse checkout answers `409 sparse_checkout`: its snapshot cannot tell a
  file outside the cone from a deleted one.

No hook runs, and a file whose content matches is never touched.

**Restore** (`checkpoint/restore.rs`). The target is the state before a turn,
which is the `from` of that turn's own diff: the previous turn's checkpoint,
the session's start baseline for turn 1, or where the chain resumed after an
earlier restore. A turn with none refuses with `409 no_checkpoint`. It never
falls back to the merge base, which knows nothing of untracked files, or to
an older checkpoint, which would also undo turns nobody picked.

1. The preview (`GET …/checkpoints/restore?turn=` or `?restore=`) snapshots the
   worktree and lists every change since the target, whoever made it. It
   returns the snapshot's tree as `current_tree`, the unsaved files in the way
   as `blocked`, and as `affected_turns` the other agents' turns the restore
   also undoes: turns in other sessions that started after the target state
   was taken, and for an undo, every turn since the restore.
2. The restore snapshots the worktree again through a private index. When the
   tree differs from `expected_tree`, it answers `409 worktree_changed`. When
   anything is in the way, it answers `409 restore_blocked` and names it.
   Either way nothing changes.
3. It commits that tree to `refs/tidebreak/checkpoints/<ws>/<session>/restore/<id>`
   and journals `CheckpointRestored` with status `started`, both before any
   file moves, so the restore's Undo is reachable even if the process dies
   mid-restore. A file whose exact bytes the snapshot lacks, because a clean
   filter changed them, is kept as it stood in a second commit: the saved
   state's second parent, named by its `Tidebreak-Exact-Bytes:` trailer. A
   restore to a saved state writes those bytes back with no filters. The
   restore also writes a record to `{data_dir}/code/restores/<id>.json`, and
   removes it once the last row is journaled.
4. It moves the files. When it stops partway, every path it moved goes back.
   Only when each checks out does the row end `failed`, which says nothing
   changed. Otherwise the row ends `partial`, the reply names the restore id,
   and every open session's chain points at the files as they stand. The row
   keeps its Undo either way: it puts back everything the restore replaced.
   A record the next boot finds belongs to a restore the process never
   finished. Before any worker attaches, the boot journals it as `partial`,
   with its Undo, and points every open session's chain at the files as they
   stand. The same boot clears every local worktree's staging folder.
5. It points `…/<session>/after/<n>` at the restored state for every open
   session in the workspace, where `n` is that session's newest turn. The next
   turn's diff starts there, so no turn is credited with undoing what the
   restore undid. The commit on that ref records where the restore went, and
   the session's next turn reads it to tell the engine, ahead of the person's
   message, which files moved since its last turn.
6. It journals the row again with status `completed`. The undo is a restore
   whose target is `before_restore`, found by the saved ref's id, and it saves
   its own state in turn.

**Revert** (`checkpoint/revert.rs`). The request names the diff being read
(`turn_id` or the workspace against its base), the file, and optionally one
hunk by position with its text as shown. Diffs pair a renamed file with its
old path, in the view and in the revert, so a rename never reads as an added
file. The server rebuilds the diff with the view's own flags and checks the
hunk text matches (`409 diff_changed` otherwise). It undoes the hunk on the
file as the diff left it, at exactly the lines the hunk names, and answers
`409 diff_changed` when those lines are not there. It then carries that
change onto the file as it stands now with a three-way `merge-file`, the
diff's version as the base. A later edit elsewhere in the file stays; one
that overlaps the change answers `409 revert_conflict`, and nothing is
written, so a stale hunk never lands on another block that reads the same. A
whole file goes back the same way. An added file is removed only while it is
still exactly as the diff left it, and a renamed file goes back to its old
name. A turn's diff is history, so the desktop marks a reverted hunk there
instead of offering it again.

Revert and discard build the new version from what the clean filters kept,
and neither has an Undo. So each refuses a file whose exact bytes the
filters do not give back, such as a notebook whose output a filter strips
(`409 filter_lossy`), and names the filter. Line endings git converts on the
way in and gives back on checkout pass.

**Discard** acts on exactly the paths it is given, as the Changes list names
them: a renamed file's row names its new path and its old one. It puts each
back to `HEAD`, or removes it when `HEAD` lacks it, and unstages it with
`reset -q HEAD --`. A named path with no uncommitted change, read against
`HEAD` with `--no-renames` as the list's `uncommitted` mark is, stays as it
is, and a request where no path has one answers `409 no_change`. A folder
that stands where a committed file goes, holding files nobody named, answers
`409 discard_blocked` and names them. `expected_tree`, the `worktree_tree` of
the list the person reviewed, refuses a named file that changed since. The
workspace file list marks discardable paths `uncommitted`, which is where
Source control offers Discard.

**Commit** keeps its route. It refuses at once while a turn runs or waits
(`409 turn_running`) or a message waits in an unpaused queue
(`409 turn_queued`), instead of waiting on the turn lock and then committing
that turn's unreviewed work under the person's message. `expected_tree`, the
`worktree_tree` of the list the person reviewed, refuses a worktree that moved
since (`409 worktree_changed`). A refused commit, most often a hook, answers
`409 commit_rejected` with a sentence, a blank line, and what the commit and
its hook printed. The commit gets ten minutes, not the thirty seconds other
calls get, so a hook or a signing prompt can finish; past that it answers
`409 commit_timed_out` and says a hook or prompt may be waiting.

## Testing

- Adapter parsers: fixture replay only
  ([`0031`](decisions/0031-harness-adapter-boundary.md)); fixtures are
  re-captured on harness version bumps, and that procedure lives in
  `crates/tidebreak-harness/fixtures/README.md`.
- Orchestration and routes: driven end to end against the scripted harness
  (turn lifecycle, WS replay, approvals round-trip including
  deny-with-feedback, interrupt, recovery matrix).
- Git: integration tests against throwaway temp repos.
- UI: reducer unit tests; DOM tests for transcript, approval card, and
  registry reference counting.

## What v1 excludes

Recorded in [`docs/deferred.md`](deferred.md): running a harness in a PTY;
an in-app editor beyond saving existing text files; chat–code convergence
(the single-surface end state: one conversation concept with an optional
workspace binding, engines behind the adapter contract, no user-facing mode
choice); the local mobile relay; and a per-repo worktree-location override.
