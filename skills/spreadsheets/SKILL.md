---
name: spreadsheets
description: Build Excel (XLSX) workbooks with openpyxl and edit existing ones through headless LibreOffice Calc — live formulas, number formats, layout — recalculated and re-inspected before delivery.
deps: { python: ["openpyxl==3.1.5"], host: ["libreoffice"] }
---

# Spreadsheets

Produce XLSX deliverables with **openpyxl**, and edit workbooks that already
exist with **LibreOffice Calc**. Follow every section below before declaring
a workbook done.

## Never round-trip an existing workbook with openpyxl

**openpyxl writes new workbooks. It never touches a file that already
exists** — not to change one number, not for a "quick value edit".

`load_workbook(path)` followed by `.save(path)` does not edit the file; it
rebuilds it from the parts openpyxl models, and everything else is gone. In
practice that means silently losing:

- **formula results** the engine computed — openpyxl drops the cached values,
  so every formula reads as empty until someone opens the file in Excel
- named ranges, pivot tables and their caches
- charts, images, and shapes
- conditional formatting and data validation
- macros, custom styles, and sheet protection

None of this raises an error. The file saves, looks plausible in an
inspection script, and hands the user a gutted copy of their own workbook.

Read-only inspection is fine and expected: `load_workbook(path,
read_only=True)` or `data_only=True`, with no `.save()` anywhere near it. The
bundled `analyze_xlsx.py` does exactly this.

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

## Creating a new workbook

A workbook built from scratch is openpyxl's job, with the formula and layout
rules below. Nothing in this section changes because of the edit path.

## Editing a workbook that already exists

Use the bundled Calc driver, which opens the file in a real spreadsheet
engine and saves it back through that engine's own filter — the edit lands
in place and everything the file already carried survives:

```
python3 .openwave/exec-scripts/calc_uno.py inspect input/model.xlsx
python3 .openwave/exec-scripts/calc_uno.py get-cell input/model.xlsx Summary B7
python3 .openwave/exec-scripts/calc_uno.py set-cell input/model.xlsx Summary B7 '=SUM(B2:B6)'
python3 .openwave/exec-scripts/calc_uno.py list-named-ranges input/model.xlsx
python3 .openwave/exec-scripts/calc_uno.py set-named-range input/model.xlsx Revenue '$Data.$B$2:$B$13'
```

`set-cell` treats a leading `=` as a formula and anything else as a number
when it parses as one, text otherwise. Each write recalculates and saves, so
run one `set-cell` per change rather than batching edits you cannot inspect.
Run it with `python3`: the UNO bridge binds to the system interpreter.

This needs the LibreOffice that ships in the sandbox document image. Outside
that sandbox the script exits saying so — **that is the end of the edit
path, not a cue to fall back to openpyxl**. Tell the user the workbook
cannot be edited in this environment and what they would get instead (for
example a new workbook built from scratch, which is not the same file).

## Formulas: the hard rule

**Write live formulas, never precomputed values, in any cell that represents
a calculation.** A total cell gets `=SUM(B2:B13)`, not the number you
computed in Python. A spreadsheet with baked-in values silently goes stale
the moment the user edits an input, which defeats the reason they asked for
a spreadsheet.

openpyxl writes formulas but **does not calculate them** — the file carries
the formula text and Excel computes it on open. Two consequences:

- You cannot read a formula's result back by reopening the file with
  openpyxl — there is no cached value there to read. Compute the expected
  value in Python for your own checking, or let the recalculation step below
  evaluate the workbook properly.
- Any calculated value the user must see in the conversation should also be
  stated in chat, since they may not open the workbook immediately.

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

Two passes, in this order, after every create or edit.

**1. Recalculate.** Formulas you wrote are unproven until an engine evaluates
them:

```
python3 .openwave/exec-scripts/xlsx_recalc.py output/<file>.xlsx
```

It opens the workbook in Calc, forces a full recalculation, saves the
computed results back, then reports per sheet any cells holding `#REF!`,
`#DIV/0!`, `#VALUE!`, `#NAME?`, `#N/A`, `#NUM!` or `#NULL!`, plus numbers
that were written as text and so drop out of the sums above them. Finding
problems is a report, not a failure — fix the offending cells and rerun
until the report is clean. Like the editor, this needs the sandbox
LibreOffice; when it is unavailable, say the formulas could not be verified
rather than declaring them correct.

**2. Inspect visually.** Re-read the saved file with the bundled analyzer:

```
python3 .openwave/exec-scripts/analyze_xlsx.py output/<file>.xlsx
```

It prints each sheet's dimensions and sample rows and writes sheet
thumbnails into `preview/` (at most 3 images are returned per exec call).
Check that every expected sheet exists, headers and sample rows look right,
and the thumbnails show a readable layout. Only declare the workbook done
after both passes are clean.
