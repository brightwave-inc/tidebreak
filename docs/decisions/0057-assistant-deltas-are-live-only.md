# 57. Assistant Deltas Are Live-Only, and Journal Replay Is Bounded

- Status: Accepted
- Date: 2026-08-21
- Owners: code mode, wire contract
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0035-code-mode-wire-contract.md`](0035-code-mode-wire-contract.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

`code_event` is append-only with no pruning, and most of what it holds says
nothing. Across eight driven code-mode sessions, delta rows were 63% to 93%
of the table by lane:

| Lane | Rows | Delta rows | Share |
|---|---|---|---|
| codex | 354 | 329 | 92.9% |
| grok | 536 | 487 | 90.9% |
| opencode | 244 | 205 | 84.0% |
| claude_code | 108 | 68 | 63.0% |

The duplication is exact. Concatenating a turn's `assistant_delta` texts
reproduces the `assistant_message` that follows it byte for byte — 38 of 38
messages across all eight lanes, no mismatches. Deltas average 42–46 bytes, so
each one pays roughly twice its own size in per-row overhead to store text the
next event already carries in full.

Three costs follow:

- **Storage.** Around 200 KB of the 340 KB the table occupied in a light test
  profile was delta rows.
- **Replay.** `list_events` had no bound, so a fresh events-socket connect
  deserialized every row the session had ever journaled.
- **Write path.** Every event is one `INSERT` inside a transaction that takes
  the session row lock and scans for the current maximum sequence, and every
  activity event then issued a `get_session` to decide whether a stall needed
  clearing — usually to return early.

The renderer already treats deltas as transient. `CodeSessionReducer`
accumulates them into a buffer and the message replaces the buffer wholesale,
so replaying stored deltas rebuilds a string the next event overwrites.

Two constraints shape the answer. [`0030`](0030-code-mode-separate-surface.md)
requires the code journal to keep the chat journal's event-table shape,
sequencing rule, journal-before-live-publication discipline, and
snapshot→replay→live contract, and it calls a second streaming idiom out of
bounds without a record saying why the shared shape cannot serve. And
`reasoning_delta` looks like the same case but is not: no `reasoning_message`
exists, so those deltas are the only record of reasoning there is.

## Decision

**Assistant deltas are live-only.** `assistant_delta` is published on the
session bus and never journaled. Everything else keeps the existing
discipline: journaled first, published second, under the spawn-epoch fence,
with `(session_id, seq)` monotonic and gap-free.

**The frame says which kind it is.** `SequencedCodeEventFrame` gains
`transient: Option<bool>`. A transient frame carries the journal cursor it
streamed behind rather than a position of its own; a client applies it, does
not advance its resume cursor past it, and does not expect it back on
reconnect. This is the chat socket's existing shape for out-of-band frames
(`RendererChatFrame::Metadata`) rather than a new idiom — one socket, two
frame classes, one cursor.

**The bus holds the tail, so mid-turn reconnect is still correct.** Between
the first delta and the message that states the whole answer, the only copy of
that text is in memory. The bus keeps it per session, bounded by
`MAX_EVENT_TEXT_CHARS` — the same bound the message carrying it will get — and
retires it on any journaled event that ends the run: the message, a
parent-level tool call, a turn boundary. A connecting socket takes the tail
and subscribes under one lock, so no delta is counted twice, and it forwards
the tail only when its replay did not read past the journal position the tail
was captured at.

**A turn that ends mid-sentence gets the message the engine owed it.** When a
turn reaches a terminal event with text still buffered, the server journals it
as an `assistant_message` before the terminal event. Synthesizing the message
is safe where replaying the deltas would not be: both the renderer and the CLI
treat a message as a *replacement* for text they already streamed, so a client
that has it shows it once.

**Replay is bounded and says when it truncated.** `list_events` takes a limit
and returns a page. It keeps the *newest* events above the cursor —
`MAX_REPLAY_EVENTS = 2000` — and the first frame of a capped window carries
`truncated: true`. The renderer turns that into a transcript line saying
earlier history is not shown.

**`reasoning_delta` is unchanged.** The duplication that justifies dropping
assistant deltas does not hold for reasoning: there is no durable event that
restates it, so dropping those rows would lose the thinking accordion's
contents on reload rather than deduplicate them.

**Stall detection reads memory before it reads rows.** The bus tracks each
session's last publication, live or journaled, so a session pouring out a long
answer and touching nothing else is not mistaken for silent. It also carries a
pessimistic "might be stalled" hint that every row read corrects, so the
common activity event costs no query.

Deliberately excluded: deleting delta rows already written (they stay
readable), a durable `reasoning_message` event, and any pruning of the journal
after the fact.

## Alternatives Considered

**Delete a turn's delta rows once its message lands.** Keeps the wire
untouched, and keeps mid-turn reconnect exactly as it is. Rejected: it pays
the insert *and* a second write, which is the opposite of the write-path cost
this record is about; it needs either a JSON predicate the two backends
express differently or a read-then-delete round trip; and it leaves holes in
the sequence that every future reader of the journal has to be correct about.
Never writing the row costs nothing and leaves the sequence dense.

**Drop the deltas with no live tail.** Simplest possible change. Rejected on
the reconnect story: a reader who opens the pane mid-answer would see the
sentence from wherever they arrived until the message landed, and the CLI —
which never reprints a message it believes it already streamed — would lose
the head of that answer permanently.

**Give transient frames no `seq` at all**, as a separate untagged frame kind.
Rejected: existing clients set their resume cursor from `frame.seq`, and a
frame without one would either reset that cursor to zero or need every client
changed in lockstep. Carrying the cursor makes the change backward-compatible
for readers that ignore the new field.

**Coalesce deltas into one row per run instead of dropping them.** Would fix
reasoning too. Rejected here: the coalesced row has to be journaled but *not*
delivered to clients that already streamed its text, and the moment it is not
published the sequence gains a gap that sends every connected socket back to
the journal to fetch exactly the row it must not apply. Making that safe needs
per-frame coverage bookkeeping on the wire, which is a larger contract change
than this problem justifies.

**Do nothing.** Rejected: the table grows without bound, and the unbounded
replay is a latent problem on its own — a long-lived session makes every
connect more expensive than the last.

## Consequences

The code journal now holds a strict subset of what the chat journal holds for
the same conversation: chat still journals `TextDelta`. That is a real
divergence from [`0030`](0030-code-mode-separate-surface.md)'s convergence
constraint, and it is the reason this record exists. It is narrow on purpose —
the table shape, the sequencing rule, the fence, and the reconnect contract
are all unchanged, so a future unified journal is still a table merge. If chat
adopts the same treatment, the transient frame class is already defined and
the tail already lives in the bus.

The bus stops being a pure fan-out. It now owns per-session live state — the
streamed tail, the cursor, last-activity, the stall hint — which means it is
the thing to look at when a live/replay disagreement shows up, and it is
per-process: two servers fronting one database would each hold their own tail.
Code mode is single-process today; this is a constraint on that changing.

A synthesized `assistant_message` is a server-authored event. It says what the
engine streamed, not what the engine declared, and the journal does not
distinguish the two.

Replay truncation is visible but lossy: a session past the cap opens on its
newest 2000 events. Anything that needs the whole journal — the debug bundle,
the fork transcript — now reads a bounded window too and will silently stop at
the same cap.

Revisit this when chat and code journals converge, when an engine appears
whose messages do not restate their deltas (the byte-for-byte finding is the
whole foundation here), or when reasoning gains a durable finalize event and
the same treatment becomes available to it.

## Validation

- A turn's journal holds its `assistant_message` and no `assistant_delta`
  rows, while the same turn's deltas arrive on the socket marked transient —
  a wrong implementation that stopped publishing them too would pass every
  durable assertion alone.
- A socket that connects after some deltas have streamed and before the
  message lands assembles the same text as one that was connected the whole
  time.
- A turn interrupted mid-sentence leaves the streamed text in the journal.
- A journal written before this change — delta rows and all — still replays in
  order.
- A replay past the cap carries `truncated` on its first frame only, and keeps
  the newest events rather than the oldest.
- `list_events` with a window that fits exactly reports no truncation, which a
  naive `limit(n)` implementation would get wrong.
