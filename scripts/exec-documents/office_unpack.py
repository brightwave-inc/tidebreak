#!/usr/bin/env python3
"""Unpack an OOXML file (PPTX/DOCX/XLSX) into a directory tree for XML editing.

Entries are written exactly as stored — no reformatting — so a later
office_pack.py round trip changes only the parts that were edited.
"""

from __future__ import annotations

import argparse
import zipfile
from pathlib import Path

from _openwave_preview import HelperError, existing_file, run_cli

OOXML_SUFFIXES = [".pptx", ".docx", ".xlsx", ".xlsm"]


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description="Unzip an OOXML package into a directory tree for direct XML edits.",
    )
    cli.add_argument("document", help="PPTX, DOCX, or XLSX file inside the exec workspace")
    cli.add_argument("directory", help="destination directory, created if missing")
    return cli


def safe_destination(root: Path, name: str) -> Path:
    """Resolve a zip entry under `root`, refusing traversal and absolute paths."""

    if name.startswith("/") or "\\" in name:
        raise HelperError(f"package entry has an unsafe name: {name}")
    target = (root / name).resolve()
    if target != root and root not in target.parents:
        raise HelperError(f"package entry escapes the destination: {name}")
    return target


def main() -> int:
    args = parser().parse_args()
    source = existing_file(args.document, OOXML_SUFFIXES)
    root = Path(args.directory)
    if root.exists() and any(root.iterdir()):
        raise HelperError(f"destination directory is not empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    root = root.resolve()

    try:
        archive = zipfile.ZipFile(source)
    except zipfile.BadZipFile as error:
        raise HelperError(f"{source.name} is not a readable OOXML package: {error}") from error

    written = 0
    with archive:
        for entry in archive.infolist():
            destination = safe_destination(root, entry.filename)
            if entry.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(archive.read(entry))
            written += 1

    if not (root / "[Content_Types].xml").is_file():
        raise HelperError(
            f"{source.name} has no [Content_Types].xml; it is not a valid OOXML package"
        )
    print(f"Unpacked {source.name} into {args.directory} ({written} parts).")
    print("Edit the XML parts in place, then run pptx_clean.py and office_pack.py.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
