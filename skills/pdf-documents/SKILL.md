---
name: pdf-documents
description: Generate PDFs with fpdf2 and merge, split, or fill existing PDFs with pypdf, with visual QA before delivery.
deps: { python: ["fpdf2==2.8.3", "pypdf==6.16.1"] }
---

# PDF documents

Produce PDF deliverables with two libraries: **fpdf2** to generate new
documents and **pypdf** to manipulate existing ones (merge, split, rotate,
fill forms). Follow every section below before declaring a PDF done.

## Installing the libraries

Install each pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user fpdf2==2.8.3
python3 -m pip install --user pypdf==6.16.1
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest format you can produce without
the library (for example markdown or HTML) — only with their knowledge,
never as a silent substitution. If a dependency cannot be installed at all,
say so plainly instead of quietly delivering a lesser format.

## Generating PDFs with fpdf2

- Build documents from `fpdf.FPDF`: `add_page()`, `set_font(...)`, then
  `cell(...)` / `multi_cell(...)` for text and `image(...)` for figures.
- The core fonts (Helvetica, Times, Courier) cover ASCII reliably. For text
  beyond Latin-1, register a Unicode TrueType font with `add_font(...)`
  before use; never let glyphs silently render as placeholders.
- Set metadata that helps the user: `set_title(...)`, `set_author(...)` when
  the document has a named author.
- Prefer `multi_cell` for paragraphs so long lines wrap instead of
  overflowing the page; leave margins at their defaults unless the layout
  needs otherwise.

## Manipulating PDFs with pypdf

- Merge: append pages from several `PdfReader` sources into one `PdfWriter`.
- Split: copy the selected page ranges into separate writers.
- Forms: read fields with `PdfReader.get_fields()` and fill them with
  `PdfWriter.update_page_form_field_values(...)`; call
  `writer.set_need_appearances_writer(True)` so viewers render the values.
- Always open source PDFs from the workspace (attachments arrive under
  `documents/`), and never assume a page count — read it.

## Working from an existing PDF as a template

A PDF the user supplies as a template is a read-only reference: its layout
cannot be edited in place. There are two ways forward, and the first is much
better whenever it applies.

1. **Check for fillable form fields first.** Read the template with
   `PdfReader.get_fields()`. If it returns usable fields, fill them as above
   — that keeps the original design intact and is far less work than
   rebuilding the document.
2. **Only when there are no usable fields, reproduce the layout.** Render the
   template to images with the helper below and study it visually — fonts,
   sizes, margins, column positions, rules, headers and footers — alongside
   its structure, then rebuild it as a new document with fpdf2, matching that
   layout as closely as fpdf2 allows. Approximations are expected; say which
   ones you made.

A reproduction is a new document that resembles the template, not the
template itself. Never present it as the original.

## Saving deliverables

Save the finished PDF in `output/` — files there are published to the user
as durable outputs. Writing the same filename again publishes a new version
of the same output, so keep the filename stable when revising and change it
only when the user asks for a distinct document.

## Visual QA before declaring done

Rendering catches what code cannot: clipped text, overlapping elements,
missing glyphs, blank pages. Before presenting the PDF:

1. Render pages to images in `preview/` with the bundled helper:
   `python3 .tidebreak/exec-scripts/render_pdf.py output/<file>.pdf`
   (at most 3 preview images are returned per exec call; render further
   pages in another call when the document is long).
2. Inspect the returned images for layout defects and fix the generator
   code rather than accepting a flawed page.

Only declare the document done after the rendered pages look right.
