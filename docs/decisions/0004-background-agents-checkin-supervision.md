# 4. Background agents are supervised by check-ins, not stopped by caps

- Status: Accepted
- Date: 2026-08-10
- Owners: agent runtime (sandbox background runs)
- Related: [0002](0002-pre-v1-schema-and-persisted-format-mutability.md);
  `crates/openwave-core/src/agent_tools.rs`,
  `crates/openwave-server/src/sandbox_agent_run_worker/`
- Supersedes: —

## Context

A background sandbox run replays its whole durable tool-call chain on every
claim, so early designs bounded the chain aggressively. By August 2026 a run
was subject to five distinct limits: a 16-row work budget, an 8-row
`update_task_plan` budget, a one-row refusal reserve patched in after three
production runs died at the work cap *holding finished work* (PR #1829), an
8-call per-step batch cap with trim-and-refuse machinery, and a 1-hour
wall-clock deadline. Meanwhile the loop guard the runtime already had —
`max_steps = 100`, which withdraws parking tools two steps early and ends with
a written answer rather than a failure — had never once fired for a background
run, because the row budgets always bound first.

The facts that forced the rethink:

- The production failures were legitimate document-generation tasks that
  needed ~16 tool calls. The cap was cutting off real work, not loops.
- Context growth, the row budgets' stated rationale, is per *step* (one
  assistant message plus one result message per step), not per row. Steps were
  always the right unit.
- Rows cannot grow without steps growing: every parked batch is preceded by a
  model completion, and the step guard already bounds completions. A separate
  row bound is implied by the step bound.
- A single completion's batch size is physically bounded by `max_tokens`; the
  per-step cap defended against nothing while requiring refusal machinery for
  the batches it trimmed.
- Real delegated work routinely deserves more time and more calls than any
  fixed number picked in advance. A hard cap is a bet that the picker knows the
  task better than the agent's supervisor does.

## Decision

One *policy* number governs a background run: the **check-in cadence**
(`agents.sandbox_agent_checkin_steps`, default 100 model steps, user-editable
in Settings). Reaching it is not failure and not termination — the run wraps up
if it can, and otherwise reports to its parent for direction. A second policy
*trigger* — `agents.sandbox_agent_error_checkin`, default 5 — escalates early
when the trailing N tool calls all resolved as errors, because a run whose
tools keep failing needs help long before its cadence expires. Both triggers
feed the same escalation path; neither kills work.

Every other mechanism must be one of:

- **Mechanical**: a storage or lease invariant that no real workload — healthy
  or not — ever observes, and that never messages the model. Lease expiry
  (dead-worker reclaim) is the canonical example.
- **A failure counter**: `max_attempts` counts worker/provider crashes, never
  productive steps.

Deliberately excluded: the row budgets and refusal reserve (deleted), the
per-step batch cap and its trim machinery (deleted), store-side re-enforcement
of policy bounds (the store keeps only invariants it owns, such as
no-unresolved-siblings), and wall-clock deadline enforcement for background
runs (every failure mode it claimed is owned by a sharper mechanism: dead
worker → lease expiry; looping model → cadence; erroring tools → error
trigger; judgment calls → the parent or the user).

Nothing in the runtime may stop a run that is doing real work. Only the
foreground agent or the user halts work.

## Alternatives Considered

- **Raise the work budget to 100 and keep the split budgets.** Rejected: keeps
  three numbers doing one job, keeps the refusal reserve patch, and re-poses
  the same question at the new cap. The plan/work split and the reserve exist
  only because the budget unit and the storage unit were the same object.
- **Make each row budget configurable.** Rejected: multiplies knobs without
  changing the failure mode — a cap that kills finished work is not improved
  by being adjustable.
- **Repetition heuristics (consecutive identical calls) as the loop brake,**
  as the sibling product's `unraveling_prevention` does, optionally with an
  LLM adjudicator. Rejected: repeated near-identical `exec` calls are what
  legitimate build work looks like, so the false-positive surface is large;
  and an LLM adjudicator is nondeterministic, which breaks the invariant that
  a replayed claim rebuilds an identical request from durable rows. The
  error-receipt trigger keeps the useful part (a thrashing run escalates
  early) with none of the guesswork: error receipts are durable facts.
- **Do nothing** (keep #1829's reserve fix). Rejected: the reserve made the
  crash impossible but left the cap cutting off real work at 16 calls.

## Consequences

- A looping-but-"productive" run can now burn up to one cadence window of
  model calls before anyone hears about it. That window is the price of never
  killing real work, and it is user-tunable.
- Aggregate spend remains bounded by `max_active_background_agents` and by
  supervision; there is no per-run hard stop. Anyone operating unattended
  fleets should set the cadence accordingly.
- The store no longer double-checks policy bounds, so a worker bug that
  ignored the step guard would write more rows than intended before the next
  claim's guard caught it. Accepted: the double-check only ever re-derived the
  worker's own arithmetic, and disagreement between the two copies is what
  killed the three production runs.
- Escalation (`needs_input`, parent resume/cancel) lands in a follow-up
  change; until it does, reaching the cadence ends with the existing
  wrap-up-with-`done` behavior.

Revisit if: per-row context growth inside a step (not step count) is shown to
blow the context window in practice; or unattended runs are observed burning
full cadence windows on loops that guidance does not fix.

## Validation

- The #1829 regression test survives, retargeted: a tool call made after the
  cadence is spent is refused with a readable receipt and the run completes
  rather than failing.
- Settings round-trip tests for both knobs, with defaults and bounds.
- The wrong implementation to guard against: re-introducing a silent kill —
  any path that turns cadence exhaustion into `Failed` without a check-in
  should fail review against this record.
