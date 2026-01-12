#!/usr/bin/env python3
"""CI guardrails for `crates/aero-d3d11`.

`aero-d3d11` should use the canonical guest-memory API paths from
`aero_gpu::guest_memory::{GuestMemory, GuestMemoryError, VecGuestMemory}` and must not rely on the
`aero_gpu::{...}` re-exports (which are easy to reach for in tests).

This script fails CI if regressions are detected.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TARGET_DIR = ROOT / "crates" / "aero-d3d11"


@dataclass(frozen=True)
class Match:
    path: Path
    line: int
    message: str


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def scan_file(path: Path) -> list[Match]:
    text = path.read_text(encoding="utf-8", errors="replace")
    rel = path.relative_to(ROOT)
    matches: list[Match] = []

    for token in ("aero-gpu-device", "aero_gpu_device"):
        idx = text.find(token)
        if idx != -1:
            matches.append(
                Match(
                    rel,
                    line_number(text, idx),
                    f"found forbidden token {token!r} (crate no longer exists)",
                )
            )

    direct_reexport_re = re.compile(r"\baero_gpu::(GuestMemory|GuestMemoryError|VecGuestMemory)\b")
    for m in direct_reexport_re.finditer(text):
        matches.append(
            Match(
                rel,
                line_number(text, m.start()),
                "use aero_gpu::guest_memory::{GuestMemory, GuestMemoryError, VecGuestMemory} instead "
                "of aero_gpu re-exports",
            )
        )

    # Catch `use aero_gpu::{GuestMemory, VecGuestMemory};` even when formatted across multiple lines.
    brace_import_re = re.compile(
        r"\buse\s+aero_gpu\s*::\s*\{[^}]*?\b(GuestMemory|GuestMemoryError|VecGuestMemory)\b",
        re.DOTALL,
    )
    for m in brace_import_re.finditer(text):
        matches.append(
            Match(
                rel,
                line_number(text, m.start()),
                "import guest-memory types from aero_gpu::guest_memory::{...} rather than "
                "use aero_gpu::{...} re-exports",
            )
        )

    return matches


def main() -> int:
    if not TARGET_DIR.is_dir():
        print(f"error: expected directory {TARGET_DIR} to exist", file=sys.stderr)
        return 2

    paths = sorted(
        [*TARGET_DIR.rglob("*.rs"), TARGET_DIR / "Cargo.toml"],
        key=lambda p: str(p),
    )

    matches: list[Match] = []
    for path in paths:
        if path.exists():
            matches.extend(scan_file(path))

    if matches:
        print("aero-d3d11 import policy check failed:", file=sys.stderr)
        for m in matches:
            print(f"  {m.path}:{m.line}: {m.message}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

