#!/usr/bin/env python3
"""Contract tests for scripts/adr.py using isolated decision logs."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
ADR_TOOL = REPOSITORY_ROOT / "scripts" / "adr.py"
TODAY = "2026-08-27"

TEMPLATE = """# N. Decision Title

- Status: Proposed | Accepted | Superseded | Rejected
- Date: YYYY-MM-DD
- Owners: name or area
- Related: other records, docs, or migrations this one depends on
- Supersedes: record number, if any

## Context

Describe the problem.

## Decision

State the decision.

## Alternatives Considered

List alternatives.

## Consequences

Describe consequences.

## Validation

Describe validation.
"""


def adr(number: int, title: str, status: str = "Accepted") -> str:
    return f"""# {number}. {title}

- Status: {status}
- Date: 2026-08-13
- Owners: core
- Related: none
- Supersedes: none

## Context

Fixture context.

## Decision

Fixture decision.
"""


def run(root: Path, *arguments: str, expected: int = 0) -> str:
    result = subprocess.run(
        ["python3", str(ADR_TOOL), "--root", str(root), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    output = result.stdout + result.stderr
    if result.returncode != expected:
        raise AssertionError(
            f"{' '.join(arguments)}: expected exit {expected}, got {result.returncode}\n{output}"
        )
    return output


def fixture(temporary_root: Path, name: str) -> Path:
    root = temporary_root / name
    decisions = root / "docs" / "decisions"
    decisions.mkdir(parents=True)
    (decisions / "0000-template.md").write_text(TEMPLATE, encoding="utf-8")
    (decisions / "0001-first-decision.md").write_text(
        adr(1, "First Decision"),
        encoding="utf-8",
    )
    (decisions / "README.md").write_text("# Decision records\n", encoding="utf-8")
    run(root, "sync")
    return root


class AdrTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        self.temporary_root = Path(self._temporary.name)

    def tearDown(self) -> None:
        self._temporary.cleanup()

    def test_clean_check(self) -> None:
        root = fixture(self.temporary_root, "clean")
        output = run(root, "check")
        self.assertIn("manifest synchronized", output)

    def test_stale_manifest_is_rejected(self) -> None:
        root = fixture(self.temporary_root, "stale-manifest")
        manifest = root / "docs" / "decisions" / "manifest.txt"
        manifest.write_text(manifest.read_text(encoding="utf-8") + "junk\n", encoding="utf-8")
        output = run(root, "check", expected=65)
        self.assertIn("manifest is stale", output)

    def test_duplicate_number_is_rejected(self) -> None:
        root = fixture(self.temporary_root, "duplicate")
        (root / "docs" / "decisions" / "0001-other-decision.md").write_text(
            adr(1, "Other Decision"),
            encoding="utf-8",
        )
        output = run(root, "check", expected=65)
        self.assertIn("ADR numbers are duplicated", output)

    def test_new_adr_uses_next_number(self) -> None:
        root = fixture(self.temporary_root, "new")
        run(
            root,
            "new",
            "--title",
            "Second Decision",
            "--owners",
            "core",
            "--date",
            TODAY,
        )
        created = root / "docs" / "decisions" / "0002-second-decision.md"
        self.assertTrue(created.exists())
        text = created.read_text(encoding="utf-8")
        self.assertTrue(text.startswith("# 2. Second Decision"))
        self.assertIn("- Status: Proposed", text)
        self.assertIn(
            "0002  0002-second-decision.md",
            (root / "docs" / "decisions" / "manifest.txt").read_text(encoding="utf-8"),
        )
        run(root, "check")

    def test_renumber_while_another_duplicate_exists(self) -> None:
        root = fixture(self.temporary_root, "renumber-with-sibling-duplicate")
        decisions = root / "docs" / "decisions"
        (decisions / "0001-other-decision.md").write_text(
            adr(1, "Other Decision"),
            encoding="utf-8",
        )
        (decisions / "0002-second-decision.md").write_text(
            adr(2, "Second Decision"),
            encoding="utf-8",
        )
        run(root, "renumber", "0001-other-decision.md", "--number", "3")
        self.assertTrue((decisions / "0003-other-decision.md").exists())
        self.assertFalse((decisions / "0001-other-decision.md").exists())
        run(root, "check")

    def test_renumber_moves_file_and_links(self) -> None:
        root = fixture(self.temporary_root, "renumber")
        run(root, "new", "--title", "Second Decision", "--date", TODAY)
        readme = root / "docs" / "other.md"
        readme.write_text(
            "See [0002-second-decision.md](decisions/0002-second-decision.md).\n",
            encoding="utf-8",
        )
        output = run(
            root,
            "renumber",
            "docs/decisions/0002-second-decision.md",
            "--number",
            "5",
        )
        self.assertIn("0002-second-decision.md -> 0005-second-decision.md", output)
        self.assertFalse(
            (root / "docs" / "decisions" / "0002-second-decision.md").exists()
        )
        moved = root / "docs" / "decisions" / "0005-second-decision.md"
        self.assertTrue(moved.exists())
        self.assertTrue(moved.read_text(encoding="utf-8").startswith("# 5. Second Decision"))
        self.assertIn(
            "0005-second-decision.md",
            readme.read_text(encoding="utf-8"),
        )
        run(root, "check")

    def test_heading_mismatch_is_rejected(self) -> None:
        root = fixture(self.temporary_root, "heading")
        path = root / "docs" / "decisions" / "0001-first-decision.md"
        path.write_text(adr(2, "First Decision"), encoding="utf-8")
        output = run(root, "check", expected=65)
        self.assertIn("heading says 2", output)

    def test_amended_status_is_accepted(self) -> None:
        root = fixture(self.temporary_root, "amended")
        path = root / "docs" / "decisions" / "0001-first-decision.md"
        path.write_text(
            adr(1, "First Decision", "Proposed (amended 2026-08-21, see [Amendment](#a))"),
            encoding="utf-8",
        )
        run(root, "sync")
        run(root, "check")


if __name__ == "__main__":
    unittest.main()
