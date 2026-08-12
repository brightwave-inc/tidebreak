# Conversation outputs

OpenWave can turn a conversation into a file without giving the model an
arbitrary host write path. The first deliverables slice is intentionally small:
the foreground agent creates bounded UTF-8 files in conversation-private
scratch, the desktop previews them in **Outputs**, and the user chooses a
destination through the native **Save As…** dialog.

## Product flow

1. The user asks for a report, plan, table, data file, or simple web page.
2. The foreground agent produces the file with code execution, saving it under
   the workspace's `output/` directory.
3. After each command, OpenWave scans `output/` and publishes what it finds as
   durable outputs: a new filename creates an output, and the same filename with
   changed bytes appends a new version of the same output.
4. The native Outputs view lists safe metadata and returns a bounded preview.
5. **Save As…** snapshots the complete file and writes it only to the path the
   user selects.

Generated outputs remain private app data until that final explicit export.
Switching conversations switches the catalog; guessing another chat ID or a
private scratch path does not widen access. Deleting the conversation also
removes its private output directory; cleanup rejects replacement root or chat
symlinks rather than following them.

## Closed first-slice contract

- Curated text formats — Markdown, plain text, CSV, JSON, and HTML — publish as
  text outputs; any other extension publishes as a binary artifact with a
  media type derived from its extension, bounded at 16 MiB.
- Filenames are one portable ASCII component, at most 120 characters.
- Content is valid UTF-8, non-empty, and at most 512 KiB.
- The catalog returns the newest 100 valid output files.
- Text previews return at most 100,000 Unicode characters. Export always uses
  the complete file.
- Formats with an inline viewer — spreadsheets and CSV (Univer), Word documents
  (docx-preview), PDFs, and images — load the revision's complete bytes and
  reuse the same engines as source documents.
- Markdown previews use the same safe renderer as assistant messages: raw HTML,
  local-file links, executable URL schemes, and remote image loads are not
  rendered. Plain text, JSON, and HTML outputs use a syntax-highlighted source
  view (HTML is never executed). Unsupported binary types (for example ZIP)
  remain export-only.

The output directory is a capability-confined child of private per-chat
scratch. Native reads reject symlinks and non-regular or oversized files.
Exports use a temporary regular file and atomic rename, reject symlink
destinations, and preserve the permissions of an existing regular destination.
Neither the model nor renderer receives the scratch path, the selected export
path, bearer credentials, or native-executor credentials.

## The durable output record

The filename-keyed catalog above cannot express version history: writing a
filename twice replaces the earlier bytes and there is nothing left to recover.
The product store therefore owns an output record whose identity is an opaque
`OutputId`, with the filename demoted to display metadata.

- An output has an opaque id, an owning conversation, a display filename, a
  fixed media type, a current revision, and a revision count.
- A revision has its own opaque id, a one-based ordinal, an exact byte length,
  a SHA-256 digest, and the producer that created it. A producer is either a
  foreground turn or a background run, never both — recorded in two mutually
  exclusive nullable references so existing turn-produced revisions are
  unchanged and a background run can now be attributed just as precisely. Only a
  turn producer may carry retrieval citations, which resolve against the turn's
  evidence. Revision rows are insert-only.
- Updating an output appends a revision and republishes the current one. The
  replaced revision keeps its own id and stays readable, so an update can no
  longer destroy the bytes it supersedes.
- Revision bytes live at `outputs/<output id>/<revision id>` under the exact
  conversation's private scratch. The path is derived only from durable
  identity, so a display filename can never steer where bytes are written, and
  a revision file is written once and never replaced.
- History is bounded at 100 revisions per output. Reaching the bound refuses
  the write rather than discarding the oldest revision, because silently
  dropping history would reintroduce the loss the record exists to prevent.
- Deleting an output is a soft delete: it leaves the catalog but keeps its
  revisions until the conversation itself is deleted. Deleting the conversation
  cascades the records away, and the existing private-scratch cleanup removes
  the bytes with the rest of the chat directory.

Every mutation is keyed by a caller-minted identity. Reusing an identity with
identical content returns the original record, so an ambiguous store response
can be retried without creating a second output or a second revision; reusing
one with different content is rejected.

This layer is what the exec `output/` scan writes into. Files created before
the record existed under the legacy `artifacts/` directory predate the record
and are not adopted into it.

