#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 shared-surface `share_token` contract.

Background:
- D3D shared `HANDLE` numeric values are process-local and must NOT be used as the
  AeroGPU protocol `share_token`.
- On Win7/WDDM 1.1, the canonical AeroGPU contract is that the Win7 KMD generates
  a stable non-zero `share_token` and persists it in the preserved WDDM allocation
  private driver data blob (`aerogpu_wddm_alloc_priv.share_token` in
  `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`). dxgkrnl returns the exact same
  bytes on cross-process opens, so both processes observe the same token.

This script is intentionally narrow and fast: it prevents accidental doc/protocol
comment drift back toward older, incorrect descriptions (for example: referencing
the deleted `aerogpu_alloc_privdata.h` model).
"""

from __future__ import annotations

import pathlib
import sys
from typing import Iterable


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()

# We only scan documentation and protocol headers/comments. We intentionally do
# not scan driver source trees where legacy identifiers may still appear.
SCAN_PATHS: list[pathlib.Path] = [
    ROOT / "docs",
    ROOT / "instructions",
    ROOT / "drivers" / "aerogpu" / "protocol",
    ROOT / "drivers" / "aerogpu" / "kmd" / "README.md",
    ROOT / "drivers" / "aerogpu" / "umd" / "d3d9" / "README.md",
    ROOT / "drivers" / "aerogpu" / "umd" / "d3d10_11" / "README.md",
    ROOT / "drivers" / "aerogpu" / "tests" / "win7" / "README.md",
]

# If this identifier shows up in docs/protocol commentary, it almost always means
# we're (incorrectly) referencing an obsolete share-token carrier model.
BANNED_SUBSTRINGS = [
    "aerogpu_alloc_privdata",
]

# These files must continue to reference the canonical share_token carrier field/header.
REQUIRED_SUBSTRINGS = {
    ROOT / "drivers" / "aerogpu" / "protocol" / "aerogpu_cmd.h": [
        "aerogpu_wddm_alloc_priv.share_token",
        "aerogpu_wddm_alloc.h",
    ],
    ROOT / "drivers" / "aerogpu" / "protocol" / "README.md": [
        "aerogpu_wddm_alloc_priv.share_token",
        "aerogpu_wddm_alloc.h",
    ],
    ROOT / "docs" / "16-d3d9ex-dwm-compatibility.md": [
        "aerogpu_wddm_alloc_priv.share_token",
        "aerogpu_wddm_alloc.h",
    ],
    ROOT / "docs" / "graphics" / "win7-shared-surfaces-share-token.md": [
        "aerogpu_wddm_alloc_priv.share_token",
        "aerogpu_wddm_alloc.h",
    ],
}


def iter_files(path: pathlib.Path) -> Iterable[pathlib.Path]:
    if path.is_file():
        yield path
        return
    if not path.is_dir():
        return
    for pat in ("*.md", "*.h"):
        yield from path.rglob(pat)


def read_text(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def main() -> int:
    errors: list[str] = []

    deprecated_header = ROOT / "drivers" / "aerogpu" / "protocol" / "aerogpu_alloc_privdata.h"
    if deprecated_header.exists():
        errors.append(
            f"{deprecated_header.relative_to(ROOT)}: deprecated header present; use drivers/aerogpu/protocol/aerogpu_wddm_alloc.h instead"
        )

    # Banned substring scan.
    for base in SCAN_PATHS:
        for path in iter_files(base):
            text = read_text(path)
            for banned in BANNED_SUBSTRINGS:
                if banned not in text:
                    continue
                # Find line numbers for actionable output.
                for idx, line in enumerate(text.splitlines(), start=1):
                    if banned in line:
                        rel = path.relative_to(ROOT)
                        errors.append(f"{rel}:{idx}: banned reference: {banned}")

    # Required substring checks.
    for path, required_list in REQUIRED_SUBSTRINGS.items():
        if not path.exists():
            errors.append(f"{path.relative_to(ROOT)}: required file missing")
            continue
        text = read_text(path)
        for required in required_list:
            if required not in text:
                errors.append(f"{path.relative_to(ROOT)}: missing required reference: {required}")

    if errors:
        print("ERROR: AeroGPU shared-surface ShareToken contract regression detected.", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print("OK: AeroGPU shared-surface ShareToken contract checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
