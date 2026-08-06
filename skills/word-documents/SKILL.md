---
name: word-documents
description: Produce Word (DOCX) documents — new documents with python-docx, existing documents and templates edited in place through their OOXML — validated before delivery.
deps: { python: ["python-docx==1.1.2"], host: ["libreoffice"] }
---

# Word documents

Two paths produce a DOCX, and picking the wrong one destroys work. Choose
first, then follow that path's section, and finish with the validation
section — it applies to both.

## Which path

- **An existing `.docx` is in play** — a template, a document the user
  uploaded, a prior version of this deliverable, a downloaded starting
  point — **edit it in place** with the XML pipeline below. Always.
- **Nothing exists yet and there is no template** — build the document from
  scratch with **python-docx**.

**Never rebuild an existing document with python-docx to apply a change.**
`Document()` starts from the library's default template, and it cannot
reproduce a firm's: the style definitions, numbering, headers and footers,
theme fonts, and page setup all come back as generic defaults, so
regenerating silently strips the design the user is attached to. A one-line
text fix is a one-line XML edit, not a rewrite. Equally, never recreate from
scratch what a template already provides — if the user handed you a template,
its design *is* the requirement.

## When DOCX is the right format

Reach for DOCX when the user will edit, print, or share the document in
Word — a report, letter, proposal, or anything with real formatting.
When the user just needs readable text they will consume in the chat or a
plain file, a markdown output serves them better than a DOCX: say so and
offer the simpler format instead of defaulting to Word.

## New documents: python-docx

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

## Existing documents and templates: edit the XML in place

A DOCX is a zip of XML parts. Editing those parts directly is the only way to
change a document without re-authoring it. The workflow:

1. **Unpack**
   `python3 .openwave/exec-scripts/office_unpack.py doc.docx build/doc`
   writes the package out as a directory tree, byte for byte.
2. **Survey before editing.** Read `build/doc/word/document.xml` and find the
   passages you need to change. Decide what you are editing and what you are
   leaving **untouched**. Do not open a part you have no edit to make in.
3. **Edit the targets.** Body text lives in `word/document.xml`, inside
   `<w:t>` runs — a single sentence is often split across several runs, so
   search for a distinctive fragment rather than the whole phrase. Style
   definitions live in `word/styles.xml`, headers and footers in
   `word/headerN.xml` and `word/footerN.xml`, and every part's images,
   hyperlinks, and part links in the matching `.rels` file under
   `word/_rels/`. Make targeted, minimal edits — replace the text of a run,
   change one attribute — and leave the surrounding markup exactly as it was.
   Never load the tree into a generic XML library and re-serialize the whole
   file: a round trip through a generic writer reorders namespaces and drops
   content Word needs.
4. **Check**
   `python3 .openwave/exec-scripts/docx_clean.py build/doc`
   parses every XML part and confirms every relationship target exists. Fix
   what it names before going on; a malformed part is the "Word found
   unreadable content" dialog.
5. **Pack**
   `python3 .openwave/exec-scripts/office_pack.py build/doc output/<name>.docx`
   rezips the tree into a valid package.

Unpack once and keep the tree for the rest of the conversation — later
revisions are more edits to the same tree, not another round trip.

## Saving deliverables

Save the finished document in `output/` — files there are published to the
user as durable outputs. Writing the same filename again publishes a new
version of the same output, so keep the filename stable when revising and
change it only when the user asks for a distinct document.

## Validation before declaring done

1. Confirm the document opens. For one you **generated** with python-docx,
   reopen it with the library — `docx.Document("output/<file>.docx")` — and
   walk its paragraphs and tables to confirm the structure you meant to write
   is actually there (headings present, table dimensions right, no empty
   required sections). For one you **edited in place**, do not reopen it with
   python-docx: rendering it (step 2) is the check, because LibreOffice
   converts only a package it can parse. A file that fails to open either way
   is not a deliverable.
2. Render pages for a visual check with the bundled helper:
   `python3 .openwave/exec-scripts/render_office.py output/<file>.docx`
   (images land in `preview/`; at most 3 are returned per exec call; the
   renderer needs `pypdfium2` and `pillow`, installable with pip). The
   helper converts through a sandbox LibreOffice when one exists; otherwise
   it renders the PDF the host converts after every successful command that
   saved the document — the workspace sync notes name it (under
   `.openwave/render/`), and on a managed sandbox you must list that PDF in
   the helper call's `files` to stage it in. Inspect for clipped text, broken
   tables, and lost formatting. When you edited an existing document, check
   the pages you meant to preserve too — they should look exactly as they did
   before. Only when the sync notes say office rendering is unavailable on
   the host, say plainly what could not be checked: for a generated document
   the reopen check still stands and the visual pass does not; for an edited
   one neither the visual pass nor the open check was possible.

Only declare the document done after validation passes.
