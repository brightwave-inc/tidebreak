#!/usr/bin/env python3
"""Recalculate a workbook with LibreOffice Calc and report broken cells.

openpyxl writes formula text but never computes it, so a freshly generated
workbook's formulas are unproven until an engine evaluates them. This script
is that engine: it opens the file, forces a full recalculation, saves the
computed results back, and then reads every used cell looking for the two
defects a generated workbook actually ships with — formulas that evaluate to
an error, and numbers that were written as text and so drop out of every
`SUM` above them.

Finding problems is not a tool failure: the report is the deliverable, and
the exit status stays zero unless the recalculation itself could not run.

`import uno` binds to the system python3, so run this script with `python3`.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from _tidebreak_calc import (
    DEFAULT_TIMEOUT,
    ERROR_VALUES,
    calc_document,
    cell_reference,
    content_type,
    looks_like_number,
    used_area,
)
from _tidebreak_preview import HelperError, existing_file, run_cli

# A used area can be enormous once a stray cell sits far from the data; stop
# well before the scan becomes the slowest part of a turn and say so.
CELL_SCAN_LIMIT = 200_000
REPORTED_REFS = 5


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description=(
            "Recalculate a workbook with LibreOffice Calc, save it, and report "
            "error cells and numbers stored as text."
        ),
    )
    cli.add_argument("workbook", help="XLSX/XLSM/ODS file inside the exec workspace")
    cli.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT,
        help=f"seconds to wait for LibreOffice, from 10 through 600 (default: {DEFAULT_TIMEOUT})",
    )
    return cli


def _scan_sheet(sheet, budget: int) -> tuple[dict[str, list[str]], list[str], int]:
    """Return per-error cell refs, text-number refs, and cells examined."""

    errors: dict[str, list[str]] = {}
    text_numbers: list[str] = []
    end_column, end_row = used_area(sheet)
    examined = 0
    for row in range(end_row + 1):
        for column in range(end_column + 1):
            if examined >= budget:
                return errors, text_numbers, examined
            cell = sheet.getCellByPosition(column, row)
            examined += 1
            kind = content_type(cell)
            if kind == "empty":
                continue
            text = cell.getString().strip()
            if kind == "formula" and text in ERROR_VALUES:
                errors.setdefault(text, []).append(cell_reference(column, row))
            elif kind == "text" and looks_like_number(text):
                text_numbers.append(cell_reference(column, row))
    return errors, text_numbers, examined


def _format_refs(refs: list[str]) -> str:
    shown = ", ".join(refs[:REPORTED_REFS])
    if len(refs) > REPORTED_REFS:
        shown += f", … (+{len(refs) - REPORTED_REFS} more)"
    return shown


def main() -> int:
    args = parser().parse_args()
    if not 10 <= args.timeout <= 600:
        raise HelperError("timeout must be between 10 and 600 seconds")
    source = existing_file(args.workbook, [".xlsx", ".xlsm", ".ods"])

    total_errors = 0
    total_text_numbers = 0
    truncated = False
    with calc_document(source, timeout=args.timeout) as document:
        document.calculateAll()
        document.store()
        print(f"Recalculated and saved {source.name}.")
        budget = CELL_SCAN_LIMIT
        for name in document.Sheets.getElementNames():
            errors, text_numbers, examined = _scan_sheet(
                document.Sheets.getByName(name), budget
            )
            budget -= examined
            if budget <= 0:
                truncated = True
            error_count = sum(len(refs) for refs in errors.values())
            total_errors += error_count
            total_text_numbers += len(text_numbers)
            if not error_count and not text_numbers:
                print(f"- {name}: clean ({examined} cells checked)")
                continue
            print(
                f"- {name}: {error_count} error cell(s), "
                f"{len(text_numbers)} number(s) stored as text"
            )
            for value, refs in sorted(errors.items()):
                print(f"    {value}: {_format_refs(refs)}")
            if text_numbers:
                print(f"    text-numbers: {_format_refs(text_numbers)}")
            if budget <= 0:
                break

    if truncated:
        print(
            f"Scan stopped at the {CELL_SCAN_LIMIT}-cell limit; later sheets were not "
            "checked."
        )
    if total_errors or total_text_numbers:
        print(
            f"Found {total_errors} error cell(s) and {total_text_numbers} number(s) "
            "stored as text — fix the source formulas or cell types and rerun."
        )
    else:
        print("No error cells or text-stored numbers found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
