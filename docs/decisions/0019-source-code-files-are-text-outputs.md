# 19. Source-code files are text outputs

- Status: Proposed
- Date: 2026-08-14
- Owners: Outputs
- Related: `docs/deliverables.md`
- Supersedes: none

## Context

Files written directly under an execution workspace's `output/` directory are
the agent's explicit user-visible deliverables. The output scanner currently
classifies only Markdown, plain text, CSV, JSON, HTML, and chart JSON as text.
A source file such as `example.py` therefore becomes an opaque binary output in
a foreground turn, while a background run refuses a small hard-coded set of
source extensions as presumed helper scripts.

The desktop already has a safe syntax-highlighted source viewer for text
outputs. The missing classification prevents source-code deliverables from
reaching it and makes foreground and background execution disagree about the
same file.

## Decision

Recognized source-code and text-configuration filenames under `output/` publish
as bounded UTF-8 text outputs with media type `text/plain`. This includes common
language, shell, stylesheet, data-definition, and configuration extensions, as
well as conventional extensionless build files such as `Dockerfile`, `Makefile`,
and `Justfile`.

The desktop selects syntax highlighting from the filename, falling back to the
media type and then plain text. Source is displayed, never executed.

Background runs no longer reject source extensions. The location remains the
intent boundary: a file directly under `output/` is a proposed deliverable;
helper scripts belong elsewhere because the model-facing tool contract says so,
not because the host guesses intent from an extension.

The existing 512 KiB, valid-UTF-8, non-empty, no-NUL text-output contract still
applies. Unknown extensions retain the existing binary-artifact behavior.

## Alternatives Considered

- **Keep source files export-only.** This preserves the current classifier but
  wastes the existing safe source viewer and makes a common requested output
  impossible to inspect before export.
- **Add one distinct media type per programming language.** This would encode
  more information in the persisted record, but would multiply mirrored
  allowlists across Rust, HTTP, and TypeScript. The filename already survives
  end to end and is the conventional language signal.
- **Continue suppressing source files only for background runs.** This avoids
  accidental helper outputs, but it also makes delegated code-generation tasks
  unable to submit their actual result. Directory placement is the clearer and
  provider-neutral intent signal.
- **Treat every valid UTF-8 unknown extension as text.** This is simpler but
  changes opaque-file behavior too broadly and risks misclassifying formats
  whose first bytes happen to decode. A curated filename set keeps the boundary
  reviewable.

## Consequences

Source-code outputs gain bounded previews, syntax highlighting, version history,
editing, and export through the existing text-output path. A background agent
that incorrectly places a helper script under `output/` can now expose it in the
catalog, so prompts and skill instructions must continue to keep intermediates
outside that directory.

Existing source files previously recorded as `application/octet-stream` keep
their stored media type and remain export-only; output media types are fixed per
record. Tidebreak is pre-1.0 and does not migrate those rare records.

Revisit this decision if output submission gains a staged manifest that can
distinguish final files from intermediates before publication, or if language
services require a persisted language identity rather than filename inference.

## Validation

- Core tests prove representative source filenames map to `text/plain`, obey
  the text ceiling, and publish from both foreground and background scans.
- Desktop tests prove the filename selects the expected highlight language and
  that a source output reaches the source viewer rather than the export-only
  fallback.
- Existing tests continue to prove unknown extensions remain binary artifacts.