## Accepting binary workspace artifacts

An execution provider with a durable workspace (see
[sandbox providers](sandbox-providers.md)) can produce a file the conversation
should keep — a rendered chart, a generated PDF, a spreadsheet. Such a file is a
*proposal*, not an output: it enters the record only when the host **accepts**
it, and acceptance is a host-side operation the model cannot perform for itself.

- The host pulls the bytes back with the workspace capability's bounded file
  read, then accepts them into an output. Acceptance publishes the bytes at the
  same write-once revision path a text deliverable uses, so an accepted binary
  artifact is cataloged, addressed, and exported exactly like any other output.
- A binary artifact carries an **explicit media type** rather than one derived
  from its filename, and its filename obeys the same portable-ASCII rules with
  no text-extension requirement. The declared media type is a bounded,
  well-formed `type/subtype` token and may not masquerade as one of the curated
  text types.
- Binary artifacts are bounded at 16 MiB — the same ceiling the workspace file
  read enforces, so every file the workspace is willing to hand back is
  acceptable and acceptance never has to reject a well-formed artifact. The text
  path keeps its 512 KiB cap; each output's media type fixes which ceiling its
  revisions use.
- The producing background run is recorded on the accepted revision, so a
  workspace artifact's provenance is a run rather than a foreground turn.

Acceptance changes nothing about export: a person still chooses the destination
through **Save As…**, and the bytes never leave private app data until then.

## A background agent's own files

A background agent produces nothing by narration. It runs commands in its own
private workspace, writes deliverables under `output/`, and each file it writes
there is published to the conversation's output record by the same scan the
foreground exec path uses — keyed by filename, so writing the same name again
appends a version rather than forking a second output. The host never authors a
result document on the run's behalf and never invents a title.

- The run finishes by calling `done` with the filenames it wants the reader to
  receive and a short summary. Submission resolves those names against the
  conversation's live outputs and records the pair; it creates nothing, so a run
  cannot submit a file it never wrote.
- The published revision records the background run as its producer, exactly
  like an accepted binary artifact, so the agent-run surface can correlate a
  completed run with the outputs it produced.
- Submission is bounded: at most 16 filenames and a short summary. The summary
  is prose beside the files, not a substitute for them.
- A run that legitimately produced no file submits no filenames; a folder
  proposal or a cancellation is not conversation content and never becomes an
  output.

## Versioning, restore, and delete

The `output/` scan is safe — for foreground turns and background runs alike —
because every published output
is a durable, append-only version history the user can always walk back:

- **Version history.** Each output's detail panel lists its versions once there
  is more than one — who produced each (a turn, a background run, or the user)
  and when. Viewing an old version is a preview; the current version stays
  current.
- **Restore is append-only.** Restoring an old version republishes its content
  as a *new* head version (restoring v2 while at v5 produces v6), so nothing is
  rewound, renumbered, or lost, and a restore can itself be undone by another
  restore. Restoring content that is already current is a no-op. The appended
  revision carries no producer, which durably marks it as a user action.
- **Editing is append-only too.** Markdown and plain-text outputs can be edited
  in place in the detail panel; Save publishes a new user-authored version
  rather than rewriting the bytes on screen, and the version it started from
  stays readable at its own id. The save carries that starting version as a
  precondition, checked inside the same transaction that publishes: if an agent
  turn or background run published a newer version while the editor was open,
  nothing is written and the reader is offered the version that won. Content
  obeys the same bounds as an agent-written text output — non-empty, UTF-8, no
  NUL, at most 512 KiB — and the structured text types (CSV, JSON, charts,
  HTML) and binary artifacts are not editable, because a free-text edit of
  those is as likely to break the document as to fix it.
- **Delete is explicit and soft.** Deleting an output hides it from the catalog
  while retaining every revision; the catalog offers an inline Undo that
  restores it exactly.

All three are host actions the model cannot perform, and none is destructive:
the revision history is insert-only, so restore and delete only move pointers a
person can move back.

## Deliberate limits

An export is a synchronous user action, so it is not automatically retried and
does not yet have a durable export receipt. Office document generation,
transcript-inline artifact cards, per-revision source references, and writing
directly into connected folders remain later slices. Those additions should
preserve the same rule: the model names a logical output, while a person or
narrowly scoped capability chooses where host data is written.
