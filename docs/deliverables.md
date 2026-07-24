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

## Deliberate limits

This baseline derives its durable catalog from the files themselves rather than
adding artifact database rows. An export is a synchronous user action, so it is
not automatically retried and does not yet have a durable export receipt.
Deleting outputs, binary formats, Office document generation, transcript-inline
artifact cards, version history, and writing directly into connected folders
remain later slices. Those additions should preserve the same rule: the model
names a logical output, while a person or narrowly scoped capability chooses
where host data is written.
