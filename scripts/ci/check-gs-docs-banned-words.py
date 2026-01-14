#!/usr/bin/env python3
"""
CI guardrail: keep GS docs wording stable.

Two docs are treated as canonical for geometry-shader (GS) emulation status:

- docs/graphics/geometry-shader-emulation.md
- docs/graphics/status.md

Historically these docs have drifted during rebases/conflict resolutions and
reintroduced a couple of ambiguous bring-up terms:

- "accepted-but-ignored"
- "placeholder"

This script intentionally enforces that these substrings do not appear (case
insensitive) in those two files, so wording stays explicit (e.g. "synthetic
expansion" / "fallback path") and the docs remain aligned with implementation
behavior.
"""

from __future__ import annotations

import pathlib
import re
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()

FILES = [
    ROOT / "docs" / "graphics" / "geometry-shader-emulation.md",
    ROOT / "docs" / "graphics" / "status.md",
]

PATTERN = re.compile(r"accepted-but-ignored|placeholder", flags=re.IGNORECASE)


def main() -> int:
    errors: list[str] = []
    for path in FILES:
        rel = path.relative_to(ROOT)
        if not path.exists():
            errors.append(f"{rel}: missing (cannot validate GS wording)")
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for line_no, line in enumerate(text.splitlines(), start=1):
            if PATTERN.search(line):
                snippet = line.strip()
                if len(snippet) > 200:
                    snippet = snippet[:197] + "..."
                errors.append(f"{rel}:{line_no}: {snippet}")

    if errors:
        print("GS docs wording check failed.", file=sys.stderr)
        print("The following lines contain banned wording:", file=sys.stderr)
        for e in errors:
            print(f"- {e}", file=sys.stderr)
        print(
            "\nFix: replace with more explicit wording (for example: "
            "'synthetic expansion' / 'fallback path') and keep docs aligned with "
            "current executor behavior.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
