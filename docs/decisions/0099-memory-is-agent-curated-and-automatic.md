# 99. Memory is agent-curated and automatic

- Status: Accepted
- Date: 2026-09-21
- Owners: memory
- Related: [`0067-memory-records-and-scopes.md`](0067-memory-records-and-scopes.md)
  (the record envelope, evidence rule, scopes, and caps this record keeps),
  [`0068-memory-backend-boundary.md`](0068-memory-backend-boundary.md),
  GitHub #3490
- Supersedes: the review lifecycle in decision 0067 (`tracking` and
  `proposed` as gates before authority)

## Context

Decision 0067 made every model-authored memory a proposal. Capture wrote
`proposed` records, weak signals sat as invisible `tracking` hypotheses
until they repeated in a second conversation, and nothing reached a prompt
until the user approved it in a review queue. The reasoning was sound —
model-written entries acquire authority nobody granted — but the product
that came out of it was a review tool with a memory attached. A user with
the feature on for weeks reported that it never accumulated anything, and
that was accurate: four unstated gates stood between a conversation and a
memory, and the surfaces exposed lifecycle states, revisions, a digest
preview, and a byte meter instead of what Tidebreak knew about them.

Agents that people describe as remembering them do something simpler. The
model curates a small, capped store itself with add, replace, and remove;
the store is injected as a frozen snapshot per session; the cap is enforced
by refusing an over-limit write and showing the current entries so the
model consolidates in the same turn; and the user's control is after the
fact — see it, edit it, forget it — rather than a gate in front.

What decision 0067 got right still holds and is kept: a record is a
markdown body with a typed envelope; a model-authored record without
resolvable evidence cannot exist; scopes are personal and repo; caps coach
instead of evicting; every mutation appends a revision; a chat can opt out
with incognito; and the digest pins per prompt boundary for prefix caching.

## Decision

**Model-authored records land active.** The foreground `memory` tool gains
`add`, `replace`, and `remove`; `propose` is gone. `add` writes an `active`
record with model authorship and evidence pointing at the newest user
message of the conversation. `replace` rewrites one record in full by id
and appends a revision. `remove` archives. Post-turn capture in work mode
and in code sessions writes `active` records the same way, and a capture
whose title matches an existing active record rewrites that record instead
of adding a second. The maintenance sweep applies the merge it derives:
the merged record activates and its sources archive with a `superseded_by`
pointer, in one transition the storage layer already owned.

**The prompt tells the model to save proactively and what to skip.** The
memory section instructs the model to save who the user is, how they like
to work, stable environment and project facts, corrections, and reusable
lessons as they come up; to prefer `replace` over a second entry on a
topic; and to skip trivia, rediscoverable facts, raw data, task progress,
secrets, and anything the user asked to keep out. A cap refusal from the
tool carries the current entries so the model consolidates and retries.

**The user's control is after the fact, and always visible.** A turn that
saved memory shows a transcript row naming what was remembered, with Edit
and Forget. The activity chip says how many records reach the
conversation. The settings page is a plain list of what Tidebreak knows,
grouped as About you (preferences) and Notes (facts, lessons, references),
each line editable and forgettable with a link to the conversation it was
learned from, plus one switch and Forget everything. It shows no lifecycle
states, revisions, digest preview, or byte meter.

**Excluded.** `tracking` and `proposed` remain in the enum for stored rows
written before this record and for imports, but no path writes them and
no surface reviews them. Transcript search as a memory tool, repo-scope
capture from chats, and skills as procedural memory are not part of this
record.

## Alternatives Considered

- **Keep the review queue and add an auto-approve setting.** A default-off
  setting leaves the reported problem in place for everyone who does not
  find it, and a default-on setting is this decision with a vestigial
  queue. Rejected.
- **Approve explicit statements automatically, review inferences.** A
  middle tier that keeps two code paths and two surfaces for one feature.
  The capture prompt already declines most turns; caps and undo bound the
  cost of a wrong inference better than a queue nobody opens. Rejected.
- **Drop the record envelope for two flat text files.** Simpler to read,
  but it loses provenance, per-record revisions, repo scope, and the
  evidence rule that makes "why do you think this" answerable. The
  envelope stays; the surfaces stop showing it.
- **Do nothing.** The feature reads as broken to its users. Rejected.

## Consequences

A wrong inference now reaches later prompts until the user forgets it or
the sweep supersedes it. The digest phrases entries as dated claims and
the prompt tells the model the conversation is newer evidence, which
bounds the damage; the transcript row and Forget make the fix one click.
Memory poisoning shifts from a queue problem to a provenance problem:
every entry names its source, and capture reads only the turn's durable
messages, never tool output or fetched pages.

The `MemorySweepOutcome::Proposed` and the `proposed`/`tracking` statuses
stay on the wire for old rows. Client code that filtered on them now
finds nothing.

Revisit if users report more harm from wrong memories than the review
queue caused in missed ones, or if a scope needs a policy stricter than
undo — for example a shared repo scope that several people write to.

## Validation

- The memory tool's `add` lands `active` with evidence and an origin turn;
  `replace` bumps the revision and keeps the id; `remove` archives; a cap
  refusal lists the current entries.
- Capture in work mode lands `active`, rewrites an existing record on a
  matching title, and never re-adds a forgotten title within the
  suppression horizon.
- The sweep's merge activates and archives its sources; a stored run
  reports `merged`.
- The transcript carries each turn's model-authored records as active, and
  the row offers Edit and Forget rather than Approve.
- A plausible wrong implementation would activate the record but skip the
  evidence check; the storage tests that refuse an evidence-less
  model-authored record still stand and cover it.
