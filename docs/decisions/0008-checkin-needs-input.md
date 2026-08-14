# 8. Check-ins pause a run in `needs_input` and ride the result machinery

- Status: Accepted
- Date: 2026-08-10
- Owners: agent runtime (sandbox background runs)
- Related: [0005](0005-background-agents-checkin-supervision.md) — defines the
  two triggers this record gives a destination
- Supersedes: —

## Context

Record 0005 replaced hard caps with two escalation triggers (step cadence,
consecutive tool errors) but left the escalation itself unbuilt: reaching the
cadence still ended in a forced wrap-up. The parent's `wait_for_agents` is a
strictly fenced receipt machine — the wait settles only on `agent_run_inbox`
rows joined to immutable `agent_run_result` receipts by exact lease identity —
and a paused child that produced neither would stall a parked parent
indefinitely.

## Decision

A check-in **is a submission**, through the same code path, receipt table, and
inbox delivery as a terminal result — with two differences: the payload is the
new `AgentRunResultPayload::CheckIn { reason, steps_used, detail }`, and the
run lands in the new non-terminal, non-claimable `AgentRunStatus::NeedsInput`
instead of `Completed`. The parent's wait therefore consumes a paused child
exactly like a finished one and reads a typed reason.

Resuming (`POST /chats/{chat}/agent-runs/{run}/resume`, and the run panel's
Resume control) deletes the check-in receipt and its inbox delivery — the
run's single result slot must be free for the outcome it eventually produces —
then durably records the grant (`checkin_grants`, multiplying the cadence
window) and the row watermark (`checkin_watermark`, so the errors one check-in
reported cannot re-trigger the next), stamps a fresh stall window, and appends
any guidance to the run's own task text, the one durable instruction stream
every claim rebuilds the transcript from. Cancellation from `needs_input`
clears the check-in receipt the same way before writing its own.

Deliberately excluded from this slice: foreground model tools to resume or
cancel a child (`resume_agent` / `cancel_agent`) — the parent model *sees* the
check-in through its wait and can say so, but acting on it is the user's,
via the run panel, until the tools land as their own change.

## Alternatives Considered

- **A parallel settlement path for paused children** (wait scan learns to
  settle on run status alone, no receipt). Rejected: it forks the invariant
  the whole wait machine rests on — one settled child, one exact receipt — and
  every consumer would carry both cases forever.
- **Check-in as a terminal completion** (child completes with a typed payload;
  parent respawns to continue). Rejected: the child's workspace and transcript
  are its identity; a respawn starts from nothing and the "resume with
  guidance" the feature exists for becomes a re-run.
- **A separate guidance table** instead of appending to the task text.
  Rejected for now: a second transcript source complicates replay for no v1
  gain; the task text already replays deterministically. Revisit if guidance
  needs positioning mid-transcript rather than at its head.

## Consequences

- `agent_run_result` stays one-row-per-run; a resume must delete before the
  run can finish. A stale worker retrying a check-in submission after a resume
  fails the lease fence and acknowledges, so the slot cannot be re-occupied.
- A parent that calls `wait_for_agents` again on a still-paused child (its
  check-in already consumed) waits until a human acts — there is no timeout by
  design (0004). The run panel is the pressure valve.
- Schema epoch bumped (pre-v1 rebuild per 0002): new status, two new columns,
  widened check constraints.

Revisit if: parents routinely need to act on check-ins without a human (build
the orchestration tools), or guidance-at-head measurably underperforms
guidance-in-place.

## Validation

- End-to-end worker test: cadence spent → `CheckedIn` → `needs_input` with
  grants 0 → resume with guidance → grants 1, guidance in task text → run
  completes under the widened window.
- The wrong implementation to guard against: settling a wait on a paused child
  without a consumable receipt, or resuming without deleting one — either
  leaves the next real result unable to land.
