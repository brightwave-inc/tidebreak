---
name: word-documents
description: Create Word (DOCX) documents with python-docx — headings, styles, tables, page setup — validated by reopening before delivery.
deps: { python: ["python-docx==1.1.2"], host: ["libreoffice"] }
---

# Word documents

Produce DOCX deliverables with **python-docx**. Follow every section below
before declaring a document done.

## When DOCX is the right format

Reach for DOCX when the user will edit, print, or share the document in
Word — a report, letter, proposal, or anything with real formatting.
When the user just needs readable text they will consume in the chat or a
plain file, a markdown output serves them better than a DOCX: say so and
offer the simpler format instead of defaulting to Word.

## Installing the library

Install the pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user python-docx==1.1.2
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest format you can produce without
the library (markdown or HTML) — only with their knowledge, never as a
silent substitution. If a dependency cannot be installed at all, say so
plainly instead of quietly delivering a lesser format.

## Building documents from scratch

- Start from `docx.Document()` and compose with `add_heading(...)`,
  `add_paragraph(...)`, `add_table(...)`, and `add_picture(...)`.
- Use the built-in styles (`Heading 1`..`Heading 3`, `Title`, `List Bullet`,
  `List Number`, `Quote`) rather than hand-formatting runs; consistent styles
  are what make the document editable for the user afterwards.
- Reserve direct run formatting (`bold`, `italic`, `font.size`) for emphasis
  inside a paragraph, not for imitating headings.
- Tables: create with explicit dimensions, put header text in the first row,
  and apply a table style (for example `Light Grid Accent 1`) so borders
  render. Populate cells via `table.cell(row, col).text`.
- Page setup lives on `document.sections[0]`: page size, orientation
  (`WD_ORIENT.LANDSCAPE` for wide tables), and margins via `Inches(...)`.
- Add page breaks with `add_page_break()` between major parts rather than
  padding with empty paragraphs.

## Saving deliverables

Save the finished document in `output/` — files there are published to the
user as durable outputs. Writing the same filename again publishes a new
version of the same output, so keep the filename stable when revising and
change it only when the user asks for a distinct document.

## Validation before declaring done

1. Reopen the saved file with the library — `docx.Document("output/<file>.docx")`
   — and walk its paragraphs and tables to confirm the structure you meant
   to write is actually there (headings present, table dimensions right,
   no empty required sections). A file that fails to reopen is not a
   deliverable.
2. When LibreOffice is available in the sandbox, render pages for a visual
   check with the bundled helper:
   `python3 .openwave/exec-scripts/render_office.py output/<file>.docx`
   (images land in `preview/`; at most 3 are returned per exec call). If the
   helper reports LibreOffice is missing, rely on the reopen check and say
   the visual pass was not possible.

Only declare the document done after validation passes.
