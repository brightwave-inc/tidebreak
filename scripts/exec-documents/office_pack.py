#!/usr/bin/env python3
"""Pack a directory unpacked by office_unpack.py back into an OOXML file.

Every part is written back — nothing is added, nothing is dropped except
editor droppings such as .DS_Store — with [Content_Types].xml first, which is
what makes the result openable by PowerPoint, Word, and LibreOffice.
"""

from __future__ import annotations

import argparse
import zipfile
from pathlib import Path

from _tidebreak_preview import HelperError, run_cli

CONTENT_TYPES = "[Content_Types].xml"
DROPPINGS = {".DS_Store", "Thumbs.db", "desktop.ini"}
DROPPING_DIRS = {"__MACOSX"}
OOXML_SUFFIXES = [".pptx", ".docx", ".xlsx", ".xlsm"]


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description="Zip an unpacked OOXML tree back into a valid PPTX/DOCX/XLSX file.",
    )
    cli.add_argument("directory", help="directory produced by office_unpack.py")
    cli.add_argument("output", help="destination PPTX, DOCX, or XLSX path")
    return cli


def package_parts(root: Path) -> list[Path]:
    parts = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if path.name in DROPPINGS or path.name.endswith("~"):
            continue
        if DROPPING_DIRS.intersection(path.relative_to(root).parts):
            continue
        parts.append(path)
    return parts


def main() -> int:
    args = parser().parse_args()
    root = Path(args.directory)
    if not root.is_dir():
        raise HelperError(f"unpacked directory does not exist: {root}")
    output = Path(args.output)
    if output.suffix.lower() not in OOXML_SUFFIXES:
        expected = ", ".join(OOXML_SUFFIXES)
        raise HelperError(f"expected an output named with one of {expected}, got: {output.name}")
    content_types = root / CONTENT_TYPES
    if not content_types.is_file():
        raise HelperError(f"{root}/{CONTENT_TYPES} is missing; this is not an OOXML tree")

    parts = [content_types] + [
        path for path in package_parts(root) if path != content_types
    ]
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in parts:
            archive.write(path, path.relative_to(root).as_posix())

    broken = zipfile.ZipFile(output).testzip()
    if broken is not None:
        raise HelperError(f"packed archive is corrupt at {broken}")
    print(f"Packed {len(parts)} part(s) from {args.directory} into {output}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
