#!/usr/bin/env python3
"""Render DOCX/PPTX pages, through LibreOffice or a host-converted PDF.

Conversion to PDF is tried in order: a LibreOffice inside this sandbox
(container images that ship one), then a PDF the host converted after the
file landed in output/ (staged at .tidebreak/render/<name>.pdf). When
neither exists the error says exactly what to do next.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from _tidebreak_preview import (
    HelperError,
    ensure_preview_dir,
    existing_file,
    run_cli,
)
from render_pdf import render_document

HOST_RENDER_DIR = Path(".tidebreak/render")


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


def host_converted_pdf(source: Path) -> Path | None:
    """The host-side conversion of `source`, if one has been staged.

    The host mirrors office files under output/ into
    .tidebreak/render/<path relative to output>.pdf after each successful
    command; on a managed sandbox that PDF must be listed in a call's
    'files' to appear here.
    """
    try:
        relative = source.resolve().relative_to((Path.cwd() / "output").resolve())
    except ValueError:
        return None
    candidate = HOST_RENDER_DIR / relative.parent / f"{relative.name}.pdf"
    return candidate if candidate.is_file() else None


def convert_with_libreoffice(libreoffice: str, source: Path, out_dir: Path) -> Path:
    profile = out_dir / "profile"
    profile.mkdir()
    environment = os.environ.copy()
    environment["HOME"] = str(out_dir)
    completed = subprocess.run(
        [
            libreoffice,
            "--headless",
            f"-env:UserInstallation={profile.resolve().as_uri()}",
            "--convert-to",
            "pdf",
            "--outdir",
            str(out_dir),
            str(source.resolve()),
        ],
        capture_output=True,
        text=True,
        env=environment,
        check=False,
    )
    converted = out_dir / f"{source.stem}.pdf"
    if completed.returncode != 0 or not converted.is_file():
        detail = (completed.stderr or completed.stdout).strip()
        raise HelperError(f"LibreOffice conversion failed: {detail or 'no PDF produced'}")
    return converted


def main() -> int:
    args = parser().parse_args()
    if not 72 <= args.dpi <= 300:
        raise HelperError("dpi must be between 72 and 300")
    source = existing_file(args.document, [".docx", ".pptx"])
    preview = ensure_preview_dir(args.preview_dir)
    libreoffice = shutil.which("libreoffice") or shutil.which("soffice")

    if libreoffice is not None:
        with tempfile.TemporaryDirectory(prefix="tidebreak-office-") as temporary:
            converted = convert_with_libreoffice(libreoffice, source, Path(temporary))
            renderer, outputs, count, omitted = render_document(
                converted,
                preview,
                args.pages,
                args.dpi,
            )
        converted_how = "with LibreOffice"
    else:
        staged = host_converted_pdf(source)
        if staged is None:
            expected = HOST_RENDER_DIR / f"{source.name}.pdf"
            raise HelperError(
                "LibreOffice is not available in this sandbox, and no host-converted PDF "
                f"was staged at {expected}. The host converts office files saved under "
                "output/ after each successful command — check that command's workspace "
                "sync notes for the PDF's path, and on a managed sandbox list the PDF in "
                "this call's 'files' so it is staged here. If the notes said office "
                "rendering is unavailable on the host, skip the visual pass: validate by "
                "reopening the file with its library and say the visual check was not "
                "possible."
            )
        renderer, outputs, count, omitted = render_document(
            staged,
            preview,
            args.pages,
            args.dpi,
        )
        converted_how = "on the host"

    names = ", ".join(path.name for path in outputs)
    print(
        f"Converted {source.name} {converted_how} and rendered "
        f"{len(outputs) - 1} of {count} page(s) with {renderer}."
    )
    print(f"Preview images: {names}")
    if omitted:
        print(f"Skipped {omitted} selected page(s) past the 12-page script limit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
