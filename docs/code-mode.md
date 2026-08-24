# Code mode

Status: supported and enabled by default. The decision records
[`0030`](decisions/0030-code-mode-separate-surface.md) through
[`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md) carry the decisions
and are accepted; this page carries the working design detail in one place.
Where this page and a decision record disagree, the record wins. The first
version ships the full surface described here — repos, workspaces, sessions,
approvals, checkpoints and review, auxiliary terminals, the git/PR flow, the
updates channel, and adapters for Claude Code (reference tier), Codex CLI,
opencode, and Grok CLI (the last honors Auto and Allow: its captured 1.0.4 plan and sandbox flags do
not confine and it has no approval channel, so Auto is the unsupervised
default headless posture and Allow is `--always-approve`, both stated where
the mode is chosen — see
[`0038`](decisions/0038-auto-is-a-declared-capability.md) and
[`0039`](decisions/0039-allow-is-a-first-class-code-permission-mode.md)) — with the
deliberately parked scope recorded in [`docs/deferred.md`](deferred.md).

Code mode is Tidebreak's second product surface and is available without an
experimental opt-in: pick a local git repository,
spin up isolated **workspaces** (one worktree + branch each), and run
**sessions** — durable conversations with external coding-agent harnesses
(Claude Code first; Codex CLI, opencode, and Grok CLI behind it) — supervised
through a structured UI: conversation, tool activity, approvals, per-turn
diffs, and a pull-request flow. The two-mode split is a delivery strategy:
the destination is one surface where context selects behavior — a
conversation bound to a workspace behaves code-like, one without is ordinary
chat — and the adapter contract below is the runtime interface every engine,
eventually including Tidebreak's own internal loop, sits behind
([`0030`](decisions/0030-code-mode-separate-surface.md)).

## Vocabulary

| Noun | Meaning |
|---|---|
| repo | A registered local git repository: root path, default base ref, branch prefix, setup/archive scripts, quick actions. |
| workspace | One isolated unit of work on a repo. Owns exactly one git worktree and one branch for life; carries PR state. |
| session | One durable conversation with one harness inside a workspace. A workspace holds several, plus at most one watch session; their turns are serialized on the shared worktree ([`0055`](decisions/0055-multiple-sessions-per-workspace.md)). |
| turn | One user→agent cycle in a session, ending in a checkpoint. |
| harness | The external coding-agent CLI being driven (never "provider"). |

## Crate and module layout

Dependencies flow downward per [`docs/crates.md`](crates.md):
`tidebreak-core` ← `tidebreak-harness` ← `tidebreak-server` ← clients.

```
crates/tidebreak-core/src/code/
  mod.rs         RepoId, WorkspaceId, CodeSessionId, CodeTurnId, CodeApprovalId,
                 CodeTerminalId, HarnessKind — new id types, structurally like chat ids
  event.rs       CodeEvent, SequencedCodeEvent (conventions of src/event.rs)
  attention.rs   AttentionState, AttentionSource, should_replace
  caps.rs        HarnessCaps, CapLevel, HarnessTier
  (permission)   PermissionMode { Plan, Ask, Auto, Allow }, shared with chat
                 (decision 48 step 2), and its per-mode contract

crates/tidebreak-core/src/db/entities.rs      six new entities (below)
crates/tidebreak-core/src/db/ops/code/        repo.rs, workspace.rs, session.rs,
                                              turn.rs, journal.rs, approval.rs

crates/tidebreak-harness/                     protocol translation only
  src/lib.rs       HarnessAdapter + HarnessSession traits, SessionSpec, LaunchPlan,
                   adapter registry
  src/probe.rs     interactive-login shell resolution + env capture (0034),
                   version detection, auth observation
  src/budget.rs    bounded stream-parse budgets
  src/claude/      mod.rs, parse.rs, session.rs, approvals.rs
  src/codex/  src/opencode/  src/grok/        later phases
  src/bin/capture.rs                          dev-only, feature = "capture"
  fixtures/<harness>/<version>/               *.ndjson + manifest.toml + *.expected.json

crates/tidebreak-server/src/code/
  mod.rs             wiring
  session_worker.rs  per-session task: lease + spawn-epoch, adapter session, event pump
  worktree.rs        git shell-out: repo validation, worktree add/remove/prune/self-heal
  checkpoint.rs      hidden refs, synthetic commits via temp index, bounded diffs
  setup_script.rs    setup/archive hooks, failure-preserves-checkout
  recovery.rs        boot scan, fencing, orphan probe, reap
  attention.rs       server-side attention computation, digest publication
  approval_bridge.rs the loopback approval-prompt endpoint glue and decision routing
  gh.rs              gh CLI shell-out: commit/push/PR/checks, graceful absence
  terminal.rs        PTY shells, ring buffers, cursor reads (0036)
  bus.rs             per-session broadcast + install-wide updates channel
crates/tidebreak-server/src/routes/code/      repos, workspaces, sessions,
                                              session_events, updates, approvals,
                                              diffs, git, terminals, harnesses
crates/tidebreak-server/src/scripted_harness.rs   feature-gated fake adapter
                                              (the scripted_provider.rs pattern)

crates/tidebreak-desktop/ui/src/code/         the UI family (below)
```

New dependencies, all exact-pinned and lockfile-matching: one Rust
pseudo-terminal crate (terminals only — the harness crate must not depend on
it, enforced by a dependency check), and the terminal-emulator UI package
pair. Nothing else: no git library, no editor component, no diff library
(git produces diffs; the UI styles them), no virtualization until measured.

## Data model

A schema change here is an appended migration, per
[`0061`](decisions/0061-schema-changes-are-migrations.md). The tables below
describe the current schema, not the frozen baseline.

- **`code_repo`** — `id`, `root_path` (unique, canonical toplevel),
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
- **`code_workspace`** — `id`, `repo_id`, `title`, `worktree_path`,
  `branch_name`, `base_ref`, `status`
  (`Creating | SetupFailed | Active | Archived | Released`), `pr` (JSON digest:
  number, url, state, checks summary; nullable), `created_at`,
  `archived_at`, `released_at`, `released_tip`, `bundle_bytes`.

  Archived and Released are reclaim tiers. Archive removes the worktree and
  keeps the branch, so restore is `git worktree add`. Release bundles
  `base..branch` into `<data_dir>/code/bundles/<id>.bundle` and drops the
  branch, so restore fetches from the bundle first. A checkout is gigabytes of
  build output; a branch's own commits are usually kilobytes, which is what
  makes the deeper tier worth the step. Transcripts are untouched at every
  tier — the row and its journal outlive the bytes.
- **`code_session`** — `id`, `workspace_id`, `kind`
  (`Interactive | Watch`, per
  [`0050`](decisions/0050-watch-and-fix-is-a-durable-task.md)), `harness_kind`,
  `harness_version` (observed at launch), `harness_resume_ref` (the
  harness's own session/thread id for resume), `permission_mode`,
  `lifecycle` (`Created | Idle | Running | Fenced | Ended`), `fence_reason`
  (JSON, nullable), `child_pid`, `spawn_epoch`, `attention_state` (JSON),
  `attention_source`, `unrecognized_event_count`, `created_at`.
- **`code_turn`** — `id`, `session_id`, `ordinal`, `status`
  (`Running | Completed | Failed | Interrupted`), `user_input` (inline or
  blob reference when large), `checkpoint_ref`, `diffstat` (JSON), `usage`
  (JSON, as reported by the harness), `narrative` (nullable; filled
  asynchronously, never blocks lifecycle), `started_at`, `ended_at`.
- **`code_event`** — `(session_id, seq)` primary key, `event` (JSON),
  `created_at`. The journal. Appends are epoch-fenced: a write carrying a
  stale `spawn_epoch` is rejected, so a superseded worker cannot corrupt the
  stream (the `db/ops/turn/` claim discipline applied here).
- **`code_approval`** — `id`, `session_id`, `turn_id`, `kind` (JSON,
  normalized classification), `harness_raw` (JSON, size-capped), `state`
  (`Pending | Approved | Denied`), `feedback`, `requested_at`,
  `decided_at`.
- **`code_watch`** — `id`, `workspace_id`, `session_id` (the watch's
  dedicated `kind = watch` session), `pr_number`, `state`
  (`Watching | Fixing | Blocked | Done | Stopped | Failed`), `detail`,
  `last_fix_head`, `cycles`, `created_at`, `updated_at`. Driven by a
  try-based sweep that reads active rows every tick, so restarts resume
  watches with no extra recovery state
  ([`0050`](decisions/0050-watch-and-fix-is-a-durable-task.md)).

Boot recovery (`code/recovery.rs`, per
[`0032`](decisions/0032-code-workspaces-worktrees-checkpoints.md)): sessions
recorded `Running` are probed by recorded pid. Dead → the open turn closes
as `Interrupted` (journaled), session `Idle`, attention
`NeedsYou { source: Lifecycle }`. Alive → `Fenced { OrphanAlive }` until an
explicit reap. Never signal a pid not recorded at spawn; `EPERM` counts as
alive.

A resume ref is only worth persisting once it would actually resume:
`HarnessSession::resume_ref` reports a token after the engine has committed
it, not when the engine first names it. Codex, for instance, does not write a
thread that has run no turn, so a thread id from `thread/start` alone stays
unreported and a session whose engine dies before its first turn re-attaches
with a fresh `thread/start`. When an engine does refuse a stored ref, the
adapter reports `HarnessError::ResumeLost` and the session is
`Fenced { ResumeLost }` — the fence drops the dead ref, so the reap it asks
for starts a clean engine session instead of failing every turn identically.

## The adapter contract

```rust
#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    fn kind(&self) -> HarnessKind;
    /// Login-shell PATH resolution, version detection, auth observation.
    /// Never reads or stores credentials (0034).
    async fn probe(&self, host: &HostEnv) -> HarnessProbe;
    /// Every capability flag stated for the probed version; `Unknown` is
    /// legal, silence is not (0031).
    fn capabilities(&self, probe: &HarnessProbe) -> HarnessCaps;
    /// Spawn or connect for one session. The spec carries the worktree path,
    /// permission mode, resume ref, approval-channel wiring, and event sink.
    async fn launch(&self, spec: SessionSpec)
        -> Result<Box<dyn HarnessSession>, HarnessError>;
}

