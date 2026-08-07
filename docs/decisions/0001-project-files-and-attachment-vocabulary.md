# 1. Project Files, Chat Attachments, and the Vocabulary for Both

- Status: Proposed
- Date: 2026-08-07
- Owners: desktop
- Related: [How OpenWave works](../how-openwave-works.md) (document model),
  [Tool architecture](../tools.md) (the `list_sources`/`read_source` pair),
  [Host access and connected folders](../host-access.md) (the grant subject a
  conversation carries, and the creation-time root snapshot this record
  deliberately diverges from)

## Context

A `Project` is an optional parent of a chat. Chats can now be filed under one,
moved between projects, and addressed through the project holding them. Folders
and their grants already scope project-wide, because the grant subject is
derived from the conversation's project. Documents do not follow, and that is
the gap this record closes.

**What a document is today.** An owner, an origin URI, a media type, a
content-addressed blob of the original bytes, and `canonical_text` — decoded
synchronously at ingest — plus a readiness flag that admits the honest case
where bytes were retained but no text could be extracted. The files-first
simplification removed the retrieval stack that used to sit on top: vector
search, chunk embeddings, byte-span citations, the pluggable parser trait, and
asynchronous ingestion. What remains is closer to an attachment ledger with a
decoded-text column than to a corpus. Nothing ranks documents and nothing
embeds them; the entire query surface is `list_sources`, a flat listing capped
at 100, and `read_source`, a bounded character range.

**Ownership is in the identity, not in a filter column.** A document is owned by
a chat or by a project, never both, and `DocumentId::derive_for_chat(chat, uri)`
and `DocumentId::derive_for_project(project, uri)` hash the same file to
different ids. Any design that moves a document between owners is therefore a
migration, not an update.

**The project half is wired but unreachable.** `POST`/`GET
/projects/{id}/documents` and their raw and file-content variants all exist.
Nothing calls them: the source tools are hard-wired to the chat scope, and the
desktop client only ever builds `/chats/…` paths.

**Four words, overlapping.** The area has accumulated vocabulary faster than it
has accumulated behavior:

- *source* — the tools (`list_sources`, `read_source`), `SourceReadiness`,
  `source_uri`, `source_blob`, `source_tools.rs`, and the documentation for
  `import_connected_file`;
- *document* — the storage record, the HTTP routes, `DocumentId`,
  `DocumentScope`;
- *attachment* — the composer's "Attach files", the transcript — and,
  separately, `chat_root_attachment`, `root_attachments`, and
  `attachment_revision`, which are about **connected folders** and have nothing
  to do with files the user handed over;
- *file* — the private-scratch tools `read_file`, `write_file`, `list_dir`, and
  the execution sandbox's filesystem.

The *attachment* collision is the worst of these: two unrelated meanings, one
word, reachable from the same struct.

## Decision

### Two product concepts

**Project files** belong to the project and are readable by every chat in it.
**Chat attachments** are handed to one conversation and readable only there.

### Sharing

1. **Union at read, live.** A chat inside a project reads the project's current
   files plus its own attachments. Project files are resolved at read time, not
   snapshotted at chat creation.

   This is a deliberate divergence from ordered root defaults, which *are* a
   creation-time snapshot. The two relationships look identical and have
   opposite requirements: a folder grant is *authority*, which must stay pinned
   to what was actually approved, while a project file is *shared material*,
   whose entire value is that adding one reaches conversations already under
   way. Recorded here so the two are not later "made consistent" with each
   other.

2. **Uploads land on the chat; promotion is explicit.** Dropping a file into a
   conversation attaches it to that conversation. Making it a project file is a
   separate, deliberate act. Silently publishing an attachment to every sibling
   chat is the kind of surprise that teaches people to stop attaching things.

3. **A move carries nothing.** Moving a chat into or out of a project leaves
   its attachments exactly where they are; project files union in on top. No id
   re-derivation, no rewrite, no migration. This follows the same rule as the
   move itself, which refuses rather than silently breaking what it cannot
   carry, and the same rule as project deletion, which unfiles chats rather
   than deleting them.

4. **Chats still do not read each other.** The project's file set is the only
   thing that crosses a conversation boundary.

### Vocabulary

- **`Document` remains the storage record** — the durable row, its blob, and
  its canonical text. It is not a product word and does not appear in the UI.
- **`source` is retired** as a product and tool word:
  - `list_sources` → `list_documents`, `read_source` → `read_document`. Not
    `list_files`/`read_file`: `read_file` is already the private-scratch tool,
    and the sandbox filesystem owns *file* in tool vocabulary.
  - `SourceReadiness` → `DocumentReadiness`, `DocumentSourceBlob` →
    `DocumentBlob`, `source_uri` → `origin_uri` — that last one records where
    the bytes came from, which is the one sense of the word worth keeping and
    is clearer once it is the only one.
- **`attachment` becomes a product word** for one conversation's files, and
  stops meaning connected folders in code. Renaming `chat_root_attachment`,
  `root_attachments`, and `attachment_revision` to a binding vocabulary is
  schema work and is settled with that change rather than here; it is named now
  because leaving both meanings in place is the single most confusing thing in
  this area.
- **In the interface**, a project page lists *Files*; a conversation shows
  *Attachments*.

### Bounds

`list_documents` stays bounded, but per scope: a conversation's own attachments
are listed first and counted separately from project files, so a busy project
cannot crowd a chat's own three files out of the listing. Exact caps are set
with the implementation.

## Alternatives Considered

- **Snapshot project files at chat creation**, mirroring root defaults. Buys
  consistency with the folder mechanism at the cost of the feature: a file
  added to the project would never reach a chat started yesterday, which is the
  case people actually have.
- **Auto-promote chat uploads to the project.** Makes every attachment a
  publication, in a surface where the user cannot see the blast radius at the
  moment they drop the file.
- **Re-derive document ids on move**, converting a moved chat's attachments
  into project files. Rewrites ownership the user never asked for, and is the
  one path here that can lose an attachment outright if it half-fails.
- **`list_files`/`read_file` as the tool names.** Rejected on collision with
  the private-scratch tools.
- **Do nothing.** A project would organize its conversations but hold none of
  their material, which is most of what a project is for.

## Consequences

- **Renaming tools touches a persisted contract.** Tool names are recorded in
  the journal and replayed to providers, so historical calls must keep
  replaying under the names they were made with. Pre-1.0 is the moment to
  absorb this; the replay-time renaming already used to avoid vendor
  web-search collisions is the mechanism.
- **The project needs a page.** Explicit promotion requires somewhere to
  promote to, and project files need somewhere to be listed and removed. This
  is the largest cost in the record and is UI work, not plumbing.
- **The flat bounded listing is a known ceiling.** With no retrieval left, a
  project holding more files than the cap has no graceful degradation — it
  truncates. Accepted deliberately: the alternative is rebuilding ranked
  selection for a problem nobody has yet.
- Documents still cannot be shared *between* projects, and unowned documents
  remain a legacy case only.
- **Revisit this** when a real project exceeds the listing cap — that is the
  trigger to reconsider ranked selection rather than to raise the number — or
  if material needs to outlive a single project, which would argue for a shared
  library rather than a project-scoped set.

## Validation

- A chat moved into a project keeps its attachments readable and unchanged, and
  reads the project's files alongside them; moved back out, it keeps them
  still.
- A file promoted to a project becomes readable in a chat created *before* the
  promotion. A snapshot implementation passes every other check here and fails
  this one, which is the point of stating it.
- A turn recorded before the rename still replays into a well-formed provider
  request.
- A project holding more files than the listing cap does not starve a
  conversation's own attachments out of `list_documents`.
