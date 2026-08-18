# 46. Rich Turn Input Rides Harness Capabilities

- Status: Proposed
- Date: 2026-08-18
- Owners: code mode
- Related: 0031 (adapter capability honesty), 0033 (approvals never paraphrase),
  0038/0039 (modes as declared capabilities), docs/code-mode.md

## Context

Code-mode turns today are plain text: `POST /code/sessions/{id}/turns`
accepts `{message, model}`, and the composer offers nothing the wire cannot
carry. Three requested inputs go beyond that:

- **Image attachments** (including paste). Some engines accept images on
  their machine-readable input (Claude Code's streamed JSON input does);
  others do not, or only on some versions.
- **Slash commands.** Engines have their own command vocabularies (skills,
  built-ins). Tidebreak deliberately has none of its own for code mode:
  inventing a parallel vocabulary would fork the engine's.
- **`@` file references.** Engines resolve workspace-relative paths in
  prompts; what the composer needs is completion, not a new reference syntax.

The forces: 0031 requires every per-engine difference to be a declared
capability, honestly `Unsupported`/`Unknown` when unproven by fixtures. The
model registry carries the analogous honesty rule today — nothing advertises
image input before the path carries it. And the transcripts are journaled
with bounded rows (0035), so attachment bytes cannot ride the journal.

## Decision

Three additive, capability-gated contracts. Nothing here changes existing
turns; a text-only client remains fully valid.

1. **Attachments on turn submission.** The turn body gains an optional
   `attachments: [{blob_id, media_type}]`. Bytes go through the existing blob
   store first; the journal records only `{blob_id, media_type, byte_len}` on
   the user turn (bounded, per 0035). A new `HarnessCaps.image_input` gates
   it end to end: the route rejects attachments for a session whose adapter
   does not declare `Supported` (`unsupported_attachment` error naming the
   harness), and the composer never offers the affordance — refusal with a
   reason, never a silent drop. Adapters may only declare `Supported` with a
   fixture proving the engine consumed an image on the machine-readable
   input path.
2. **Slash commands are pass-through plus discovery.** Text starting with
   `/` is sent verbatim — the engine owns its vocabulary. A new
   `HarnessCaps.slash_commands` plus an optional `HarnessProbe.commands`
   list (name + one-line description, captured from the engine's own
   listing where a machine-readable one exists) feeds the composer popup.
   No Tidebreak-defined commands, no interception, no rewriting; a harness
   without discovery still accepts free-typed `/` text.
3. **Worktree completion for `@`.** A new bounded route
   `GET /code/workspaces/{id}/tree?query=&limit=` returns git-tracked and
   untracked-but-unignored paths (name-matched, capped, never file
   contents), served by the existing worktree layer. The composer inserts a
   plain relative path — no link syntax, no metadata; the engine sees only
   text.

Deliberately excluded: non-image attachment types (no engine fixture yet),
Tidebreak-side command execution, resolving `@` references to contents
client-side, and any journaling of attachment bytes.

## Alternatives Considered

- **Base64 attachments inline on the turn body.** Simpler wire, but it puts
  megabytes through the journaled submission path and forces a second bound
  system; the blob store already exists and already has GC.
- **A Tidebreak-owned slash vocabulary translated per engine.** Rejected for
  the same reason 0031 rejects pairwise translators: every engine addition
  multiplies the mapping, and paraphrasing a command is exactly the
  dishonesty 0033 forbids for approvals.
- **Client-side directory walking for `@` completion** (no new route). The
  UI cannot see the worktree (webview, no fs access), and the CLI shelling
  out per keystroke has no bound; a capped server route is the only place
  ignore rules are already applied.
- **Do nothing.** Keeps the composer text-only. Rejected: paste-an-image and
  reference-a-file are the two highest-frequency inputs in comparable
  supervision UIs, and both are engine-supported today behind a flag we
  simply do not carry.

## Consequences

Costs: a schema bump for the attachment reference on `code_turn.user_input`'s
journaled shape; three new capability flags across four adapters (each
starting `Unknown` until fixtures exist — visible as absent affordances);
one new route with its own bound; composer surface area. The blob-store GC
must learn that code-turn references pin blobs.

This constrains future engines: an adapter cannot ship attachments without a
fixture, so a new engine's image support arrives dark until captured — that
is the point.

Revisit if: an engine ships attachments only over a non-machine-readable
path (would force a rethink of `image_input`'s meaning); the command
discovery lists grow user-configurable entries (would need namespacing); or
convergence with chat (0030) unifies turn submission, at which point this
contract should merge into the chat attachment model rather than diverge.

## Validation

- Route tests: attachments rejected with `unsupported_attachment` for a
  declaring-`Unsupported` adapter; accepted and journaled as bounded
  references for a scripted adapter declaring `Supported`.
- Adapter honesty: the denylist-style test that no adapter declares
  `image_input: Supported` without a fixture directory containing an image
  round-trip capture (the wrong implementation this catches: flipping the
  flag optimistically — it would pass every UI test and fail only here).
- Tree route: bounded (limit respected under a pathological worktree),
  ignore rules applied, never returns file contents.
- Composer DOM tests: affordances absent when caps are `Unsupported`/
  `Unknown`; paste with support produces an attachment chip; `/` popup lists
  probe-discovered commands and still submits free text without them.
