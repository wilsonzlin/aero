#!/usr/bin/env python3
"""
Guardrail: prevent "virtqueue_split.h" include-path ambiguity in Windows 7 drivers.

Background
----------
Historically the repo had *two* different headers named `virtqueue_split.h`:
  - `drivers/windows7/virtio/common/include/virtqueue_split.h` (legacy/transitional; now renamed to `virtqueue_split_legacy.h`)
  - `drivers/windows/virtio/common/virtqueue_split.h` (modern shared engine)

That made header resolution depend on include path ordering, which is a footgun:
drivers could silently compile against the wrong API.

Policy
------
To keep includes unambiguous:
  - `drivers/windows/virtio/common/virtqueue_split.h` is the *only* header with that
    name in-tree, and
  - the Win7 common split-ring header is named `virtqueue_split_legacy.h`.

Shipped drivers are expected to include the canonical `virtqueue_split.h`; the
legacy header is retained for host-side unit tests and experiments. Regardless,
drivers must not rely on include path ordering to "pick the right one".
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

CANONICAL_HEADER = REPO_ROOT / "drivers/windows/virtio/common/virtqueue_split.h"
WIN7_DIRS = [
    REPO_ROOT / "drivers/windows7",
    REPO_ROOT / "drivers/win7",
]


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


INCLUDE_RE = re.compile(r'^\s*#\s*include\s*[<"]([^">]+)[">]')


def main() -> None:
    if not CANONICAL_HEADER.is_file():
        fail(f"missing canonical header: {CANONICAL_HEADER.relative_to(REPO_ROOT)}")

    # Ensure there is exactly one virtqueue_split.h in the repository tree.
    #
    # Use a case-insensitive match because Windows checkouts are case-insensitive.
    # (Git also struggles with case-only renames on Windows, so catching this in
    # CI avoids confusing breakage.)
    #
    # Prefer `git ls-files` so untracked local build artifacts can't trigger the
    # guardrail. Fall back to a filesystem walk if git is unavailable.
    found_rel: list[str] = []
    try:
        out = subprocess.check_output(
            ["git", "-C", str(REPO_ROOT), "ls-files"], text=True
        )
        for raw in out.splitlines():
            rel = raw.strip()
            if not rel:
                continue
            if Path(rel).name.lower() == "virtqueue_split.h":
                found_rel.append(rel)
    except (OSError, subprocess.CalledProcessError):
        for path in REPO_ROOT.rglob("*"):
            if path.is_file() and path.name.lower() == "virtqueue_split.h":
                found_rel.append(path.relative_to(REPO_ROOT).as_posix())

    found_rel = sorted(found_rel)
    if found_rel != [CANONICAL_HEADER.relative_to(REPO_ROOT).as_posix()]:
        fail(
            "expected exactly one 'virtqueue_split.h' (the canonical modern header), found:\n  - "
            + "\n  - ".join(found_rel)
        )

    # Ensure Win7 sources don't use explicit relative includes to virtqueue_split.h.
    # With only one header of that name, include order cannot matter; drivers should
    # be able to use `#include "virtqueue_split.h"` and rely on their include dirs.
    offenders: list[str] = []
    source_paths: list[Path] = []
    try:
        out = subprocess.check_output(
            ["git", "-C", str(REPO_ROOT), "ls-files"], text=True
        )
        tracked = [line.strip() for line in out.splitlines() if line.strip()]
        for rel in tracked:
            if not (rel.startswith("drivers/windows7/") or rel.startswith("drivers/win7/")):
                continue
            path = REPO_ROOT / rel
            if path.suffix.lower() not in {".c", ".h", ".cpp", ".inc"}:
                continue
            source_paths.append(path)
    except (OSError, subprocess.CalledProcessError):
        for root in WIN7_DIRS:
            if not root.is_dir():
                continue
            for path in root.rglob("*"):
                if not path.is_file():
                    continue
                if path.suffix.lower() not in {".c", ".h", ".cpp", ".inc"}:
                    continue
                source_paths.append(path)

    for path in sorted(set(source_paths)):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue

        for i, line in enumerate(text.splitlines(), start=1):
            m = INCLUDE_RE.match(line)
            if not m:
                continue
            inc = m.group(1)
            inc_lower = inc.lower()

            # Any path component in an include for virtqueue_split.h is a sign
            # we are compensating for include-path ambiguity.
            if inc_lower.endswith("virtqueue_split.h") and ("/" in inc or "\\" in inc):
                offenders.append(f"{path.relative_to(REPO_ROOT).as_posix()}:{i}: {inc}")

    if offenders:
        fail(
            "Win7 sources must not include 'virtqueue_split.h' via a relative/explicit path.\n"
            "Use `#include \"virtqueue_split.h\"` (canonical) and keep the include directories correct.\n"
            "Offending includes:\n  - "
            + "\n  - ".join(sorted(offenders))
        )

    print("ok: virtqueue_split.h is unambiguous and Win7 sources use the canonical header name")


if __name__ == "__main__":
    main()
