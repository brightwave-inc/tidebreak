# 60. Triggers Are Durable Rules On Pull-Request Facts

- Status: Proposed
- Date: 2026-08-21
- Owners: code mode, harness integration
- Related: [`0009-queued-turns.md`](0009-queued-turns.md),
  [`0031-harness-adapter-boundary.md`](0031-harness-adapter-boundary.md),
  [`0042-user-initiated-pr-merge.md`](0042-user-initiated-pr-merge.md),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md),
  [`0050-watch-and-fix-is-a-durable-task.md`](0050-watch-and-fix-is-a-durable-task.md),
  [`0055-multiple-sessions-per-workspace.md`](0055-multiple-sessions-per-workspace.md),
  [`0057-one-claude-child-per-session.md`](0057-one-claude-child-per-session.md)

## Context

An agent working a pull request learns its checks failed by asking. Polling
costs a tool call and a slice of context per look, and it is the agent's own
loop, so nothing outside the turn can make it look sooner. What the user wants
is the opposite arrangement: declare once that a failing check should reach the
agent working that workspace, and have the fact arrive.

Three pieces of this already exist and none of them know about each other.

**Watch-and-fix is the automation half, hard-coded.**
[Record 50](0050-watch-and-fix-is-a-durable-task.md) made "CI fails, an agent
acts" a `code_watch` row driven by a try-based sweep, and everything a trigger
needs is in it: `assess` classifying a fresh `PullRequestDigest`
(`crates/tidebreak-server/src/code/watch.rs`), `fix_turn_instruction` composing
a bounded message from check names and buckets rather than raw logs, a
`last_fix_head` guard that parks instead of retrying against an unchanged head,
and a contention guard that skips the cycle while any session in the workspace
runs a turn. What is fixed is the condition set, the action, and the dedicated
`kind = watch` session it drives.

**Notification rules are the user-defined half, and they are not durable.**
`crates/tidebreak-desktop/ui/src/code/CodeDeliveryStore.ts` already ships
user-editable rules — `pull_request_attention`, `pull_request_ready`,
`run_failure`, each with repository scoping — from #2348. They live in
`localStorage` under `tidebreak.code-delivery`, with no record behind them and
no server row. The durable/ephemeral split is backwards: the rules the user
wrote are the ephemeral ones, and the automation nobody can edit is durable.

**Mid-turn delivery is reachable but not uniformly available.**
`CodeRuntime::steer` refuses unless the session's harness declares
`mid_turn_steering: CapLevel::Supported`. Codex declares it on the 0.147 line,
behind `supports_native_steering`. [Record 57](0057-one-claude-child-per-session.md)
holds Claude Code's stdin open and deliberately does not claim the capability.

That question is now answered, and the answer is conditional. Captured live on
Claude Code 2.1.239 and recorded under `verified_2_1_239` in
`crates/tidebreak-harness/fixtures/claude-code/2.1.233/manifest.toml`: a user
line written to open stdin during a running turn is queued, and the running
turn reads it **at a tool-call boundary** — the call in flight finishes, one
further call runs, then the model answers the injected message and abandons its
original task, ending with one `result` and `num_turns=3`. A turn with no
further tool call never reaches it. A text-only turn kept generating for
seventy seconds after the write, ended `num_turns=1`, and the queued line then
ran as its own turn behind a reprinted `system/init`. Nothing acknowledges the
write.

So Claude Code cannot declare `Supported` on the strength of that capture
alone. A line the turn never reaches becomes an unannounced extra turn, and
`EngineChannel`'s reader and parser live for the whole session, so that turn's
`result` would be read as the terminal event for whatever turn is submitted
next. Bounding that miss is adapter work, not a capability edit.

## Decision

**A trigger is a durable row, not a prompt.** It is modelled on `code_watch`
in `crates/tidebreak-core/src/db/migration/baseline/code.rs`, with operations
beside `crates/tidebreak-core/src/db/ops/code/watch.rs`, and driven by a
try-based sweep that reads its work list from the table every tick. The event
bus is lossy `broadcast`; anything that must not miss a fact reads rows on a
sweep rather than subscribing.

**Conditions are enum-shaped edge transitions of `PullRequestDigest`,
fingerprinted against `head_sha`.** The classifier generalizes `assess`: the
same digest, the same buckets, the same host tokens. A trigger fires on the
transition into a condition, once per `head_sha`, not on every sweep that finds
the condition still true. There is no user scripting and no expression
language.

**Actions in v1 are exactly two: deliver a bounded message to a session, and
raise a notification.** Merge, auto-merge, and mark-ready stay excluded —
[record 42](0042-user-initiated-pr-merge.md) reserves those for the user, and a
trigger is a weaker authorization than a watch, not a stronger one. No shell
commands and no webhooks.

**Delivery is steer where the harness declares it, and a sweep retry
otherwise.** This is [record 9](0009-queued-turns.md)'s posture — queue is the
default, steer is the deliberate alternative — expressed in code mode's
vocabulary. Code sessions have no `queued_turn` table; the fallback is the
watch's own idiom, holding the fire and submitting on a later tick when no
session in the workspace is running a turn. Claude Code sessions take that path
until the adapter can bound a missed steer, and the interface says which path a
given session is on rather than implying every trigger interrupts.

**Every fire writes a journal event, and the message names its own origin.**
The transcript has to show why an agent received something nobody typed. A
delivered message must never read as the user speaking; it carries the trigger
that fired and the fact that fired it, so awareness does not depend on a
standing instruction staying fresh.

**Triggers bind per repository and apply to workspaces that have a pull
request.** The watch arms per workspace because a watch is a decision about one
pull request. A trigger is a standing preference about how the user wants to be
reached, which is the same shape `CodeDeliveryStore`'s rules already have, and
re-arming it per workspace would make the common case the tedious one.

