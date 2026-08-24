# 69. Code sessions queue follow-ups durably, on the chat queue's contract

- Status: Accepted
- Date: 2026-08-24
- Owners: code turn lifecycle, desktop composer
- Related: [0009](0009-queued-turns.md); [0048](0048-one-interaction-model.md);
  [0055](0055-multiple-sessions-per-workspace.md);
  `crates/tidebreak-core/src/db/ops/code/queued.rs`
- Supersedes: — (records 9 and 55 stand; only the single in-memory follow-up
  slot they describe for code sessions is replaced here)

## Context

Decision 9 made queue the default mid-turn send for chat and built the full
contract: durable rows, FIFO promotion, a tray with edit, reorder, delete,
pause, and send-now. Code mode borrowed only the posture. A mid-turn send
parked in a single in-memory slot on the session worker; a second send
refused with `queue_full`; a restart silently dropped the parked message; and
nothing could list, edit, or retract the slot. The desktop showed a bare
"1 follow-up queued" line.

The gap is loudest on pull-request chores. The delivery surfaces offer
per-PR agent actions — fix failing checks, resolve conflicts, address
feedback, update branch — and more than one applies at once, but the second
prompt hit `queue_full` and bounced back into the composer. Decision 48
already names queued turns as a structure chat and code must stop
implementing twice.

## Decision

**Code sessions get the chat queue's contract, stored code-side and promoted
by the session worker.**

- A durable `code_queued_turn` row per queued message: owner-scoped,
  FIFO by dense per-session `position`, capped at 32 per session, carrying
  the message and its image attachments. The row id *is* the turn id the
  promoted turn is inserted under.
- **Promotion is the worker's drain, in one transaction.** A code session has
  exactly one consumer — its worker — so no admission machinery is needed.
  The worker snapshots the FIFO head and `promote_queued_turn` deletes that
  exact row (id, position, `updated_at`) and inserts the running turn
  together. An edit, reorder, or retraction landing after the snapshot makes
  the promotion stale: nothing is written, and the drain re-reads. A crash
  can lose neither side of the pair. Boot recovery re-attaches a worker per
  live session, and the drain runs first, so queues survive restarts.
- A send parks whenever the session is mid-turn, its workspace checkout is
  held by a sibling (record 55), *or the queue has backlog* — FIFO means a
  new send may not overtake parked rows even when the session is idle.
- Settings resolve at promotion, exactly as decision 9 reads it: the turn
  runs under the session's model and effort as they are then, not as they
  were at enqueue.
- The same REST surface as chat, one path segment over: `GET
  /code/sessions/{id}/queued`, `PATCH`/`DELETE
  …/queued/{queued_id}`, `PUT …/queue-paused` (a settings key,
  `code.sessions.{id}.queue_paused`), `POST …/queued/send-now`. The queued
  submit receipt returns the row.
- **One tray component for both modes.** `QueueTray` now takes a five-verb
  adapter (`list`, `update`, `remove`, `setPaused`, `sendNow`); chat and code
  each bind it to their client calls. Code mounts it above the code composer,
  replacing the queued pill, so PR quick actions fired mid-turn land as
  visible, editable, retractable rows. Send-now composes client-side as in
  chat: pause, move first, stop the live turn, release.
- Stop keeps its meaning and loses its data loss: an interrupt that lands
  while a queued turn waits on the workspace checkout declines to start it
  and pauses the queue. The rows hold visibly — the tray shows Paused —
  until resume or send-now releases them. The pause is load-bearing, not
  cosmetic: a sibling's turn ending releases the checkout but wakes nobody,
  so an unpaused hold would stall silently while looking live.

Deliberately excluded: trigger deliveries (they have their own durable
outbox with leases and retries, decision 60); a per-workspace queue
(rejected in record 55 — the queue belongs to the session, and the worktree
lock already sequences siblings); merging the chat and code queue tables
(decision 48 step 5's territory, not this record's).

## Alternatives Considered

- **Widen the in-memory slot to a Vec.** Fixes depth only: still lost on
  restart, still invisible, still unaddressable. Rejected.
- **Reuse chat's `queued_turn` table and promoter sweep.** Crosses the id
  spaces decision 30 keeps apart and drags in the admission table that
  exists only because any process may promote a chat turn. The worker is a
  single consumer; a transaction is strictly stronger than try-based
  promotion there. Rejected.
- **An event-pushed tray instead of polling.** Chat's tray polls at 1.5 s
  while visible; matching it keeps one component and one behavior. Revisit
  together if polling ever matters.

## Consequences

- `queue_full` now means the 32-row cap, not depth one; the composer copy
  changed accordingly.
- A queued row whose start fails (a dead attachment blob, a failed
  preparation) is dropped with a harness notice, like chat's promoter
  dropping unpromotable rows — not retried forever.
- An interrupt aimed at a queued-but-waiting turn pauses the queue rather
  than dropping the message, and the tray's Paused state is the recovery
  path: resume or send-now clears it and wakes the worker.
- The pre-1.0 schema regime applies: one appended migration
  (`m20260824_000011_code_queued_turns`), no baseline edit.

## Validation

- Store: FIFO positions; promotion under the row id; stale promotion after
  edit or retraction writes nothing; dense reorder; the 32 cap; owner
  isolation; pause round-trip.
- End-to-end: two mid-turn sends both park; list, edit, and retraction work
  against live rows; both survivors run in order after the live turn, each
  under its row's id; a stop pressed while a queued turn waits on a sibling
  leaves the message queued and the queue paused, and send-now revives it
  once the checkout is free.
- The wrong implementation to guard against: promotion deleting the row in
  a separate write from the turn insert — a crash between them either loses
  a message or runs it twice. The transaction is the record's core.
