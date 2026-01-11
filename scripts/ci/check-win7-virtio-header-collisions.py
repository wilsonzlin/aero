#!/usr/bin/env python3
"""
Guardrail: prevent header include-path ambiguity across Win7 virtio drivers.

Win7 drivers in this repo compile with a mix of include roots pulled from:
  - drivers/windows7/virtio/common (legacy + shims)
  - drivers/windows/virtio/common (canonical split virtqueue engine + helpers)
  - drivers/windows/virtio/pci-modern (canonical modern transport)
  - drivers/win7/virtio/virtio-core (virtio-core capability parser + layouts)

If two different headers share the same "virtual include path" (e.g. both expose
`virtqueue_split.h` at the root), then `#include "virtqueue_split.h"` becomes an
include-order footgun and can silently bind drivers to the wrong API.

This check scans the shared include roots used by Win7 virtio drivers and fails
if any header relative-path is present in more than one root.

Notes
-----
- We intentionally *do not* scan per-driver include directories (e.g.
  drivers/windows7/virtio-snd/include) because those headers are private to the
  driver and are normally searched first. The footgun we want to prevent is
  collisions between *shared* libraries.
"""

from __future__ import annotations

import sys
from collections import defaultdict
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

INCLUDE_ROOTS = [
    # Win7 virtio "common" library (legacy + OS shims).
    REPO_ROOT / "drivers/windows7/virtio/common/include",
    REPO_ROOT / "drivers/windows7/virtio/common/os_shim",
    # Canonical portable virtio libraries (shared across Windows drivers).
    REPO_ROOT / "drivers/windows/virtio/common",
    REPO_ROOT / "drivers/windows/virtio/pci-modern",
    # virtio-core headers used by Win7 drivers and tests.
    REPO_ROOT / "drivers/win7/virtio/virtio-core/include",
    REPO_ROOT / "drivers/win7/virtio/virtio-core/portable",
]

HEADER_SUFFIXES = {".h", ".inc"}


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def relposix(path: Path) -> str:
    return path.relative_to(REPO_ROOT).as_posix()


def main() -> None:
    # Map virtual include path (relative to include root) -> list of real paths.
    include_map: dict[str, list[str]] = defaultdict(list)

    for root in INCLUDE_ROOTS:
        if not root.is_dir():
            continue

        for path in root.rglob("*"):
            if not path.is_file():
                continue
            if path.suffix.lower() not in HEADER_SUFFIXES:
                continue

            virtual = path.relative_to(root).as_posix()
            include_map[virtual].append(relposix(path))

    collisions = {k: v for k, v in include_map.items() if len(v) > 1}
    if collisions:
        lines: list[str] = []
        for virtual in sorted(collisions.keys()):
            lines.append(f"- {virtual}")
            for real in sorted(collisions[virtual]):
                lines.append(f"    - {real}")
        fail(
            "duplicate header virtual paths across Win7 virtio include roots (include-order footgun):\n"
            + "\n".join(lines)
        )

    print("ok: no Win7 virtio header collisions across shared include roots")


if __name__ == "__main__":
    main()

