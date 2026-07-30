#!/usr/bin/env python3
"""Extract embedded raster figures from a PDF into preview/."""

from __future__ import annotations

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

from _openwave_preview import (
    HelperError,
    ensure_preview_dir,
    existing_file,
    overview_grid,
    remove_matching,
    run_cli,
)


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description="Extract embedded PDF figures and an overview into preview/.",
    )
    cli.add_argument("pdf", help="PDF file inside the exec workspace")
    cli.add_argument(
        "--max-figures",
        type=int,
        default=8,
        help="maximum figures to retain before preview scan capping (default: 8)",
    )
    cli.add_argument(
        "--preview-dir",
        default="preview",
        help="preview output directory (default: preview)",
    )
    return cli


def main() -> int:
    args = parser().parse_args()
    if not 1 <= args.max_figures <= 24:
        raise HelperError("max-figures must be between 1 and 24")
    source = existing_file(args.pdf, [".pdf"])
    preview = ensure_preview_dir(args.preview_dir)
    pdfimages = shutil.which("pdfimages")
    if pdfimages is None:
        raise HelperError(
            "embedded figure extraction requires Poppler's 'pdfimages' executable"
        )

    remove_matching(preview, ["overview-grid.png", "figure-*.png"])
    with tempfile.TemporaryDirectory(prefix="openwave-pdf-figures-") as temporary:
        prefix = Path(temporary) / "figure"
        completed = subprocess.run(
            [pdfimages, "-png", str(source), str(prefix)],
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            detail = (completed.stderr or completed.stdout).strip()
            raise HelperError(f"pdfimages failed: {detail or 'unknown error'}")
        extracted = sorted(Path(temporary).glob("figure-*.png"))
        retained = extracted[: args.max_figures]
        outputs: list[Path] = []
        for index, path in enumerate(retained, start=1):
            output = preview / f"figure-{index:03d}.png"
            shutil.copyfile(path, output)
            outputs.append(output)

    if not outputs:
        print("No embedded raster figures were found.")
        return 0
    overview = preview / "overview-grid.png"
    overview_grid(
        [(path, f"Figure {index}") for index, path in enumerate(outputs, start=1)],
        overview,
    )
    names = ", ".join(path.name for path in [overview, *outputs])
    print(f"Extracted {len(outputs)} embedded figure(s).")
    print(f"Preview images: {names}")
    if len(extracted) > len(outputs):
        print(
            f"Skipped {len(extracted) - len(outputs)} figure(s) past the "
            f"{args.max_figures}-figure script limit."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
