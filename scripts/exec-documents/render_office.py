#!/usr/bin/env python3
"""Convert DOCX/PPTX with LibreOffice and render the resulting PDF."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from _openwave_preview import (
    HelperError,
    ensure_preview_dir,
    existing_file,
    run_cli,
)
from render_pdf import render_document


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(
        description="Render DOCX or PPTX pages into preview/ through LibreOffice.",
    )
    cli.add_argument("document", help="DOCX or PPTX file inside the exec workspace")
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
    source = existing_file(args.document, [".docx", ".pptx"])
    preview = ensure_preview_dir(args.preview_dir)
    libreoffice = shutil.which("libreoffice") or shutil.which("soffice")
    if libreoffice is None:
        raise HelperError(
            "Office rendering requires the LibreOffice 'libreoffice' or 'soffice' executable"
        )

    with tempfile.TemporaryDirectory(prefix="openwave-office-") as temporary:
        temporary_path = Path(temporary)
        profile = temporary_path / "profile"
        profile.mkdir()
        environment = os.environ.copy()
        environment["HOME"] = temporary
        completed = subprocess.run(
            [
                libreoffice,
                "--headless",
                f"-env:UserInstallation={profile.resolve().as_uri()}",
                "--convert-to",
                "pdf",
                "--outdir",
                temporary,
                str(source.resolve()),
            ],
            capture_output=True,
            text=True,
            env=environment,
            check=False,
        )
        converted = temporary_path / f"{source.stem}.pdf"
        if completed.returncode != 0 or not converted.is_file():
            detail = (completed.stderr or completed.stdout).strip()
            raise HelperError(f"LibreOffice conversion failed: {detail or 'no PDF produced'}")
        renderer, outputs, count, omitted = render_document(
            converted,
            preview,
            args.pages,
            args.dpi,
        )

    names = ", ".join(path.name for path in outputs)
    print(
        f"Converted {source.name} with LibreOffice and rendered "
        f"{len(outputs) - 1} of {count} page(s) with {renderer}."
    )
    print(f"Preview images: {names}")
    if omitted:
        print(f"Skipped {omitted} selected page(s) past the 12-page script limit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
