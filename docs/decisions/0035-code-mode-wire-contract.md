# 35. Code-Mode Wire Contract: Per-Session Journals Plus One Updates Channel

- Status: Proposed
- Date: 2026-08-15
- Owners: code mode, wire contract
- Related: [`0030-code-mode-separate-surface.md`](0030-code-mode-separate-surface.md),
  [`0004-chat-scoped-journal-events.md`](0004-chat-scoped-journal-events.md),
  [`docs/wire-types.md`](../wire-types.md),
  [`docs/code-mode.md`](../code-mode.md)

## Context

Code mode's defining UI problem is plural: a user runs several sessions at
once and needs, simultaneously, (a) a full live transcript for the session
they are looking at, and (b) a cheap, always-current summary of every other
session — lifecycle, whether it needs them, what its pull request is doing.
Prior art in this category tends to answer (b) badly: every session looks
equally important, "blocked on you" is indistinguishable from "working", and
the user becomes the polling loop.

Chat mode already has a proven streaming contract for (a): a per-chat journal
table, sequence numbers, journal-before-live-publication, and a WebSocket
whose reconnect discipline is snapshot → replay from cursor → live
([`0004`](0004-chat-scoped-journal-events.md),
`crates/tidebreak-server/src/routes/events.rs`). It has no contract for (b)
beyond per-chat metadata notices.

All renderer-facing types are generated from Rust
([`docs/wire-types.md`](../wire-types.md)); anything this record defines
rides that pipeline.

## Decision

**Per-session journals, identical discipline.** Code sessions journal into
their own table (`code_event`: session id, monotonic per-session sequence,
event payload), written before any live publication. The per-session
WebSocket (`/code/sessions/{id}/events?after=`) implements exactly the chat
contract: subscribe live first, snapshot, replay `seq > after`, then live,
with sequence-based dedupe. One socket exists per *open* session view; a
session nobody is looking at streams to no one.

**One install-wide updates channel.** A single WebSocket (`/code/updates`)
carries unsequenced digest notices: per-session
`{workspace, session, lifecycle, attention, title, turn count, pull-request
state}`. Digests are restated in full on connect, so a dropped notice costs
nothing and no cursor is needed. The channel drives the sidebar, badges,
list views, and OS notifications for any number of sessions without a socket
per session.

**Attention is computed server-side.** So that the desktop, the CLI, and
notifications agree — and so notifications work with no window open — every
session carries a server-computed attention state:

- `Working` — the harness is doing something.
- `NeedsYou { prompt, source }` — an approval, question, or failure is
  waiting on the user.
- `Stalled { idle for }` — running but silent past a threshold.
- `DoneUnreviewed` — finished work the user has not looked at.
- `Fenced { reason }` — crash recovery parked it
  ([`0032`](0032-code-workspaces-worktrees-checkpoints.md)).
- `Manual { note }` — the user pinned a state by hand.

Each automatic state carries its `source` — `Structured` (an exact protocol
event), `Heuristic` (an inference such as idle time), or `Lifecycle` (a state
machine fact) — and the replacement rule is fixed: a structured signal is
never second-guessed by a heuristic on the same facts, and a `Manual` state
is never overwritten by any automatic source, only by the user. The
attention vocabulary is defined over "a unit of supervised work", not over
code sessions specifically, so chat can adopt it unchanged when the modes
converge ([`0030`](0030-code-mode-separate-surface.md)).

**Journal rows are bounded.** Large payloads — diffs, file lists, raw
harness payloads — never ride the journal. Events carry hints (ids,
diffstats) and the renderer loads bodies from bounded GET routes, following
the chat journal's precedent for plans and previews.

**Everything is generated wire.** All types here derive into the generated
TypeScript alongside the chat types, under the same staleness test, with
hand-written runtime validators at the renderer boundary in the existing
pattern.

Deliberately excluded: multiplexing full event streams over one socket, and
any renderer-side attention computation beyond display.

## Alternatives Considered

**One multiplexed socket carrying every session's full stream.** Rejected:
per-session replay cursors over one connection is new protocol machinery,
and only open views need full streams; the digest channel serves everything
else at a fraction of the traffic.

**Cursor-pull for session events** (server notifies "changed", client pulls
from a byte or event cursor). Workable — and adopted for terminals in
[`0036`](0036-code-mode-auxiliary-terminals.md), where bytes are ephemeral —
but rejected for the journal: push-with-replay is the proven idiom here, and
two streaming idioms for the same kind of durable data is a cost with no
buyer.

**Reuse the chat `event` table with a discriminator column.** Rejected: it
foreign-keys code sessions into chat id space, exactly what
[`0030`](0030-code-mode-separate-surface.md) forbids, and buys only a table
name.

**Poll for digests.** Rejected: N sessions × M clients polling is the
failure mode this record exists to avoid, and restated-on-connect push is
simpler than it looks — it is the existing metadata-notice pattern widened
to the install.

**Renderer-computed attention.** Rejected: the CLI and OS notifications need
the same answer with no renderer running, and two implementations of "does
this need me" will disagree in exactly the cases that matter.

## Consequences

The updates channel becomes a small install-wide broadcast hub in the
server; its digests must stay cheap to compute and bounded in size, which
the hint-shaped journal discipline already forces.

Attention thresholds (the stall timer) become product tuning knobs on the
server; changing them changes every client at once, which is the point.

Because the journal is bounded and hint-shaped, some renderer views require
a follow-up GET; view code must treat hint-then-fetch as the normal path,
not an error path.

Revisit this decision if the modes converge (the digest channel and
attention vocabulary should then widen to chats rather than being
reinvented), or if per-session sockets prove too heavy on very large session
counts — the digest channel already carries the data a coarser UI would
need.

## Validation

- Reconnect tests mirroring the chat events suite: replay-then-live handoff
  with no gap and no duplicate across the seam, under concurrent writes.
- A digest-restatement test: connect, receive full state; mutate sessions;
  reconnect; receive full current state with no dependence on missed
  notices.
- The generated-types staleness test covering every new type.
- An attention property test: no sequence of automatic transitions ever
  replaces a `Manual` state, and a `Structured` `NeedsYou` is never
  downgraded by a `Heuristic` source while the structured condition stands.
- A plausible wrong implementation publishes live events before the journal
  write commits and passes happy-path streaming tests; the reconnect test
  must inject a crash between publish and commit to fail it.
