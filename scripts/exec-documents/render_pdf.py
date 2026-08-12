#!/usr/bin/env python3
"""Render selected PDF pages plus an overview thumbnail into preview/."""

from __future__ import annotations

import argparse
from pathlib import Path

from _tidebreak_preview import (
    HelperError,
    ensure_preview_dir,
    existing_file,
    overview_grid,
    parse_pages,
    remove_matching,
    run_cli,
)


def _pdfium_renderer(source: Path, pages: list[int], dpi: int):
    try:
        import pypdfium2 as pdfium
    except ImportError:
        return None

    document = pdfium.PdfDocument(str(source))
    if not pages:
        return len(document), []
    rendered = []
    scale = dpi / 72
    for page_number in pages:
        bitmap = document[page_number - 1].render(scale=scale)
        rendered.append((page_number, bitmap.to_pil()))
    return len(document), rendered


def _pdf2image_page_count(source: Path) -> int | None:
    try:
        from pdf2image import pdfinfo_from_path
    except ImportError:
        return None
    info = pdfinfo_from_path(str(source))
    return int(info["Pages"])


def _pdf2image_renderer(source: Path, pages: list[int], dpi: int):
    try:
        from pdf2image import convert_from_path
    except ImportError:
        return None

    rendered = []
    for page_number in pages:
        images = convert_from_path(
            str(source),
            dpi=dpi,
            first_page=page_number,
            last_page=page_number,
            fmt="png",
            single_file=True,
        )
        if len(images) != 1:
            raise HelperError(f"PDF renderer returned no image for page {page_number}")
        rendered.append((page_number, images[0]))
    return rendered


def page_count(source: Path) -> tuple[str, int]:
    pdfium = _pdfium_renderer(source, [], 72)
    if pdfium is not None:
        return "pypdfium2", pdfium[0]
    count = _pdf2image_page_count(source)
    if count is not None:
        return "pdf2image", count
    raise HelperError(
        "PDF rendering requires pypdfium2 or pdf2image with Poppler; "
        "install one of those Python toolchains"
    )


def render_document(
    source: Path,
    preview_dir: Path,
    pages_spec: str,
    dpi: int,
) -> tuple[str, list[Path], int, int]:
    renderer, count = page_count(source)
    pages, omitted = parse_pages(pages_spec, count)
    if renderer == "pypdfium2":
        pdfium = _pdfium_renderer(source, pages, dpi)
        if pdfium is None:
            raise HelperError("pypdfium2 became unavailable during rendering")
        rendered = pdfium[1]
    else:
        rendered = _pdf2image_renderer(source, pages, dpi)
        if rendered is None:
            raise HelperError("pdf2image became unavailable during rendering")

    remove_matching(preview_dir, ["overview-grid.png", "page-*.png"])
    outputs: list[Path] = []
    for page_number, image in rendered:
        output = preview_dir / f"page-{page_number:04d}.png"
        image.convert("RGB").save(output, format="PNG", optimize=True)
        outputs.append(output)
    overview = preview_dir / "overview-grid.png"
    overview_grid(
        [(path, f"Page {page}") for path, page in zip(outputs, pages)],
        overview,
    )
    return renderer, [overview, *outputs], count, omitted


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description="Render PDF pages and a thumbnail overview into preview/.",
    )
    cli.add_argument("pdf", help="PDF file inside the exec workspace")
    cli.add_argument(
        "--pages",
        default="1",
        help="one-based pages, ranges such as 1,3-5, or all (default: 1)",
    )
    cli.add_argument(
        "--dpi",
        type=int,
        default=144,
        help="render resolution from 72 through 300 DPI (default: 144)",
    )
    cli.add_argument(
        "--preview-dir",
        default="preview",
        help="preview output directory (default: preview)",
    )
    return cli


def main() -> int:
    args = parser().parse_args()
    if not 72 <= args.dpi <= 300:
        raise HelperError("dpi must be between 72 and 300")
    source = existing_file(args.pdf, [".pdf"])
    preview = ensure_preview_dir(args.preview_dir)
    renderer, outputs, count, omitted = render_document(
        source,
        preview,
        args.pages,
        args.dpi,
    )
    names = ", ".join(path.name for path in outputs)
    print(f"Rendered {len(outputs) - 1} of {count} PDF page(s) with {renderer}.")
    print(f"Preview images: {names}")
    if omitted:
        print(f"Skipped {omitted} selected page(s) past the 12-page script limit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
