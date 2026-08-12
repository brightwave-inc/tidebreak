"""Shared, dependency-light helpers for Tidebreak's document scripts."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Iterable, Sequence


class HelperError(RuntimeError):
    """A concise, model-actionable command failure."""


def existing_file(value: str, suffixes: Sequence[str]) -> Path:
    path = Path(value).expanduser()
    if not path.is_file():
        raise HelperError(f"input file does not exist: {path}")
    if path.suffix.lower() not in suffixes:
        expected = ", ".join(suffixes)
        raise HelperError(f"expected one of {expected}, got: {path.name}")
    return path


def ensure_preview_dir(value: str) -> Path:
    path = Path(value)
    path.mkdir(parents=True, exist_ok=True)
    if not path.is_dir():
        raise HelperError(f"preview path is not a directory: {path}")
    return path


def remove_matching(directory: Path, patterns: Iterable[str]) -> None:
    for pattern in patterns:
        for path in directory.glob(pattern):
            if path.is_file():
                path.unlink()


def parse_pages(spec: str, page_count: int, limit: int = 12) -> tuple[list[int], int]:
    """Return one-based pages plus the count truncated by the safety limit."""

    if page_count < 1:
        raise HelperError("document has no pages")
    if spec.strip().lower() == "all":
        pages = list(range(1, page_count + 1))
    else:
        pages: list[int] = []
        for token in spec.split(","):
            token = token.strip()
            if not token:
                raise HelperError("page selection contains an empty item")
            match = re.fullmatch(r"(\d+)(?:-(\d+))?", token)
            if match is None:
                raise HelperError(
                    "pages must be 'all' or comma-separated numbers/ranges such as 1,3-5"
                )
            start = int(match.group(1))
            end = int(match.group(2) or start)
            if start < 1 or end < start or end > page_count:
                raise HelperError(
                    f"page range {token} is outside this {page_count}-page document"
                )
            pages.extend(range(start, end + 1))
        pages = list(dict.fromkeys(pages))
    omitted = max(0, len(pages) - limit)
    return pages[:limit], omitted


def require_pillow():
    try:
        from PIL import Image, ImageDraw, ImageOps
    except ImportError as error:
        raise HelperError(
            "Pillow is required to create preview images; install the Python 'Pillow' package"
        ) from error
    return Image, ImageDraw, ImageOps


def overview_grid(
    images: Sequence[tuple[Path, str]],
    destination: Path,
    *,
    cell_width: int = 420,
    cell_height: int = 320,
) -> None:
    """Build a bounded overview whose priority name wins the three-image scan."""

    if not images:
        raise HelperError("cannot build an overview without images")
    Image, ImageDraw, ImageOps = require_pillow()
    count = min(len(images), 6)
    columns = 2 if count > 1 else 1
    rows = (count + columns - 1) // columns
    label_height = 28
    canvas = Image.new(
        "RGB",
        (columns * cell_width, rows * (cell_height + label_height)),
        "white",
    )
    draw = ImageDraw.Draw(canvas)
    for index, (path, label) in enumerate(images[:count]):
        with Image.open(path) as source:
            thumbnail = ImageOps.contain(
                source.convert("RGB"),
                (cell_width - 16, cell_height - 16),
            )
        x = (index % columns) * cell_width
        y = (index // columns) * (cell_height + label_height)
        canvas.paste(
            thumbnail,
            (x + (cell_width - thumbnail.width) // 2, y + 8),
        )
        draw.rectangle(
            (x, y, x + cell_width - 1, y + cell_height + label_height - 1),
            outline="#b8bec8",
        )
        draw.text((x + 8, y + cell_height + 6), label[:64], fill="black")
    canvas.save(destination, format="PNG", optimize=True)


def slug(value: str, fallback: str = "sheet") -> str:
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "-", value.strip()).strip("-._")
    return (cleaned or fallback)[:48]


def run_cli(function) -> int:
    try:
        return int(function() or 0)
    except HelperError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    except Exception as error:  # Third-party parsers should fail without a traceback dump.
        print(f"error: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("error: interrupted", file=sys.stderr)
        return 130
