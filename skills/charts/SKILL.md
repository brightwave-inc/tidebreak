---
name: charts
description: Render charts with matplotlib to PNG (or SVG for vector) in output/ — readable sizing, labels, and legends, inspected before delivery.
deps: { python: ["matplotlib==3.10.0"] }
---

# Charts

Produce chart deliverables with **matplotlib**. Follow every section below
before declaring a chart done.

## Installing the library

Install the pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user matplotlib==3.10.0
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest result you can produce without
the library (the summarized data as a table, or an SVG you write by hand
for a simple chart) — only with their knowledge, never as a silent
substitution. If a dependency cannot be installed at all, say so plainly
instead of quietly delivering a lesser format.

## Rendering

- Use the non-interactive backend — set `matplotlib.use("Agg")` before
  importing `pyplot` — and save with `fig.savefig(...)`; never call
  `plt.show()` in the sandbox.
- Render PNG by default. When the user asks for a vector or print-ready
  graphic, save SVG (and provide PNG alongside it unless they only want the
  vector).
- Size for reading, not for thumbnails: around `figsize=(10, 6)` at
  `dpi=150` for a standalone PNG, and `bbox_inches="tight"` so labels are
  not clipped.

## Readability

- Every chart gets a title, axis labels with units, and — whenever more
  than one series is plotted — a legend with meaningful names.
- Rotate or thin dense tick labels rather than letting them collide;
  format large numbers with thousands separators or scaled units.
- Prefer one clear message per chart over a crowded multi-axis figure;
  produce several charts when the data carries several stories.
- Use color plus a second cue (marker or line style) to distinguish series,
  and keep font sizes at 10pt equivalent or larger after export.

## Saving deliverables

Save finished charts in `output/` — files there are published to the user
as durable outputs. Writing the same filename again publishes a new version
of the same output, so keep the filename stable when revising and change it
only when the user asks for a distinct chart. When a chart accompanies a
document deliverable (a report embedding the figure), save both files in
`output/`: the document and the standalone chart image.

## Validation before declaring done

PNG and SVG land in `output/` where you can see them directly in the exec
result flow — inspect the rendered image before presenting it: no clipped
or colliding labels, a legend that matches the series, axes that start
where you intended, and text large enough to read at natural size. Fix the
plotting code rather than accepting a flawed render. Only declare the chart
done after it reads correctly.
