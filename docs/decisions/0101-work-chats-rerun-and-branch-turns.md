# 101. Work chats rerun and branch turns

- Status: Accepted
- Date: 2026-09-23
- Owners: chat, server
- Related: [`0007-cli-headless-feature-parity.md`](0007-cli-headless-feature-parity.md)
  (every action is a server route with a CLI command),
  [`0048-one-interaction-model.md`](0048-one-interaction-model.md)
  (chats are sessions; the chat routes are still their own handlers),
  [`0055-multiple-sessions-per-workspace.md`](0055-multiple-sessions-per-workspace.md)
  (Code mode's fork of a transcript into a fresh context),
  [`0061-schema-changes-are-migrations.md`](0061-schema-changes-are-migrations.md),
  [`0100-the-1-0-compatibility-surface.md`](0100-the-1-0-compatibility-surface.md),
  migration `m20260924_000003_turn_versions_and_branches`
- Supersedes: none

## Context

A Work chat had no way to take back or redo a turn. The chat routes had no
edit, regenerate, truncate, or fork route. The desktop offered only Copy, and
only on answers. Retry appeared only when a transcript ended on a failure, and
it posted the failed prompt again as a new message, so each retry stacked
another copy of the question. A stopped answer or a fixed provider key had no
retry at all.

Code mode already forks: `POST /sessions/{id}/fork` writes the session's
transcript to a file, and a new session reads it as framing. That shape exists
because an external harness owns its own history and cannot be handed ours.
The internal engine owns its history in our tables, so a Work chat can share
history directly instead of summarizing it into a file.

A turn's input and answer are rows: a `turn`, its `message` rows, its
`tool_call` rows, and its attachments. The model's context is rebuilt from
those rows on every request (`agent/context.rs`), and the renderer's transcript
is a projection of the same rows (`GET /chats/{id}/messages`). Whatever a
rerun does has to change what both of them read, and nothing else.

## Decision

**Three actions, all through the server.** Each is a chat route, and the CLI
has a command for each (`chat regenerate`, `chat edit`, `chat branch`):

- `POST /chats/{id}/turns/{turn_id}/regenerate` answers the latest message
  again, optionally with another model for that turn only.
- `POST /chats/{id}/turns/{turn_id}/edit` replaces the latest message and
  answers the new one.
- `POST /chats/{id}/turns/{turn_id}/branch` starts a new conversation with a
  copy of the history through any settled turn.

**A rerun is an ordinary turn that names the turn it replaces.** Regenerate
and edit admit a new turn through the same admission path a message takes,
with a copy of the old turn's input (or the edited input). The new `turn` row
records `replaces_turn_id` and `replacement` (`regenerate` or `edit`), written
in the same transaction that accepts the turn. Only the chat's latest settled
turn can be replaced; the check runs under the chat lock, and a unique index on
`replaces_turn_id` backs it.

**A replaced turn leaves the model's view, and its rows stay.** Context
assembly and the approval judge filter out every replaced turn's messages,
tool calls, and attachments. The renderer projection sorts replaced turns by
how they were replaced (`tidebreak_core::replaced_turns`):

- A regenerated answer is an earlier version of the answer now shown. The
  transcript lists it under `answer_versions`, and the desktop pages through
  versions ("2 of 3"). An answer that failed or was stopped before it said
  anything is dropped rather than kept as a version, so repeated retries
  never stack.
- An edited turn is gone from the conversation, and so is every earlier
  version of it: they answered a message that no longer exists.

**An edit that would erase a record of action starts a new conversation
instead.** When the turn being edited wrote files, created outputs, called a
connected app or MCP server, or started other work (background agents, code
sessions, app or browser control, a new app), the edit branches before that
turn and sends the edited message in the branch. The original keeps its
record of what ran, which nothing undoes either way. The answer says so with
`branched` and `side_effects`, and the transcript carries the latest turn's
`side_effects` so the desktop says so before you send.

**A branch copies; it does not point back.** Branching copies, in one
transaction, the settled turns of the conversation as it stands through the
branch point, their messages, settled tool calls, attachments, citations, the
compaction checkpoint when everything it covers was copied, the
conversation's documents under new ids (with every mention of an old id in the
copied model context rewritten), and its image publications. The new session
records `branched_from_session_id` and `branched_from_turn_id` as a plain link,
not a foreign key. It is named after the original (`<title> (branch)`) and
keeps its model and settings. Branching from an earlier version copies that
version.