**A trigger targets the workspace's most recently active interactive session,
named in the interface when the trigger is armed.** Workspaces hold several
sessions ([record 55](0055-multiple-sessions-per-workspace.md)), so "the agent"
needs a rule and the rule needs to be visible before it fires. Watch sessions
are never a target: a watch is already acting on the same facts, and delivering
to it would put two drivers on one loop.

**The client-side notification rules move into this substrate.** One rule
engine, server-side, owning both actions. `localStorage` state under
`tidebreak.code-delivery` migrates once on first read and is then authoritative
on the server.

**Anti-loop, inherited from the watch.** A trigger that fires against an
unchanged fact parks rather than retrying, following `last_fix_head`. A fire
that would overlap an active watch on the same workspace is suppressed, or two
agents chase one failure.

**Owner-scoped and code-shaped**, per [record 48](0048-one-interaction-model.md).
Chat inherits triggers at step 5, when a conversation is a session with the
internal engine.

Deliberately excluded: shell commands and webhooks as actions, which need their
own record; scheduled and recurring triggers, which wait for the parked
local-first scheduler in [`docs/deferred.md`](../deferred.md) and should be
hosted here rather than grow a second driver; triggers on chat conversations;
rerunning a failed check before waking an agent, which `RerunFailed` already
offers as a user-initiated delivery action and which wants evidence of flaky
checks first; and re-expressing watch-and-fix as a built-in trigger, which
waits until this substrate has carried real use for a release.

## Alternatives Considered

**Do nothing; let agents keep polling.** Rejected. Polling is the cost this
exists to remove, and it cannot be told to look sooner. It also leaves the
user's own notification rules stranded in `localStorage` with no record.

**Model it as a prompt.** Compose a standing instruction telling the agent what
to watch for. Rejected for the reasons [record 50](0050-watch-and-fix-is-a-durable-task.md)
already established: a prompt dies with the turn, the session, or the app, it
occupies the user's conversation, and it burns context waiting. Those arguments
do not weaken because the condition set got larger.

**Leave the notification rules in the client and build triggers beside them.**
Cheapest — no migration, no schema change to what already ships. Rejected: two
rule engines over the same pull-request facts is exactly the outcome
`docs/deferred.md` warns against for the scheduler, and the two would drift on
scoping, on fingerprinting, and on what "attention" means. The migration is the
price of one engine, and it is a one-way move of a small object.

**Let users write conditions.** An expression language over the digest is more
expressive and would not need a new enum per condition. Rejected for v1: it
turns a bounded product surface into a language with its own errors, its own
evaluation cost on every sweep, and its own security questions, before anyone
has asked for a condition the enum cannot express.

**Arm triggers per workspace, like the watch.** Consistent with the surface
that already exists. Rejected: it makes the standing case repetitive, and it is
not what the rules being absorbed already do.

**Subscribe to the event bus instead of sweeping rows.** Lower latency and no
polling interval to tune. Rejected: the bus is lossy `broadcast`, so a dropped
message is a trigger that silently never fired — the one failure this feature
cannot have.

**Give triggers their own session, as the watch does.** Clean isolation, and
the contention rules are already written for it. Rejected: the point is to
reach the agent the user is working with. A message delivered to a session the
user is not reading is a notification with extra steps.

**Declare `mid_turn_steering: Supported` for Claude Code now.** The capture
shows steering working, and the trigger use case is agentic turns, which have
tool boundaries. Rejected: the failure is silent and it corrupts turn
accounting for the rest of the session, not just for the steer that missed.
[Record 31](0031-harness-adapter-boundary.md) does not allow a capability claim
the adapter cannot honor.

## Consequences

- A baseline schema edit, so `DESKTOP_SCHEMA_EPOCH` moves from 39 to 40
  (`crates/tidebreak-server/src/desktop_schema.rs`). #2427 added a test that
  fails a baseline edit shipped without the bump.
- A one-way `localStorage` migration. A user who downgrades after it runs sees
  their rules on the server and an empty client store; the rules are small and
  re-creatable, which is why this is acceptable rather than merely unavoidable.
- Two automation engines coexist until watch-and-fix is re-expressed. They
  share the pull-request digest cache and the contention guard, and the
  suppression rule above is what keeps them from both acting.
- Claude Code — the reference-tier harness — delivers on a sweep retry rather
  than mid-turn until its adapter bounds a missed steer. Triggers are useful
  without that, but the headline "the agent finds out while it is working"
  only holds on Codex until it lands.
- Every delivered message is a turn the user did not type, so anything reading
  a session's history has to tolerate turns with a non-user origin.

Revisit if a condition the enum cannot express turns out to be the common
request, if per-repository binding proves too coarse for someone running many
workspaces on one repository, or if a harness protocol starts carrying an
acknowledged mid-turn message so delivery no longer splits by capability.

## Validation

- A condition that stays true across several sweeps fires once. A wrong
  implementation that fingerprints on the condition rather than on `head_sha`
  re-fires every tick and still passes any single-tick test, so the assertion
  has to span sweeps.
- A fire against an unchanged head parks rather than retrying, asserted the way
  the watch's `last_fix_head` case is.
- A trigger whose workspace has an active watch does not fire.
- A session on a harness that does not declare `mid_turn_steering` receives the
  message as an ordinary turn on a later sweep, and never through `steer`.
  Asserting only the Codex path would pass while Claude Code silently dropped
  every fire.
- Every fire leaves a journal event naming the trigger and the fact, and the
  delivered turn is attributable to the trigger rather than to the user.
- The `localStorage` migration runs once: a second read after migrating does
  not duplicate rules.