#[async_trait]
pub trait HarnessSession: Send + Sync {
    /// Feed one user turn; normalized events flow to the sink until a
    /// terminal turn event arrives. The outcome reports how the engine
    /// *process* ended — stdout reaching EOF is not a completed turn.
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError>;
    /// Resolve a pending approval through the harness's native channel (0033).
    async fn decide(&self, approval: HarnessApprovalRef,
                    decision: ApprovalDecision) -> Result<(), HarnessError>;
    async fn interrupt(&self) -> Result<(), HarnessError>;
    fn resume_ref(&self) -> Option<String>;
    /// Pid of a child this session spawned, and every transition of it. An
    /// adapter with one child per turn publishes the pid the moment the child
    /// exists: the session row's pid is what crash recovery probes (0032),
    /// and it has to be there for the whole time a turn is in flight.
    fn child_pid(&self) -> Option<i64>;
    fn child_pid_changes(&self) -> Option<watch::Receiver<Option<i64>>>;
    /// Stream events this build could not map, counted since launch (0031).
    fn unrecognized_events(&self) -> u64;
    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError>;
}
```

The session worker folds the unrecognized count onto `code_session`
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
- **Codex CLI** — a long-lived JSON-RPC server child (preferred; its
  approval methods are the richer channel) or the JSONL exec mode with
  resume, whichever the fixture spike proves stable on the installed
  version.
- **opencode** — a long-lived server child driven over HTTP with its event
  stream; permissions over its permission API.
- **Grok CLI** — best-effort tier; one print-mode child per turn; honors
  Auto only, as its default headless posture (unsupervised — see
  [`0038`](decisions/0038-auto-is-a-declared-capability.md)); capabilities
  honestly `Unsupported` or `Unknown` where its surface does not carry them.

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

## The event vocabulary

`CodeEvent` (journal payload; internally tagged, `#[non_exhaustive]`,
bounded):

