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

**Four actions, all through the server.** Each is a chat route, and the CLI
has a command for each (`chat retry`, `chat regenerate`, `chat edit`,
`chat branch`):

- `POST /chats/{id}/turns/{turn_id}/retry` continues the latest turn after it
  failed or was stopped.
- `POST /chats/{id}/turns/{turn_id}/regenerate` answers the latest message
  again, optionally with another model for that turn only.
- `POST /chats/{id}/turns/{turn_id}/edit` replaces the latest message and
  answers the new one.
- `POST /chats/{id}/turns/{turn_id}/branch` starts a new conversation with a
  copy of the history through any settled turn.

**A rerun is an ordinary turn that names the turn it reruns.** Retry,
regenerate, and edit admit a new turn through the same admission path a
message takes, with a copy of the old turn's input (or the edited input). The
new `turn` row records `replaces_turn_id` and `replacement` (`retry`,
`regenerate`, or `edit`), written in the same transaction that accepts the
turn. Only the chat's latest settled turn can be rerun, and only a turn that
did not finish can be retried; the checks run under the chat lock, and a
unique index on `replaces_turn_id` backs them.

**A retry continues; it does not replace.** The retried turn stays in the
conversation, for the model and for the reader. The model reads everything
it said, every tool call it made, and every result, then the same message
again with a note that the attempt above stopped before it finished and that
what already ran is done. So a turn that sent an invoice and then lost its
provider is continued with the send in view, not started over blind. The
transcript shows the message once: the copy a retry sends is left out, and
the retried turn's message carries the retry's turn id so an edit acts on the
turn that can be rerun. The retried turn's tool activity, file changes,
memory chips, and notice stay. Only a retried turn that left nothing but its
notice drops the notice, because the answer below it says the same thing. A
retry and the turns it retried form one attempt
(`tidebreak_core::TurnPlacements`).

**A regenerated or edited attempt leaves the model's view, and its rows
stay.** Context assembly, the approval judge, memory capture, and chat
titling leave out every turn of a regenerated or edited attempt. The renderer
projection places each turn by how its attempt was replaced:

- A regenerated attempt is an earlier version of the answer now shown. The
  transcript lists it under `answer_versions` with everything it said and
  did, and the desktop pages through versions ("2 of 3"). While an earlier
  version is on screen, the desktop says the conversation continues from the
  latest one.
- An edited attempt is gone from the conversation, and so is every earlier
  version of it: they answered a message that no longer exists.

**A regenerate or an edit that would erase a record of action starts a new
conversation instead.** When the attempt being replaced, or any earlier
answer to the same message, called a tool that does more than read, the
rerun branches before that attempt and sends the message in the branch. The
rule fails closed: every call counts unless the reader declined it, it never
ran, or its tool is on a reviewed list of tools that only read
(`READ_ONLY_TOOLS` in `routes/turn_rerun.rs`). A tool added later counts
until someone reviews it. The original keeps its record of what ran, which
nothing undoes either way. The answer says so with `branched` and
`side_effects`, and the transcript carries the latest turn's `side_effects`,
so the desktop says so before anything is sent: in the editor for an edit,
and in the Regenerate menu for a regenerate.

**A compaction summary goes where its view went.** A checkpoint summarizes
the whole view it was written from, recent turns included, so it records the
latest turn of that view (`through_turn_id`). Accepting a regenerate or an
edit drops the checkpoint when that turn is part of the attempt being
replaced, the save of a checkpoint whose view a rerun overtook meanwhile is
refused, and a checkpoint is never projected while that turn is out of the
conversation. A checkpoint written before this was recorded falls back to
its time.

**A branch copies what existed at its point; it does not point back.**
Branching copies, in one transaction, the settled turns of the conversation
as it stands through the branch point (a retry together with the turns it
retried), their messages, settled tool calls, attachments, citations, the
documents added before the point and the files the branch's first message
carries, and its image publications. Documents come along under new ids, and
every mention of an old id in the copied model context is rewritten. The
compaction checkpoint comes along only when the latest turn it summarized was
copied. The new session records `branched_from_session_id` and
`branched_from_turn_id` as a plain link, not a foreign key. It is named after
the original (`<title> (branch)`) and keeps its model and settings. Branching
from an earlier version copies that version. When a rerun's branch refuses
its first message, the branch is removed, together with the folders its
project gave it: nothing ran in it, so no folder change was ever made for it.

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

