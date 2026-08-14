# 23. Office documents use local Extend viewers

- Status: Proposed
- Date: 2026-08-14
- Owners: Desktop document preview
- Related: 0020-spreadsheets-use-a-native-read-only-workbook-surface
- Supersedes: none

## Context

Tidebreak has treated each office format as a separate rendering problem. PDFs
use a custom single-page pdf.js surface, DOCX files use `docx-preview`, and
presentations are converted to PDF with LibreOffice before they can be read.
The resulting controls, navigation, loading behavior, and visual quality differ
substantially by format. Presentation conversion also flattens slides and makes
LibreOffice installation part of the ordinary preview path.

The spreadsheet viewer established a better boundary: a specialized local
renderer owns document layout and interaction, while Tidebreak owns loading,
security, citations, read-only policy, and the surrounding product chrome. PDF,
DOCX, and PPTX previews need the same boundary. They must continue to work
without uploading source files or mutating the bytes that Tidebreak exports.

## Decision

PDF, DOCX, and PPTX previews use the current Extend registry viewer surfaces and
their exact-pinned local rendering engines. The registry source lives in the
desktop UI repository so Tidebreak can adapt integration details without
forking document parsing or rendering behavior. XLSX continues to use the
existing exact-pinned Extend workbook engine described in decision 0020.

The surfaces are viewing-only. Upload and download controls inside the embedded
viewers are disabled because Tidebreak already owns file selection and export.
DOCX rendering explicitly uses the engine's read-only mode. Presentation and
PDF display controls may change viewport state, such as zoom or rotation, but
never write a modified file back to the source or replace its exported bytes.
All document viewers use a fixed light document surface.

Tidebreak remains responsible for fetching the immutable source bytes, creating
and revoking local blob URLs, reporting transfer failures, applying citation
navigation, remembering PDF page state, and opening only HTTPS external links
outside the application. PDFium WebAssembly is bundled as an application asset;
the preview does not fetch a rendering engine from a CDN.

PPTX files render directly with the Extend presentation engine. Legacy PPT and
OpenDocument presentations continue through the LibreOffice-to-PDF path. A
PPTX that the direct engine cannot render also falls back to that path so the
new primary renderer does not remove an existing compatibility route.
LibreOffice conversion preserves the original source bytes and remains a
fallback rather than the normal PPTX experience.

## Alternatives Considered

- **Keep the three existing viewers.** This avoids dependency churn, but keeps
  the inconsistent controls, weak DOCX navigation, single-page PDF experience,
  and mandatory presentation conversion that prompted the change.
- **Convert every office format to PDF.** This can improve print fidelity for
  some files, but removes native document structure, selection behavior, slide
  navigation, workbook inspection, and format-specific controls.
- **Use hosted Microsoft, Google, or third-party viewers.** Hosted viewers add
  document upload, identity, availability, and network dependencies to a local
  desktop reading path.
- **Build complete PDF and OOXML renderers in Tidebreak.** Owning the entire
  parser, layout, font, drawing, and navigation stack would offer maximum
  control, but is disproportionate to a read-only product requirement and
  would duplicate maintained rendering engines.
- **Remove LibreOffice immediately.** Direct PPTX rendering covers the common
  format, but not legacy PPT or ODP, and has not yet earned the right to be the
  only recovery path for difficult decks.

## Consequences

Office previews gain consistent toolbars, thumbnails, search or page
navigation where the engine supports them, continuous virtualized reading, and
format-native rendering. The desktop bundle grows to include PDFium and the
DOCX/PPTX engines. Viewer source copied from the registry becomes maintained
application code: registry upgrades must be deliberate, reviewed diffs rather
than an unexamined installer command.

The rendering engines are product-critical dependencies. Their licenses,
worker and WebAssembly loading, webview compatibility, document-link behavior,
and failure modes require review on every upgrade. LibreOffice remains an
installation and release concern while presentation fallback exists.

Revisit this decision if the engines stop being maintained, important files
lose fidelity, the local asset model becomes incompatible with the desktop
runtime, security review rejects their parsing boundary, or Tidebreak adds
editing whose persistence model cannot be layered on this read-only contract.

## Validation

- PDF, DOCX, PPTX, and XLSX files open with the same family of local viewer
  controls and no viewer-owned upload or download action.
- A PDF citation opens and scrolls to its recorded page; subsequent scrolling
  updates Tidebreak's remembered page and any registered page controls.
- PDF search, text selection, thumbnails, continuous scrolling, zoom, and
  display-only rotation work with a bundled PDFium asset and no CDN request.
- DOCX renders in read-only mode with page thumbnails, comments and tracked
  changes available for inspection, while unsafe links cannot leave the app.
- PPTX renders directly with slide thumbnails and navigation. PPT and ODP files,
  plus a PPTX direct-render failure, use the existing LibreOffice fallback.
- Replacing a source revokes its stale blob URL, and exported bytes remain the
  exact original bytes regardless of preview interactions.
