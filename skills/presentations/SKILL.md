---
name: presentations
description: Build PowerPoint (PPTX) decks with python-pptx — layouts, placeholders, in-bounds geometry, overflow-free text — with visual QA before delivery.
deps: { python: ["python-pptx==1.0.2"], host: ["libreoffice"] }
---

# Presentations

Produce PPTX deliverables with **python-pptx**. Follow every section below
before declaring a deck done.

## Installing the library

Install the pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user python-pptx==1.0.2
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest format you can produce without
the library (a markdown outline of the deck) — only with their knowledge,
never as a silent substitution. If a dependency cannot be installed at all,
say so plainly instead of quietly delivering a lesser format.

## Structure and layouts

- Default structure: a title slide, then title-and-content slides — one idea
  per slide. Use `prs.slide_layouts[0]` for the title slide and
  `prs.slide_layouts[1]` (Title and Content) for body slides.
- Fill the layout's placeholders (`slide.shapes.title`,
  `slide.placeholders[1]`) instead of free-floating text boxes; placeholders
  carry the theme's fonts and positions for free.
- Add bullets through the content placeholder's `text_frame`: first bullet
  on `text_frame.paragraphs[0]`, subsequent ones via `add_paragraph()`,
  with `paragraph.level` for sub-bullets.

## Geometry and overflow

- The default slide is 13.33 x 7.5 inches (16:9). Any shape you position
  manually must satisfy `left + width <= slide width` and
  `top + height <= slide height` — compute positions with `Inches(...)`
  rather than guessing EMUs, and never let images or boxes hang off the
  slide edge.
- Avoid text overflow by writing less text, not by shrinking fonts:
  at most about 6 bullets per slide and about 12 words per bullet. When
  content does not fit, split the slide. Keep body text at 18pt or larger;
  a slide that needs 12pt text is a document, not a slide.

## Consistency

- Keep one font family and a small, consistent color palette across the
  deck; set them through the layout placeholders and reuse the same
  `RGBColor` constants everywhere rather than styling each slide ad hoc.
- Images: preserve aspect ratio by setting only width or only height, and
  keep them inside the slide bounds.

## Saving deliverables

Save the finished deck in `output/` — files there are published to the user
as durable outputs. Writing the same filename again publishes a new version
of the same output, so keep the filename stable when revising and change it
only when the user asks for a distinct deck.

## Validation before declaring done

1. Reopen the saved file — `pptx.Presentation("output/<file>.pptx")` — and
   confirm the slide count and that each slide's title and body text are
   what you intended. A file that fails to reopen is not a deliverable.
2. When LibreOffice is available in the sandbox, render slides for a visual
   check with the bundled helper:
   `python3 .openwave/exec-scripts/render_office.py output/<file>.pptx`
   (images land in `preview/`; at most 3 are returned per exec call).
   Inspect for clipped text, overlapping shapes, and anything crossing the
   slide edge, and fix the generator rather than accepting a flawed slide.
   If the helper reports LibreOffice is missing, rely on the reopen check
   and say the visual pass was not possible.

Only declare the deck done after validation passes.