Deliberately excluded: rerunning a turn that is not the latest; keeping edits
as pageable versions; branching Code mode sessions differently (their fork is
unchanged); copying the event journal, outputs and their bytes, the
file-change journal, background agent runs, approvals, standing grants, the
task plan, or connected folders into a branch.

## Alternatives Considered

**Delete the replaced turn's rows.** Simplest for the model's view, but it
destroys earlier versions, punches holes in an append-only journal that
clients replay by sequence, and loses the record of tool calls. Rejected.

**Mark the old turn rather than the new one.** A `superseded_by` column on the
replaced turn reads more directly, but it needs a second write outside the
acceptance insert, so a crash between them can leave two visible copies of a
question or a hidden turn with no answer. Recording the link on the new turn
makes it part of the one insert that accepts the turn. Rejected.

**Rerun without a new user message.** The new turn could reuse the old turn's
message row. Every read path, idempotency check, and attachment binding
assumes a turn owns its input message; sharing one would thread an exception
through all of them. The copied message costs one row and the renderer shows
one bubble. Rejected.

**Branch by reference.** A branch could read its parent's rows up to the
branch point instead of copying them. Every read path — context assembly,
the transcript, document and image access, citations, tool-result reads — would
have to follow the link, and deleting the original would break the branch.
Rejected.

**Reuse Code mode's fork.** Writing a transcript file for a fresh session to
read loses the tool calls, attachments, and exact history the internal
engine can carry itself. It is the right shape for an external harness and the
wrong one here. Rejected.

**Always truncate on edit.** Simpler, but a turn that wrote files or called a
connected app would vanish from the only record of it while its effects stay.
Rejected.

## Consequences

- Migration `m20260924_000003_turn_versions_and_branches` adds two nullable
  columns and a unique index to `turn`, and two nullable columns to `session`.
  Existing rows need no backfill.
- Every reader of a chat's rows for the model has to filter replaced turns.
  Context assembly and the approval judge do. Readers that authorize by
  identity (image and tool-result access) deliberately do not, so an earlier
  version's images still load.
- The transcript wire adds `turn_id` to messages and tool activity,
  `answer_versions`, and `side_effects` on the latest turn. Clients that ignore
  them see the conversation as it stands.
- A branch's copied turns keep their answers but not their streamed reasoning
  summaries, which live only in the journal. Output cards in copied history
  point at the original's outputs, which the branch does not own.
- The side-effect rule reads tool names and the file-change journal. A new
  tool that acts outside the conversation needs an entry in `tool_side_effect`,
  or an edit of a turn that used it truncates in place.
- Revisit if people want to edit an earlier message, not only the latest: that
  makes replacement a mid-history rewrite, and both the model filter and the
  branch copy would need to treat the conversation as a tree.

## Validation

- `tests::turn_rerun` in `tidebreak-server` drives the routes end to end
  against a recording provider: a regenerate keeps one question and one
  earlier version, and the model never sees the replaced answer; a retry after
  a failure keeps no version; an edit of a talking turn replaces it in place;
  an edit of a turn that wrote files and called a connected app starts a new
  chat, says why, leaves the original untouched, and answers a retry the same
  way; a branch copies history through one turn, links back, survives the
  original's deletion, and carries message files under new ids the model is
  told about; retry with another model answers under it and leaves the chat's
  model alone.
- `model::turns::replacement_tests` pins the chain rule: an edit anywhere
  along a chain of reruns discards every answer before it.
- The conformance matrix answers `404` to another owner for all three routes.
- `regenerate_and_branch_rerun_and_fork_a_chat` in `tidebreak-cli` runs the
  commands against an embedded server with a scripted model.

A plausible wrong implementation that hides the replaced turn from the
renderer but not from the model passes a transcript assertion. The route tests
assert on what the provider received, not only on what the transcript shows.
