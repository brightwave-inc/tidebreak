---
name: presentations
description: Build PowerPoint (PPTX) decks — new decks with pptxgenjs, existing decks and templates edited in place through their OOXML — with visual QA before delivery.
deps: { npm: ["pptxgenjs@4.0.1"], host: ["libreoffice"] }
---

# Presentations

Two paths produce a PPTX, and picking the wrong one destroys work. Choose
first, then follow that path's section, and finish with the validation
section — it applies to both.

## Which path

- **An existing `.pptx` is in play** — a template, a deck the user uploaded,
  a prior version of this deliverable, a downloaded starting point — **edit
  it in place** with the XML pipeline below. Always.
- **Nothing exists yet and there is no template** — write a Node.js script
  with **pptxgenjs** and generate the deck.

**Never rebuild an existing deck to apply a change.** Regenerating a deck
re-authors every slide: the master, theme, fonts, colors, logos, footers, and
every slide you did not think to re-emit are gone, silently, and the user sees
a deck that no longer looks like theirs. A one-line text fix is a one-line XML
edit, not a rewrite. Equally, never recreate from scratch what a template
already provides — if the user handed you a template, its design *is* the
requirement.

## New decks: pptxgenjs

`require("pptxgenjs")` resolves in the sandbox with no install — the pinned
version is baked into the image and `NODE_PATH` points at it. On a local exec
workspace, install it once with its own `exec` call:

```
npm install --ignore-scripts pptxgenjs@4.0.1
```

Keep `--ignore-scripts`: a local workspace runs on the user's own machine, and
a package's install hooks are arbitrary code nobody asked to run. The Node
runtime itself is host-managed on that backend — the turn's operating notes say
whether it is available, being installed, or absent, so read them before
choosing this path.

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused by
policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest format you can produce without the
library (a markdown outline of the deck) — only with their knowledge, never as
a silent substitution. If a dependency cannot be installed at all, say so
plainly instead of quietly delivering a lesser format.

Write the deck as a script and run it:

```js
const PptxGenJS = require("pptxgenjs");

const pres = new PptxGenJS();
pres.defineLayout({ name: "W16x9", width: 10, height: 5.625 });
pres.layout = "W16x9";

const slide = pres.addSlide();
slide.addText("Quarterly review", {
  x: 0.6,
  y: 0.5,
  w: 8.8,
  h: 1,
  fontSize: 36,
  color: "1F2933",
});

pres.writeFile({ fileName: "output/quarterly-review.pptx" });
```

Style rules:

- Define a 16:9 layout explicitly — `defineLayout` with 10 x 5.625 inches —
  and lay every slide out against those bounds. Keep shapes inside them:
  `x + w <= 10` and `y + h <= 5.625`.
- One idea per slide. At most about 6 bullets, about 12 words each. When
  content does not fit, split the slide rather than shrinking type.
- Body text at 18pt or larger. A slide that needs 12pt text is a document.
- Colors are hex **without** the `#`: `color: "1F2933"`, not `"#1F2933"`.
- Images: set only `w` or only `h` so the aspect ratio is preserved, and keep
  them inside the slide bounds.
- Use the native `addTable` and `addChart` APIs — a real table and a real
  chart, never a picture of one. Charts stay editable and tables stay
  selectable text.
- Avoid merged table cells (`colspan`/`rowspan`). The LibreOffice conversion
  behind the visual check does not render them reliably, so a merged layout
  cannot be verified before delivery.
- Keep one font family and a small, consistent palette across the deck; define
  the colors once as constants and reuse them.
- Save with `pres.writeFile({ fileName: "output/<name>.pptx" })`.

## Existing decks and templates: edit the XML in place

A PPTX is a zip of XML parts. Editing those parts directly is the only way to
change a deck without re-authoring it. The workflow:

1. **Unpack**
   `python3 .openwave/exec-scripts/office_unpack.py deck.pptx build/deck`
   writes the package out as a directory tree, byte for byte.
2. **Survey before editing.** List `build/deck/ppt/slides/` and read the
   slides. Classify each one: a *target* you will edit, or a slide to
   **preserve untouched**. Do not open a part you have no edit to make in.
3. **Edit the targets.** Slide text lives in `ppt/slides/slideN.xml`, inside
   `<a:t>` runs; images, hyperlinks, and layout links live in the matching
   `ppt/slides/_rels/slideN.xml.rels`. Make targeted, minimal edits — replace
   the text of a run, change one attribute — and leave the surrounding markup
   exactly as it was. Never load the tree into a library and re-serialize the
   whole file: a round trip through a generic XML writer reorders namespaces
   and drops content PowerPoint needs.
4. **To add a slide, duplicate one that already exists.** Copy
   `ppt/slides/slide3.xml` to `slide9.xml` and its rels file alongside it,
   then register the new part in three places: a `<p:sldId>` entry in
   `ppt/presentation.xml`, the matching relationship in
   `ppt/_rels/presentation.xml.rels` (with an `Id` no other relationship
   uses), and an `<Override>` for `/ppt/slides/slide9.xml` in
   `[Content_Types].xml`. Then edit the copy's text — a duplicated slide
   inherits the deck's design for free. Deleting a slide is the same three
   registrations in reverse.
5. **Check**
   `python3 .openwave/exec-scripts/pptx_clean.py build/deck`
   parses every XML part and confirms every relationship target exists. Fix
   what it names before going on; a malformed part is the "PowerPoint found a
   problem with this content" dialog.
6. **Pack**
   `python3 .openwave/exec-scripts/office_pack.py build/deck output/<name>.pptx`
   rezips the tree into a valid package.

Unpack once and keep the tree for the rest of the conversation — later
revisions are more edits to the same tree, not another round trip.

## Saving deliverables

Save the finished deck in `output/` — files there are published to the user
as durable outputs. Writing the same filename again publishes a new version
of the same output, so keep the filename stable when revising and change it
only when the user asks for a distinct deck.

## Validation before declaring done

1. Confirm the deck opens. Rendering it (step 2) is that check: LibreOffice
   converts only a package it can parse, so a file the renderer cannot convert
   is not a deliverable — go back and fix the generator or the XML edit rather
   than shipping it.
2. Render slides for a visual check with the bundled helper:
   `python3 .openwave/exec-scripts/render_office.py output/<file>.pptx`
   (images land in `preview/`; at most 3 are returned per exec call; the
   renderer needs `pypdfium2` and `pillow`, installable with pip). The
   helper converts through a sandbox LibreOffice when one exists; otherwise
   it renders the PDF the host converts after every successful command that
   saved the deck — the workspace sync notes name it (under
   `.openwave/render/`), and on a managed sandbox you must list that PDF in
   the helper call's `files` to stage it in. Inspect for clipped text,
   overlapping shapes, and anything crossing the slide edge, and fix the
   generator or the edited XML rather than accepting a flawed slide. When you
   edited an existing deck, check the slides you meant to preserve too — they
   should look exactly as they did before. Only when the sync notes say office
   rendering is unavailable on the host, say plainly that neither the visual
   pass nor the open check was possible.

Only declare the deck done after validation passes.
