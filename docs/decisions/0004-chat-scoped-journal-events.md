# 4. Chat-scoped journal events

- Status: Accepted
- Date: 2026-08-10
- Owners: conversation storage
- Related: `crates/openwave-core/src/storage/store.rs`, `crates/openwave-core/src/db/ops/conversation.rs`, `crates/openwave-server/src/routes/compaction.rs`

## Context

Every event in a chat's journal has, until now, belonged to a turn. The two
append paths say so: `append_turn_event` requires a live turn claim and an
attempt ordinal, and the legacy `append_event` refuses outright once a chat has
any durable turn history. The renderer reads the journal — live over the socket
and again on replay — so anything it must be told about has to be in there.

Compaction run on the user's request is the first thing the agent does for a
chat while no turn exists. It is deliberately between turns: it rewrites what
the next model call will see, so it cannot run under one. Its two status events
(`CompactionStarted`, `CompactionFinished`) are the same events the renderer
already handles during a turn, and they describe the chat rather than any
particular turn.

The schema already permits this. `event.turn_id`, `lease_token`, and
`attempt_event_ordinal` are all nullable, and the table's checks require a turn
id only for terminal events and require a lease token only when a turn id is
present. What was missing was a `Store` method that would write such a row for
a chat that has turns.

## Decision

A chat's journal may carry non-terminal events that name no turn, written
through `Store::append_chat_event`. The rules:

- **Non-terminal only.** Terminal events resolve a turn; a turn-less row could
  not name the one it resolved, and the schema forbids it. The store rejects a
  terminal event on this path rather than relying on the caller.
- **Sequence allocation takes the chat write lock**, the same lock turn
  acceptance takes, so a maintenance append and the next turn's admission
  cannot race for a number. Ordering within the journal stays total.
- **The renderer needs no new vocabulary.** These are existing event variants;
  what changed is only that a turn is not their owner.
- **Not a general-purpose escape hatch.** Anything that happens *during* a turn
  still appends through that turn's claim, so stale attempts stay fenced. This
  path is for work the user asked for between turns.

## Alternatives Considered

- **Return the outcome in the HTTP response only, and journal nothing.** The
  requesting window would show the result and every other view of the chat —
  another window, the same chat reopened later, the transcript's compaction
  divider — would show a conversation that silently changed shape. The journal
  is where the renderer's account of a chat comes from; leaving it out would
  make this the one piece of agent work that is invisible in it.
- **Wrap the pass in a synthetic turn.** It would reuse the claim machinery, at
  the cost of a turn row that ran no model conversation, produced no message,
  and would appear in turn listings, usage totals, and retry affordances as if
  it had. The lie would spread further than the feature.
- **Relax `append_event` instead of adding a method.** Its refusal of chats with
  durable turns is what keeps legacy direct-execution callers from writing into
  a durable chat's journal. Widening it would remove that guard for every
  caller to buy one new one.

## Consequences

- Journal readers must not assume `turn_id` is present. In practice none did —
  `list_events` never read it, and the renderer projects from the payload.
- Anything that later reconstructs turn-by-turn history from events must join
  on `turn_id` and tolerate rows without one.
- The window between "no turn is running" and the append is not held under a
  lock: a turn accepted while the compaction pass is in flight will interleave
  its events with these. That is harmless for the status pair, which the
  renderer treats as a flag it sets and clears, but a future chat-scoped event
  whose meaning depends on turn ordering would need more than this.

Revisit if a chat-scoped event ever needs to be ordered against a specific
turn's events, or if a second maintenance path wants to write while a turn is
running — either would mean this path needs its own fencing rather than the
"between turns" rule the caller enforces today.

## Validation

- `tests::compaction::compacting_on_request_checkpoints_the_chat_and_journals_it`
  reads the pair back out of the journal after the route returns, which a
  response-only implementation would fail.
- `tests::compaction::compaction_is_refused_while_a_turn_runs` holds the
  "between turns" rule the ordering argument above rests on.
- `tests::compaction::compacting_a_chat_with_nothing_to_give_up_says_so` asserts
  that a pass which never started journals nothing, so the events keep meaning
  "a summarization call ran" rather than "someone asked".
