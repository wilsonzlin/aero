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

This script is intentionally narrow and fast:

- Prevent accidental doc/protocol comment drift back toward older, incorrect
  descriptions (for example: referencing the deleted
  `aerogpu_alloc_privdata.h` model).
- Enforce a key KMD invariant for share-token refcount tracking: avoid
  `ExAllocatePoolWithTag` under `Adapter->AllocationsLock` (spin lock hold time /
  contention).
"""

from __future__ import annotations

import pathlib
import sys
from typing import Iterable


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()

# For the doc/protocol contract checks, we only scan documentation and protocol
# headers/comments. We intentionally do not scan driver source trees where legacy
# identifiers may still appear. (We do read the KMD source for a targeted
# lock/allocate guardrail; see check_no_pool_alloc_under_allocations_lock.)
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


def extract_c_function_body_span(text: str, func_name: str) -> tuple[int, str] | None:
    """
    Returns (brace_start_offset, body_with_outer_braces) for a C function in
    `text`, or None if not found.

    This is intentionally lightweight: it only needs to be robust enough to
    extract a single known KMD helper for a CI guardrail.
    """

    idx = text.find(func_name)
    if idx < 0:
        return None

    # Find the first '{' after the name and then brace-match.
    brace_start = text.find("{", idx)
    if brace_start < 0:
        return None

    depth = 0
    for end in range(brace_start, len(text)):
        c = text[end]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return brace_start, text[brace_start : end + 1]

    return None


def check_no_pool_alloc_under_allocations_lock(errors: list[str]) -> None:
    """
    Guardrail for AGPU-ShareToken refcount tracking:

    - `AeroGpuShareTokenRefIncrementLocked` must not call `ExAllocatePoolWithTag`
      while `Adapter->AllocationsLock` is held.
    - The implementation is expected to drop the spin lock, allocate, then
      re-acquire and re-check before inserting.
    """

    kmd_path = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"
    if not kmd_path.exists():
        errors.append(f"{kmd_path.relative_to(ROOT)}: missing (cannot validate share-token locking guardrail)")
        return

    text = read_text(kmd_path)
    span = extract_c_function_body_span(text, "AeroGpuShareTokenRefIncrementLocked")
    if span is None:
        errors.append(f"{kmd_path.relative_to(ROOT)}: AeroGpuShareTokenRefIncrementLocked not found")
        return
    brace_start, body = span
    base_line = text[:brace_start].count("\n") + 1

    # This helper is named "*Locked" and is expected to be entered with the lock
    # held by the caller.
    lock_held = True
    for idx, raw_line in enumerate(body.splitlines(), start=0):
        file_line = base_line + idx
        line = raw_line.strip()
        if "KeReleaseSpinLock(&Adapter->AllocationsLock" in line:
            lock_held = False
        elif "KeAcquireSpinLock(&Adapter->AllocationsLock" in line:
            lock_held = True

        if "ExAllocatePoolWithTag(" in line and lock_held:
            errors.append(
                f"{kmd_path.relative_to(ROOT)}:{file_line}: ExAllocatePoolWithTag called while Adapter->AllocationsLock is held"
            )


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

    check_no_pool_alloc_under_allocations_lock(errors)

    if errors:
        print("ERROR: AeroGPU shared-surface ShareToken contract regression detected.", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print("OK: AeroGPU shared-surface ShareToken contract checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
