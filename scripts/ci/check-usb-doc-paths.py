#!/usr/bin/env python3
"""
Fail CI if markdown docs reference emulator-only USB implementation file paths.

The canonical browser/WASM USB stack lives in:
  - crates/aero-usb
  - crates/aero-wasm
  - web/

The native emulator also has its own (legacy/native-only) USB implementation, but
docs should not point at `crates/emulator/src/io/usb/...` as the primary place to
look for browser runtime behavior. Those file paths have repeatedly confused
readers and can easily reappear during refactors.

This script is intentionally narrow: it only checks for the specific
`crates/emulator/src/io/usb` (and `emulator/src/io/usb`) substrings in markdown
files.
"""

from __future__ import annotations

import sys
from pathlib import Path


FORBIDDEN_SUBSTRINGS = (
    "crates/emulator/src/io/usb",
    "emulator/src/io/usb",
)


def iter_markdown_files(repo_root: Path) -> list[Path]:
    files: list[Path] = []

    docs_dir = repo_root / "docs"
    if docs_dir.exists():
        files.extend(sorted(docs_dir.rglob("*.md")))

    # The repo has a small set of per-workstream onboarding docs under
    # `instructions/`. These are effectively "docs" too, and have historically
    # been a source of stale path pointers during refactors.
    instructions_dir = repo_root / "instructions"
    if instructions_dir.exists():
        files.extend(sorted(instructions_dir.rglob("*.md")))

    agents = repo_root / "AGENTS.md"
    if agents.exists():
        files.append(agents)

    files.extend(sorted(repo_root.glob("README*.md")))
    return files


def main() -> int:
    repo_root = Path(__file__).resolve().parents[2]
    offenders: list[str] = []

    for path in iter_markdown_files(repo_root):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as err:
            offenders.append(f"{path}: failed to read: {err}")
            continue

        for idx, line in enumerate(text.splitlines(), start=1):
            for needle in FORBIDDEN_SUBSTRINGS:
                if needle in line:
                    rel = path.relative_to(repo_root)
                    offenders.append(f"{rel}:{idx}: contains forbidden substring {needle!r}")

    if offenders:
        sys.stderr.write(
            "Found emulator-only USB path pointers in docs. The browser/WASM USB stack lives in "
            "`crates/aero-usb` + `crates/aero-wasm` + `web/`; avoid pointing at "
            "`crates/emulator/src/io/usb/...` in markdown.\n\n"
        )
        sys.stderr.write("\n".join(offenders))
        sys.stderr.write("\n")
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
