#!/usr/bin/env python3
"""
Guardrail: ensure Win7 virtio-blk doesn't regress to defining AerovblkFreeResources twice.

MSVC treats duplicate function definitions as a hard compile error, which previously broke
the Win7 virtio-blk driver build. This check is intentionally small and targeted so it can
run quickly in CI without requiring a full WDK/MSBuild environment.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def main() -> int:
    src = Path("drivers/windows7/virtio-blk/src/aero_virtio_blk.c")
    if not src.exists():
        # This repository layout is expected, but keep the check robust for forks.
        print(f"skip: {src} not found")
        return 0

    text = src.read_text(encoding="utf-8", errors="replace")
    matches = list(re.finditer(r"^\s*static\s+VOID\s+AerovblkFreeResources\s*\(", text, re.MULTILINE))

    if len(matches) != 1:
        print(f"error: expected exactly 1 'static VOID AerovblkFreeResources(' in {src}, found {len(matches)}")
        for m in matches:
            line = text.count("\n", 0, m.start()) + 1
            # Display the matched line to make CI failures easy to understand.
            line_text = text.splitlines()[line - 1] if line - 1 < len(text.splitlines()) else ""
            print(f"  match at line {line}: {line_text}")
        return 1

    print("ok: aero_virtio_blk.c contains a single AerovblkFreeResources definition")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