| Variant | Carries |
|---|---|
| `SessionStarted` | harness kind, version, resume ref |
| `TurnStarted` | turn id |
| `AssistantDelta` / `AssistantMessage` | streamed or whole assistant text |
| `ReasoningDelta` | streamed thinking text where the harness reports it |
| `ToolStarted` | call id, name, `ToolDetail` (`Command {cmd, cwd}` \| `FileEdit {path}` \| `FileRead {path}` \| `Search {query}` \| `Other {summary}`) |
| `ToolCompleted` | call id, outcome, bounded preview, optional corrected `ToolDetail` |
| `FileChanged` | path, change kind, diffstat |
| `ApprovalRequested` | approval id (hint; body loads from the approvals route) |
| `ApprovalResolved` | approval id, decision |
| `UserSteered` | the user's mid-turn message |
| `TurnCompleted` | usage, checkpoint info |
| `TurnFailed` | bounded error |
| `TurnInterrupted` | — |
| `CheckpointRecorded` | turn id, diffstat |
| `HarnessNotice` | level, message — the visible-degradation channel |
| `AttentionChanged` | state, source |

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

```
POST/GET        /code/repos                GET/PATCH/DELETE /code/repos/{id}
GET             /code/harnesses            doctor    POST /code/harnesses/refresh
GET/PUT         /code/worktree-root        {root}    where new worktrees land (admin)

POST/GET        /code/workspaces           {repo_id, base_ref?, title?}
GET/PATCH       /code/workspaces/{id}
POST            /code/workspaces/{id}/archive        {force?}
POST            /code/workspaces/{id}/release        {force?}
POST            /code/workspaces/{id}/sessions       {harness, permission_mode}
POST            /code/sessions/{id}/turns            {message}  (queued while running —
                                                     the chat product's queue-default rule;
                                                     steering is the explicit alternative,
                                                     available only where the adapter's
                                                     mid_turn_steering capability carries it)
POST            /code/sessions/{id}/interrupt | /permission-mode | /attention | /reap
WS              /code/sessions/{id}/events?after=    snapshot → replay → live
                                                     (replay is capped and flags truncation;
                                                     assistant deltas ride the same socket
                                                     as live-only frames — record 0058)
WS              /code/updates                        digests, restated on connect

GET             /code/approvals?state=pending
POST            /code/approvals/{id}/decision        {approve | deny, feedback?}
POST            /code/mcp/approval-prompt            loopback approval endpoint (0033)

GET             /code/workspaces/{id}/files          changed files vs base, per-turn filter
GET             /code/workspaces/{id}/diff?turn=&file=   bounded unified diff
POST            /code/workspaces/{id}/git/commit | /git/push | /git/pr
GET             /code/workspaces/{id}/pr             PR + checks digest (gh; graceful absence)
POST/DELETE     /code/workspaces/{id}/watch          durable watch-and-fix task (0050)
POST            /code/workspaces/{id}/actions/{name} quick action; output journaled

POST/GET/DELETE /code/workspaces/{id}/terminals
GET             /code/workspaces/{id}/terminals/{tid}/read?cursor=
POST            /code/workspaces/{id}/terminals/{tid}/write | /resize
```

