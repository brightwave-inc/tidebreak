# 20. Spreadsheets use a native read-only workbook surface

- Status: Proposed
- Date: 2026-08-14
- Owners: Desktop document preview
- Related: none
- Supersedes: none

## Context

An authored workbook is both a visual document and an inspectable model. A PDF
conversion preserves print fidelity but discards cells, formulas, sheet
navigation, frozen panes, and range selection. Reconstructing the workbook into
a general-purpose web spreadsheet from the subset exposed by SheetJS preserves
interaction, but loses drawings, charts, conditional formatting, dimensions,
and other OOXML details before the renderer sees them.

The desktop only needs review in this phase. Editing, recalculation initiated by
the reader, and writing changes back to the source file are deliberately out of
scope. The original workbook must remain the exported artifact.

## Decision

XLSX previews use an exact-pinned, local-only workbook engine that parses OOXML
through WebAssembly and renders a virtualized canvas workbook surface. The
surface receives an in-memory, read-only display copy of the original OOXML,
not a PDF or an intermediate SheetJS model, so it can preserve worksheets,
charts and chart sheets, images, shapes, merged cells, frozen panes, row and
column dimensions, formulas, conditional formatting, tables, and number
formats supported by the engine.

When a formula cell contains Excel's cached result, the display copy renders
that authored result as an ordinary typed cell. The wrapper retains the
original formula separately for the formula bar. This avoids turning valid
workbooks into `#VALUE!` merely because the browser calculation engine does not
implement a function or a producer-specific formula detail. The source bytes
remain unchanged and are still the artifact exported by Tidebreak.

The Tidebreak wrapper always enables read-only mode. It owns loading and error
states, a fixed light document theme, zoom controls, formula and address
display, citation navigation, and the visual contract with the surrounding
document panel.
The display projection may make semantically equivalent OOXML explicit when a
renderer would otherwise misread it, such as expanding an empty default border
or resolving inherited chart series colors from the workbook theme. It may
also supply render-only cell styles for standard data bars and color scales
when the canvas renderer omits them. These corrections never change the source
workbook or become exportable edits.
Selection, keyboard navigation, scrolling, sheet tabs, and copying remain
available. Mutation controls, resizing, paste, fill, and export of a modified
workbook are not exposed.

CSV and TSV remain on the lightweight grid path because they have no OOXML
visual model. Presentation preview remains a PDF conversion. LibreOffice stays
available for presentation conversion and visual-quality tooling, but XLSX
preview does not invoke it.

Workbook parsing and rendering run entirely inside the desktop renderer and its
worker. The preview does not upload workbook contents or fetch document assets
from a service. The WebAssembly binary is bundled with the application.

## Alternatives Considered

- **Convert each workbook to PDF.** This preserves a printable appearance, but
  a PDF page is not a workbook: cells, formulas, sheets, and range citations are
  no longer inspectable.
- **Export Calc HTML and add interaction around its tables.** This produces a
  strong static rendering, including rasterized charts, but it flattens the
  workbook into legacy HTML and leaves Tidebreak to reconstruct spreadsheet
  semantics and viewport behavior from presentation markup.
- **Continue the SheetJS-to-Univer reconstruction.** The existing path already
  supports useful selection and formulas, but information discarded during the
  reconstruction cannot be recovered by the renderer. Adding one parser per
  missing OOXML feature would make Tidebreak own an incomplete import stack.
- **Embed LibreOfficeKit.** The LibreOffice desktop SDK explicitly does not
  support LibreOfficeKit on macOS. Depending on exported private symbols would
  create an unsupported ABI and release boundary.
- **Use a hosted office suite.** This adds network, identity, document-upload,
  and service-availability dependencies to a local desktop preview.
- **Build the full OOXML engine in Tidebreak.** This provides maximum control,
  but duplicates a large parser, formula, chart, drawing, and rendering stack
  before the product needs editing.

## Consequences

The desktop gains a real workbook viewer rather than two partial representations
of the same file. The application bundle grows because it carries a WebAssembly
OOXML engine and chart renderer. Viewer upgrades become deliberate dependency
bumps and require workbook regression checks. Unsupported or malformed workbook
features must degrade inside the workbook surface or show a specific preview
error; they must not silently route the workbook back through PDF.

The viewer dependency is a product-critical rendering engine. Its license,
maintenance, bundled asset loading, worker behavior, and compatibility with the
desktop webview must be reviewed on every upgrade.

Revisit this decision if the engine stops being maintained, fails the golden
workbook suite on important Excel features, becomes incompatible with the
desktop runtime, or Tidebreak adds workbook editing whose persistence contract
the read-only integration cannot satisfy.

## Validation

- A workbook with multiple sheets, merged cells, formulas, frozen panes,
  authored dimensions, comments, and charts opens as one interactive workbook
  with no office-to-PDF request.
- A range citation activates the named or indexed sheet, selects the requested
  address, and scrolls it into view.
- The address/formula display follows selection while mutation gestures cannot
  change workbook contents.
- The golden dashboard workbook preserves its authored borders, merged KPI
  alignment, themed chart series, progress data bars, and risk color scale.
- The viewer works with its WebAssembly asset bundled in a production UI build,
  without a network request.
- CSV and presentation routing remain unchanged.
