# 6. Mid-turn sends queue by default; steering is the explicit alternative

- Status: Accepted
- Date: 2026-08-10
- Owners: chat turn lifecycle, desktop composer
- Related: [0005](0005-checkin-needs-input.md); `crates/openwave-core/src/db/ops/turn/queued.rs`
- Supersedes: —

## Context

Sending while a turn is active had exactly one meaning: steer. The composer's
Enter always redirected the running model (`interrupt: true`), and
`POST /chats/{id}/messages` refused a busy chat with `409 ChatBusy` — nothing
queued anywhere, so a user with three follow-ups either derailed the current
turn three times or babysat the spinner. Steering is fully durable and
boundary-applied server-side; queueing did not exist in any layer.

## Decision

**Queue is the default mid-turn send; steer is the deliberate alternative.**

- A durable `queued_turn` row per queued message: the row id *is* the client
  turn id promotion will accept under. `POST /messages` gains `queue: true`,
  turning `ChatBusy` into a durable park of the already-validated body.
  FIFO by a dense per-chat `position`; capped at 32 per chat.
- **Promotion is try-based, not fenced.** A sweep (750 ms) walks chats holding
  queued rows and *tries* the ordinary idempotent turn acceptance under the
  row's own id, deleting the row only on success. `ChatBusy` leaves it for
  the next sweep; a crash between acceptance and deletion re-runs into
  `Existing`; a row whose attachments or skills no longer resolve is dropped
  (its turn could never be accepted), while an unusable chat model leaves the
  queue intact so fixing the model releases it.
- The tray above the composer renders the rows with edit, delete, reorder,
  and per-chat pause (a settings key, `chats.{id}.queue_paused` — promotion
  skips paused chats).
- The composer's mid-turn button is mode-switched (Queue ↔ Redirect), and the
  choice persists per install (localStorage): this is composer posture, not
  chat state, and does not merit a server setting yet.

Deliberately excluded: "send now" on a queued row steering its content into
the live turn (retract + steer compose today by hand); drag reordering
(buttons cover it); steering switched to `interrupt: false` (Redirect keeps
its meaning until the queue default has soaked).

## Alternatives Considered

- **Client-side draft buffer** that auto-sends when the chat frees. Rejected:
  lost on quit, invisible to other clients, and racing the 409 it papers over.
- **Fenced server-side promotion** (a lease-holding promoter committing
  acceptance and deletion atomically). Rejected: acceptance is already
  idempotent under the row's id, which makes try-then-delete crash-safe for
  free; a new fenced path would re-prove what `AcceptTurnOutcome::Existing`
  already proves.
- **Steer-by-default, queue as the modifier** (today's behavior, softened).
  Rejected: Enter mid-turn derailing the model is the papercut this exists to
  fix; interjecting should be the deliberate act.

## Consequences

- A queued message runs later under settings resolved *at promotion* (model,
  managed policy) — the honest reading of "run this when you're free".
- Dropped rows (dead attachment, removed skill) are logged, not surfaced in
  the transcript yet; the tray simply stops showing them. Revisit if users
  report silent loss in practice.
- The promoter is poll-based at 750 ms; if that cadence ever matters, wire it
  to the turn-resolution wake instead.

Revisit if: multiple clients need a shared send-mode default (promote the
localStorage choice to a setting), or promotion-order guarantees beyond FIFO
are wanted (priorities, scheduling).

## Validation

- Store: enqueue caps and idempotent retry; FIFO promotion order; retract and
  reorder keep positions dense.
- End-to-end: queue two messages mid-turn, watch both run in order after the
  live turn resolves; pause holds promotion, resume releases it.
- The wrong implementation to guard against: promotion deleting the row
  before acceptance commits — a crash there loses a message; the delete must
  follow `Accepted`/`Existing`.
