#!/usr/bin/env python3
"""Inspect and edit an existing spreadsheet through headless LibreOffice Calc.

This is the edit path for workbooks that already exist. A real spreadsheet
engine opens the file and saves it back through its own filter, so formulas
the engine computes, named ranges, pivot tables, charts, conditional
formatting and data validation all survive — none of which is true of a
library that rebuilds the file from the parts it happens to model.

`import uno` binds to the system python3, so run this script with `python3`.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from _tidebreak_calc import (
    DEFAULT_TIMEOUT,
    apply_cell_input,
    calc_document,
    cell_by_reference,
    cell_reference,
    content_type,
    sheet_by_name,
    used_area,
)
from _tidebreak_preview import HelperError, existing_file, run_cli

SPREADSHEETS = [".xlsx", ".xlsm", ".ods", ".csv"]


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description=(
            "Inspect or edit an existing spreadsheet in place with headless "
            "LibreOffice Calc."
        ),
    )
    cli.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT,
        help=f"seconds to wait for LibreOffice, from 10 through 600 (default: {DEFAULT_TIMEOUT})",
    )
    commands = cli.add_subparsers(dest="command", required=True)

    inspect = commands.add_parser(
        "inspect", help="list sheets, used ranges, and named ranges"
    )
    inspect.add_argument("workbook", help="spreadsheet inside the exec workspace")

    get_cell = commands.add_parser("get-cell", help="print one cell's formula and value")
    get_cell.add_argument("workbook", help="spreadsheet inside the exec workspace")
    get_cell.add_argument("sheet", help="sheet name")
    get_cell.add_argument("ref", help="cell reference such as B7")

    set_cell = commands.add_parser(
        "set-cell", help="write one cell and save the workbook in place"
    )
    set_cell.add_argument("workbook", help="spreadsheet inside the exec workspace")
    set_cell.add_argument("sheet", help="sheet name")
    set_cell.add_argument("ref", help="cell reference such as B7")
    set_cell.add_argument(
        "value",
        help="cell contents; a leading '=' writes a formula, otherwise a number or text",
    )

    list_named = commands.add_parser(
        "list-named-ranges", help="print every named range and what it refers to"
    )
    list_named.add_argument("workbook", help="spreadsheet inside the exec workspace")

    set_named = commands.add_parser(
        "set-named-range",
        help="point a named range at a new reference or expression, creating it if needed",
    )
    set_named.add_argument("workbook", help="spreadsheet inside the exec workspace")
    set_named.add_argument("name", help="named range")
    set_named.add_argument("value", help="reference or expression, such as $Data.$A$1:$C$20")
    return cli


def _inspect(document) -> None:
    sheets = document.Sheets
    names = list(sheets.getElementNames())
    print(f"Sheets: {len(names)}")
    for name in names:
        sheet = sheets.getByName(name)
        end_column, end_row = used_area(sheet)
        extent = f"A1:{cell_reference(end_column, end_row)}"
        print(f"- {name}: used={extent}, rows={end_row + 1}, columns={end_column + 1}")
    _print_named_ranges(document)


def _print_named_ranges(document) -> None:
    ranges = document.NamedRanges
    names = list(ranges.getElementNames())
    if not names:
        print("Named ranges: none")
        return
    print(f"Named ranges: {len(names)}")
    for name in names:
        print(f"- {name}: {ranges.getByName(name).Content}")


def _get_cell(document, sheet_name: str, reference: str) -> None:
    cell = cell_by_reference(sheet_by_name(document, sheet_name), reference)
    kind = content_type(cell)
    print(f"{sheet_name}!{reference}: type={kind}")
    formula = cell.getFormula()
    if kind == "formula":
        print(f"  formula: {formula}")
    print(f"  displayed: {cell.getString()}")
    if kind in {"number", "formula"}:
        print(f"  numeric: {cell.getValue()}")


def _set_cell(document, sheet_name: str, reference: str, value: str, path: Path) -> None:
    cell = cell_by_reference(sheet_by_name(document, sheet_name), reference)
    kind = apply_cell_input(cell, value)
    document.calculateAll()
    document.store()
    print(f"Set {sheet_name}!{reference} as a {kind} in {path.name}.")
    print(f"  displayed: {cell.getString()}")


def _set_named_range(document, name: str, value: str, path: Path) -> None:
    from com.sun.star.table import CellAddress

    ranges = document.NamedRanges
    if ranges.hasByName(name):
        ranges.getByName(name).setContent(value)
        action = "Updated"
    else:
        anchor = CellAddress()
        anchor.Sheet = 0
        anchor.Column = 0
        anchor.Row = 0
        ranges.addNewByName(name, value, anchor, 0)
        action = "Created"
    document.calculateAll()
    document.store()
    print(f"{action} named range {name} = {ranges.getByName(name).Content} in {path.name}.")


def main() -> int:
    args = parser().parse_args()
    if not 10 <= args.timeout <= 600:
        raise HelperError("timeout must be between 10 and 600 seconds")
    source = existing_file(args.workbook, SPREADSHEETS)
    read_only = args.command in {"inspect", "get-cell", "list-named-ranges"}
    with calc_document(source, timeout=args.timeout, read_only=read_only) as document:
        if args.command == "inspect":
            _inspect(document)
        elif args.command == "get-cell":
            _get_cell(document, args.sheet, args.ref)
        elif args.command == "list-named-ranges":
            _print_named_ranges(document)
        elif args.command == "set-cell":
            _set_cell(document, args.sheet, args.ref, args.value, source)
        elif args.command == "set-named-range":
            _set_named_range(document, args.name, args.value, source)
        else:  # argparse rejects anything else first.
            raise HelperError(f"unknown command: {args.command}")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