**Retry as a regenerate.** Answering a failed turn again with the failed
turn taken out of the model's view never stacks copies of the question, but
the model loses every tool call the failed turn made. A turn that sent an
invoice before its provider failed would send it again. Rejected.

**Retry by sending the message again as a new turn.** It keeps the calls in
view, but shows the question twice and leaves the chain of attempts
invisible to an edit or a regenerate that follows. Rejected in favor of a
retry that names the turn it continues.

**A list of tools that act.** Listing the tools known to act outside the
conversation fails open: a tool added later, or one nobody thought of, is
edited in place and its record disappears. The reviewed list is of tools
that only read, so a new tool fails closed. Rejected.

**Judge a checkpoint by its time alone.** Dropping every checkpoint written
after a replaced turn started also drops the clean ones written after the
replacement, so the chat would compact again on every turn. Recording the
latest turn the summary saw ties the checkpoint to its view. Rejected.

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
  columns and a unique index to `turn`, two nullable columns to `session`,
  and one nullable column to `context_checkpoint`. Existing rows need no
  backfill.
- Every reader of a chat's rows for the model has to filter the turns out of
  the conversation. Context assembly, the approval judge, memory capture, and
  chat titling do. Readers that authorize by identity (image and tool-result
  access) deliberately do not, so an earlier version's images still load.
- The transcript wire adds `turn_id` to messages and tool activity,
  `answer_versions`, and `side_effects` on the latest turn. Clients that ignore
  them see the conversation as it stands.
- The model reads a retried request twice, the second time with a note. A
  model that ignores the note could still repeat an action; the note and the
  results in view make that unlikely, and the reader sees every call either
  way.
- A branch's copied turns keep their answers but not their streamed reasoning
  summaries, which live only in the journal. Output cards in copied history
  point at the original's outputs, which the branch does not own.
- A tool that only reads has to be added to `READ_ONLY_TOOLS` after review,
  or a regenerate or an edit of a turn that used it starts a new conversation.
- Revisit if people want to edit an earlier message, not only the latest: that
  makes replacement a mid-history rewrite, and both the model filter and the
  branch copy would need to treat the conversation as a tree.

## Validation

- `tests::turn_rerun` in `tidebreak-server` drives the routes end to end
  against a recording provider: a regenerate keeps one question and one
  earlier version, and the model never sees the replaced answer; a retry
  after a failure that did nothing shows one question and its answer; a retry
  after a tool call and a provider error keeps the call and its result in the
  model's view and in the transcript, sends the invoice once, and a
  regenerate after it answers in a new chat; an edit of a turn that only
  talked replaces it in place; an edit or a regenerate of a turn that acted
  starts a new chat, says why, and leaves the original alone; an edit drops a
  checkpoint that summarized the edited turn and refuses to store one written
  before it; a branch leaves behind a checkpoint and a file from after its
  point; a branch whose first message is refused is removed even in a project
  with folders; retry with another model answers under it and leaves the
  chat's model alone.
- `routes::turn_rerun` pins that every tool counts as acting unless it only
  reads, including a tool nobody has reviewed.
- `model::turns::replacement_tests` pins the attempt rules: an edit anywhere
  along a chain discards every answer before it, and a retry keeps the turn
  it retried in the conversation until a regenerate or an edit replaces both.
- The conformance matrix answers `404` to another owner for all four routes.
- `regenerate_and_branch_rerun_and_fork_a_chat` in `tidebreak-cli` runs the
  commands against an embedded server with a scripted model.

A plausible wrong implementation that hides a replaced turn from the renderer
but not from the model passes a transcript assertion. The route tests assert
on what the provider received, not only on what the transcript shows.
