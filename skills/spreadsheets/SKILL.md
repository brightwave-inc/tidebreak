---
name: spreadsheets
description: Build Excel (XLSX) workbooks with openpyxl — live formulas, number formats, layout — re-inspected with the bundled analyzer before delivery.
deps: { python: ["openpyxl==3.1.5"] }
---

# Spreadsheets

Produce XLSX deliverables with **openpyxl**. Follow every section below
before declaring a workbook done.

## Installing the library

Install the pinned dependency with its own `exec` call — commands have a
bounded wall clock, and one `pip` invocation per package stays inside it:

```
python3 -m pip install --user openpyxl==3.1.5
```

Installs work only when this chat's network policy allows package managers,
and they persist for the rest of the conversation. If an install is refused
by policy, do not retry: tell the user to enable the package-manager network
policy for this chat, and offer the closest format you can produce without
the library (CSV) — only with their knowledge, never as a silent
substitution. If a dependency cannot be installed at all, say so plainly
instead of quietly delivering a lesser format.

## Formulas: the hard rule

**Write live formulas, never precomputed values, in any cell that represents
a calculation.** A total cell gets `=SUM(B2:B13)`, not the number you
computed in Python. A spreadsheet with baked-in values silently goes stale
the moment the user edits an input, which defeats the reason they asked for
a spreadsheet.

openpyxl writes formulas but **does not calculate them** — the file carries
the formula text and Excel computes it on open. Two consequences:

- You cannot read a formula's result back in a later exec; do not try to
  "verify" totals by reopening the file. Compute the expected value in
  Python for your own checking if needed.
- Any calculated value the user must see in the conversation should also be
  stated in chat, since you cannot read it from the file and they may not
  open the workbook immediately.

## Layout conventions

- Give columns real number formats: currency `#,##0.00`, percentages
  `0.0%`, dates `yyyy-mm-dd` — via `cell.number_format`. Never leave money
  as bare floats.
- Size columns to their content with `worksheet.column_dimensions["A"].width`;
  unreadably narrow columns are a defect.
- Freeze the header row (`worksheet.freeze_panes = "A2"`) on any sheet the
  user will scroll.
- Bold the header row and keep one table per sheet; split unrelated data
  into multiple named sheets (`workbook.create_sheet("Assumptions")`)
  instead of stacking tables.

## Saving deliverables

Save the finished workbook in `output/` — files there are published to the
user as durable outputs. Writing the same filename again publishes a new
version of the same output, so keep the filename stable when revising and
change it only when the user asks for a distinct workbook. Outputs above
the 16 MiB ceiling are refused: keep workbooks lean (no embedded raw data
dumps that a summary sheet serves better), and if the data genuinely cannot
fit, tell the user and deliver a trimmed workbook plus the raw data as CSV.

## Validation before declaring done

Re-inspect the saved file with the bundled analyzer:

```
python3 .openwave/exec-scripts/analyze_xlsx.py output/<file>.xlsx
```

It prints each sheet's dimensions and sample rows and writes sheet
thumbnails into `preview/` (at most 3 images are returned per exec call).
Check that every expected sheet exists, headers and sample rows look right,
and the thumbnails show a readable layout. Only declare the workbook done
after this inspection passes.