The session worker is the only journal writer for its session, under a
lease and the spawn epoch; routes submit work and read state, they never
write the journal directly.

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

Pull-request operations shell out to the user's `gh` (auth observed, never
brokered — the [`0034`](decisions/0034-harness-discovery-credentials.md)
boundary applies to `gh` exactly as to harnesses). Absent or signed-out
`gh` degrades to copyable instructions, never to a broken button.

## UI

Routes (code-defined, hash history, in `ui/src/router.tsx`): `/code` (repo
list, doctor-driven empty state), `/code/w/$workspaceId` (the main
surface; files, diffs, browsers, and terminals open as center tabs
through the existing panel system; git, pull-request state, and comments
live in a review sidebar). A repo has no page of its own: registering one
opens the new-workspace dialog, and picking one on `/code` does the same.

`ui/src/code/`:

- `CodeSidebar.tsx` on the existing sidebar frame and primitives: one
  "Workspaces" header carrying list settings, add repo, and new workspace,
  then workspace cards with attention badges, and the mode switch back to
  chat.
- `CodeUpdatesStore.ts` — one singleton store fed by `/code/updates`;
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
  permission-mode selector, interrupt), `PrCard` (status-quad chips, hosted
  in the review sidebar), `CodeInspector` (git sync, PR state, comments),
  `DiffPanel`/`FilesPanel` (server-produced unified diffs styled with the
  semantic status tokens; per-file grouping; per-turn anchoring),
  `TerminalDrawer`/`TerminalPane` (ephemeral renderer over the cursor-read
  API; replays recent bytes on mount; chunked writes on a frame budget).
- Settings: one new section, "Coding harnesses" — the doctor.
- Wire: generated types plus hand-written validators in
  `ui/src/code/parsers.ts`, per [`docs/wire-types.md`](wire-types.md).

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
- Live harnesses cannot run in CI. Env-gated smoke tests
  (`TIDEBREAK_LIVE_HARNESS=1`, ignored by default) plus a per-adapter
  manual smoke checklist cover the real thing before an adapter ships.

## What v1 excludes

Recorded in [`docs/deferred.md`](deferred.md): running a harness in a PTY;
checkpoint restore (the refs land in v1; the restore surface does not);
multiple sessions per workspace;
an in-app code editor; chat–code convergence (the single-surface end
state: one conversation concept with an optional workspace binding,
engines behind the adapter contract, no user-facing mode choice); remote
session execution (the same harness in a managed sandbox feeding the same
journal); a supervision-first mobile client over the updates channel; and a
per-repo worktree-location override.
