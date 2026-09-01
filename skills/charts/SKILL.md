---
name: charts
description: Produce charts as interactive native figures (Plotly JSON) in output/, or as matplotlib PNG/SVG when an image file is needed — readable sizing, labels, and legends, validated before delivery.
deps: { python: ["matplotlib==3.11.1"] }
---

# Charts

Charts ship in one of two forms. Pick the form first, then follow that
section plus the shared rules at the end.

- **Native chart** — an interactive figure the app renders inline, with
  zoom, hover, and PNG export. Default for anything delivered on screen.
- **Raster chart** — a matplotlib PNG or SVG. Use it when the chart must be
  embedded in a generated document (DOCX, PDF, PPTX) or when the user asks
  for an image file.

When a chart is both shown to the user and embedded in a document, produce
both: the native figure for the conversation, the image for the document.

## Native charts

Write a file named `<name>.chart.json` in `output/`. It must contain a
single JSON object — a Plotly figure — with a `data` array of traces and an
optional `layout` object:

```json
{
  "data": [
    {
      "type": "bar",
      "name": "Revenue",
      "x": ["Q1", "Q2", "Q3", "Q4"],
      "y": [12400, 15100, 14800, 19300]
    }
  ],
  "layout": {
    "title": { "text": "Revenue by quarter (USD)" },
    "xaxis": { "title": { "text": "Quarter" } },
    "yaxis": { "title": { "text": "Revenue (USD)" } }
  }
}
```

No library is required. Author the JSON directly when you already know the
numbers — with `json.dump` from a small script, or by writing the file
outright. Reach for `plotly` only when the traces have to be computed from
data you are processing anyway; if you do, install it in its own `exec`
call the way matplotlib is installed below (`python3 -m pip install --user
plotly==6.9.0`) and serialize with `fig.to_json()`.

Use standard Plotly figure attributes — the whole vocabulary is available,
including `bar`, `scatter` (lines and points), `pie`, `heatmap`,
`histogram`, `box`, `candlestick`, `treemap`, `sunburst`, `waterfall`, and
`funnel`.

Rules for native figures:

- **Self-contained data only.** Inline every value in the trace. No
  external data URLs, no remote sources, no scripts — a figure that fetches
  something will not render.
- **Stay well under 512 KiB.** Aggregate, bucket, or downsample before
  writing rather than embedding a huge raw series; a chart nobody can read
  at full resolution does not need full resolution.
- **Title, axis labels with units, and a legend for multi-series figures
  are mandatory**, same as for raster charts.
- **Do not set explicit colors, fonts, or backgrounds** unless the user
  asks for specific ones. The app themes figures to match its light and
  dark palette, and hardcoded styling fights it.

## Raster charts (matplotlib)

### Installing the library

Install the pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user matplotlib==3.11.1
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest result you can produce without
the library (a native chart figure, the summarized data as a table, or an
SVG you write by hand for a simple chart) — only with their knowledge,
never as a silent substitution. If a dependency cannot be installed at all,
say so plainly instead of quietly delivering a lesser format.

### Rendering

- Use the non-interactive backend — set `matplotlib.use("Agg")` before
  importing `pyplot` — and save with `fig.savefig(...)`; never call
  `plt.show()` in the sandbox.
- Render PNG by default. When the user asks for a vector or print-ready
  graphic, save SVG (and provide PNG alongside it unless they only want the
  vector).
- Size for reading, not for thumbnails: around `figsize=(10, 6)` at
  `dpi=150` for a standalone PNG, and `bbox_inches="tight"` so labels are
  not clipped.

### Readability

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
`output/`: the document and the standalone chart.

## Validation before declaring done

**Native charts.** You cannot see the render, so validate the file
instead. Read it back and check that it parses as JSON, that it is a single
object, that `data` is a non-empty array whose entries are objects, and
that each trace carries a plausible `type` and the fields that type needs.
Re-read the numbers against your source too — a figure that renders
beautifully from the wrong values is still wrong. Fix the file rather than
shipping a figure you have not checked.

**Raster charts.** PNG and SVG land in `output/` where you can see them
directly in the exec result flow — inspect the rendered image before
presenting it: no clipped or colliding labels, a legend that matches the
series, axes that start where you intended, and text large enough to read
at natural size. Fix the plotting code rather than accepting a flawed
render.

Only declare the chart done after it checks out.
