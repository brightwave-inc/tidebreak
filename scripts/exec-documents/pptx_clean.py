#!/usr/bin/env python3
"""Validate an unpacked PPTX tree before packing it back up.

Checks two things that a hand edit breaks and PowerPoint reports only as
"needs repair": every XML part still parses, and every relationship target
still resolves to a part that exists. It reports; it never rewrites.
"""

from __future__ import annotations

from _openwave_ooxml import check_tree, parser
from _openwave_preview import run_cli


def main() -> int:
    args = parser("PPTX").parse_args()
    return check_tree(args.directory)


if __name__ == "__main__":
    raise SystemExit(run_cli(main))
