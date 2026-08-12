"""Shared validation for unpacked OOXML package trees.

The two checks below are what a hand edit breaks and Word or PowerPoint
reports only as "needs repair": every XML part still parses, and every
relationship target still resolves to a part that exists. Neither depends on
the package format, so `pptx_clean.py` and `docx_clean.py` are thin CLIs over
this module. Nothing here rewrites a file.
"""

from __future__ import annotations

import argparse
import posixpath
import urllib.parse
import xml.etree.ElementTree as ElementTree
from pathlib import Path

from _tidebreak_preview import HelperError

RELATIONSHIP_TAG = "{http://schemas.openxmlformats.org/package/2006/relationships}Relationship"


def parser(package_label: str) -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description=(
            f"Check XML well-formedness and relationship targets in an unpacked "
            f"{package_label} tree."
        ),
    )
    cli.add_argument("directory", help="directory produced by office_unpack.py")
    return cli


def xml_parts(root: Path) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in {".xml", ".rels"}
    )


def check_well_formed(root: Path, parts: list[Path], problems: list[str]) -> None:
    for path in parts:
        try:
            ElementTree.parse(path)
        except ElementTree.ParseError as error:
            problems.append(f"{path.relative_to(root)}: malformed XML ({error})")


def check_relationships(root: Path, parts: list[Path], problems: list[str]) -> None:
    for path in parts:
        if path.suffix.lower() != ".rels" or path.parent.name != "_rels":
            continue
        try:
            tree = ElementTree.parse(path)
        except ElementTree.ParseError:
            continue  # Already reported as malformed.
        # A part's relationships resolve against the directory holding _rels.
        base = path.parent.parent
        for relationship in tree.getroot().iter(RELATIONSHIP_TAG):
            if relationship.get("TargetMode") == "External":
                continue
            target = relationship.get("Target")
            identifier = relationship.get("Id", "<no Id>")
            if not target:
                problems.append(f"{path.relative_to(root)}: {identifier} has no Target")
                continue
            cleaned = urllib.parse.unquote(target.split("#", 1)[0].split("?", 1)[0])
            if cleaned.startswith("/"):
                resolved = root / cleaned.lstrip("/")
            else:
                resolved = base / posixpath.normpath(cleaned)
            if not resolved.is_file():
                problems.append(
                    f"{path.relative_to(root)}: {identifier} points at a missing part "
                    f"({target})"
                )


def check_tree(directory: str) -> int:
    root = Path(directory)
    if not root.is_dir():
        raise HelperError(f"unpacked directory does not exist: {root}")
    if not (root / "[Content_Types].xml").is_file():
        raise HelperError(f"{root}/[Content_Types].xml is missing; this is not an OOXML tree")

    parts = xml_parts(root)
    if not parts:
        raise HelperError(f"no XML parts found under {root}")
    problems: list[str] = []
    check_well_formed(root, parts, problems)
    check_relationships(root, parts, problems)

    if problems:
        for problem in problems:
            print(f"error: {problem}")
        raise HelperError(f"{len(problems)} problem(s) found; fix them before packing")
    print(f"Checked {len(parts)} XML part(s) under {root}: well-formed, relationships resolve.")
    return 0
