# Conversation outputs

OpenWave can turn a conversation into a file without giving the model an
arbitrary host write path. The first deliverables slice is intentionally small:
the foreground agent creates bounded UTF-8 files in conversation-private
scratch, the desktop previews them in **Outputs**, and the user chooses a
destination through the native **Save As…** dialog.

## Product flow

1. The user asks for a report, plan, table, data file, or simple web page.
2. The foreground agent calls `create_deliverable` with a portable filename and
   complete text content.
3. OpenWave atomically creates or replaces that filename under the exact
   conversation's private output directory.
4. The native Outputs view lists safe metadata and returns a bounded preview.
5. **Save As…** snapshots the complete file and writes it only to the path the
   user selects.

Generated outputs remain private app data until that final explicit export.
Switching conversations switches the catalog; guessing another chat ID or a
private scratch path does not widen access. Deleting the conversation also
removes its private output directory; cleanup rejects replacement root or chat
symlinks rather than following them.

## Closed first-slice contract

- Supported formats: Markdown, plain text, CSV, JSON, and HTML.
- Filenames are one portable ASCII component, at most 120 characters.
- Content is valid UTF-8, non-empty, and at most 512 KiB.
- The catalog returns the newest 100 valid output files.
- Previews return at most 100,000 Unicode characters. Export always uses the
  complete file.
- Markdown previews use the same safe renderer as assistant messages: raw HTML,
  local-file links, executable URL schemes, and remote image loads are not
  rendered. HTML outputs are shown as text rather than executed.

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
  a SHA-256 digest, and the turn that produced it. Revision rows are
  insert-only.
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

This layer is the foundation the model-facing and native surfaces move onto. It
is not yet wired to `create_deliverable`, which still writes a filename into
`artifacts/`. Files already in `artifacts/` predate the record and are not
adopted into it: they stay on disk and remain listed by the current catalog
until that surface moves to the record.

## Deliberate limits

An export is a synchronous user action, so it is not automatically retried and
does not yet have a durable export receipt. Binary formats, Office document
generation, transcript-inline artifact cards, per-revision source references,
and writing directly into connected folders remain later slices. Those
additions should preserve the same rule: the model names a logical output,
while a person or narrowly scoped capability chooses where host data is
written.
