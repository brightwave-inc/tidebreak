#!/usr/bin/env python3
"""Regenerate crates/openwave-sandbox-agent/documents-requirements.txt.

Run from the repository root:

    python3 scripts/generate-documents-requirements.py

Reads the top-level pins from the document skills' SKILL.md `deps` manifests
and from crates/openwave-code-execution/baseline_python_deps.txt (the baseline
set every execution backend guarantees), resolves their transitive closure for
the image's platform (linux cp311) with pip, and records every
file hash PyPI publishes for each resolved version. It also resolves the skill
pins and the baseline pins together for the local sandbox's fixed runtime
(macOS arm64 cp310), so a pin the local backend could never install — the
baseline set's whole promise — fails regeneration instead of making every local
cache-population pass retry it. The Dockerfile's documents stage installs the
result with `pip install --require-hashes`, so the publish moment trusts these
recorded digests rather than whatever the index serves that day. Also asserts
that every compiled package publishes an aarch64 manylinux wheel, so the arm64
image build cannot discover a missing wheel at publish time.
"""

import json
import pathlib
import re
import subprocess
import sys
import tempfile
import urllib.request

# The baseline packages guaranteed on every execution backend, declared once
# for the image, the local offline package cache, and the operating prompt.
BASELINE = pathlib.Path("crates/openwave-code-execution/baseline_python_deps.txt")

OUTPUT = pathlib.Path("crates/openwave-sandbox-agent/documents-requirements.txt")
LOCAL_SANDBOX_PLATFORM = "macosx_11_0_arm64"
LOCAL_SANDBOX_PYTHON = "3.10"
LOCAL_SANDBOX_ABI = "cp310"

HEADER = """\
# Hash-checked closure for the documents image variant. Installed by the
# Dockerfile's documents stage with `pip install --require-hashes`, so the
# publish moment trusts these recorded digests rather than whatever PyPI
# serves that day; a substituted artifact fails the build.
#
# The top-level pins mirror the document skills' SKILL.md `deps` manifests plus
# the baseline set in crates/openwave-code-execution/baseline_python_deps.txt
# (the sandbox-image-pins test enforces both locksteps); the rest is their
# pinned transitive closure. Each entry lists every file hash PyPI publishes
# for that version, so one file covers amd64, arm64, and pure wheels alike.
#
# Regenerate after bumping a skill pin or a baseline pin:
#   python3 scripts/generate-documents-requirements.py
"""


def baseline_pins():
    pins = [
        line.strip()
        for line in BASELINE.read_text().splitlines()
        if line.strip() and not line.startswith("#")
    ]
    if not pins:
        sys.exit(f"{BASELINE} declares no pins")
    return pins


def skill_pins():
    pins = []
    for manifest in sorted(pathlib.Path("skills").glob("*/SKILL.md")):
        # Only the python list carries pip pins; a host list (or any later
        # key) must not bleed into the capture.
        match = re.search(r"^deps:\s*\{.*?python:\s*\[([^\]]*)\]", manifest.read_text(), re.M)
        if match:
            pins += [entry.strip().strip('"') for entry in match.group(1).split(",")]
    return pins


def validate_local_sandbox(pins):
    with tempfile.TemporaryDirectory() as scratch:
        subprocess.run(
            [
                sys.executable, "-m", "pip", "download", "-q",
                "--dest", scratch, "--no-cache-dir", "--only-binary=:all:",
                "--platform", LOCAL_SANDBOX_PLATFORM,
                "--python-version", LOCAL_SANDBOX_PYTHON,
                "--implementation", "cp", "--abi", LOCAL_SANDBOX_ABI,
                *pins,
            ],
            check=True,
        )


def resolve_closure(pins):
    with tempfile.TemporaryDirectory() as scratch:
        subprocess.run(
            [
                sys.executable, "-m", "pip", "download", "-q",
                "--dest", scratch, "--no-cache-dir", "--only-binary=:all:",
                "--platform", "manylinux2014_x86_64",
                "--platform", "manylinux_2_17_x86_64",
                "--platform", "manylinux_2_28_x86_64",
                "--python-version", "3.11", "--implementation", "cp",
                *pins,
            ],
            check=True,
        )
        resolved = {}
        for wheel in pathlib.Path(scratch).glob("*.whl"):
            name, version = wheel.name.split("-")[:2]
            resolved[name.replace("_", "-").lower()] = version
        return resolved


def hash_lines(resolved):
    lines = []
    for name, version in sorted(resolved.items()):
        with urllib.request.urlopen(f"https://pypi.org/pypi/{name}/{version}/json") as response:
            release = json.load(response)
        files = [entry for entry in release["urls"] if not entry.get("yanked")]
        if not files:
            sys.exit(f"no unyanked files for {name}=={version}")
        wheels = [entry["filename"] for entry in files if entry["filename"].endswith(".whl")]
        pure = any(name.endswith("-none-any.whl") for name in wheels)
        arm = any("manylinux" in name and "aarch64" in name for name in wheels)
        if not pure and not arm:
            sys.exit(f"{name}=={version} has no pure or manylinux aarch64 wheel")
        hashes = sorted({entry["digests"]["sha256"] for entry in files})
        parts = [f"{name}=={version}"] + [f"--hash=sha256:{value}" for value in hashes]
        lines.append(" \\\n    ".join(parts))
    return lines


def main():
    skill_dependencies = skill_pins()
    baseline = baseline_pins()
    pins = skill_dependencies + baseline
    if len(pins) < len(baseline) + 5:
        sys.exit(f"expected at least five SKILL.md pins, found: {pins}")
    validate_local_sandbox(pins)
    resolved = resolve_closure(pins)
    for pin in pins:
        name, version = pin.split("==")
        if resolved.get(name.replace("_", "-").lower()) != version:
            sys.exit(f"resolution changed the pinned {pin}")
    OUTPUT.write_text(HEADER + "\n" + "\n".join(hash_lines(resolved)) + "\n")
    print(f"wrote {OUTPUT} with {len(resolved)} pins")


if __name__ == "__main__":
    main()
