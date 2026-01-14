#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 shared-surface `share_token` contract.

Background:
- D3D shared `HANDLE` numeric values are not stable cross-process (for real NT
  handles they are process-local; some stacks use token-style shared handles) and
  must NOT be used as the AeroGPU protocol `share_token`.
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
  potentially expensive allocation calls under `Adapter->AllocationsLock` (spin
  lock hold time / contention).
"""

from __future__ import annotations

import pathlib
import re
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

    # Prefer matching the actual function definition (avoid false matches on call
    # sites elsewhere in the file).
    m = re.search(rf"(?m)^\s*static\b[^\n;]*\b{re.escape(func_name)}\s*\(", text)
    if m is None:
        return None
    idx = m.start()

    # Find the first '{' after the name and then brace-match.
    brace_start = text.find("{", m.end())
    if brace_start < 0:
        return None
    # Reject forward declarations (prototype ends with ';').
    if text.find(";", m.end(), brace_start) != -1:
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

    - `AeroGpuShareTokenRefIncrementLocked` must not perform allocations while
      `Adapter->AllocationsLock` is held.
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

    # Accept a few common spellings (Adapter vs adapter) and spinlock acquire/release APIs.
    re_lock_release = re.compile(
        r"\bKeReleaseSpinLock(?:FromDpcLevel)?\s*\(\s*&\s*(?:Adapter|adapter)\s*->\s*AllocationsLock\b"
    )
    re_lock_acquire = re.compile(
        r"\bKeAcquireSpinLock(?:AtDpcLevel)?\s*\(\s*&\s*(?:Adapter|adapter)\s*->\s*AllocationsLock\b"
    )
    # Less common, but still acceptable.
    re_lock_acquire_raise = re.compile(
        r"\bKeAcquireSpinLockRaiseToDpc\s*\(\s*&\s*(?:Adapter|adapter)\s*->\s*AllocationsLock\b"
    )

    # Allocation APIs that can take global locks and/or significantly extend the
    # critical section. We conservatively treat lookaside allocs as allocations
    # too: they can fall back to pool allocation when the list is empty.
    alloc_needles = (
        "ExAllocatePoolWithTag(",
        "ExAllocatePool(",
        "ExAllocatePool2(",
        "ExAllocateFromNPagedLookasideList(",
        "ExAllocateFromPagedLookasideList(",
    )
    alloc_re = re.compile("|".join(re.escape(n) for n in alloc_needles))

    # Scan events by offset (more robust than line-based scanning if the call
    # spans multiple lines).
    events: list[tuple[int, str, str]] = []
    for m in re_lock_release.finditer(body):
        events.append((m.start(), "release", m.group(0)))
    for m in re_lock_acquire.finditer(body):
        events.append((m.start(), "acquire", m.group(0)))
    for m in re_lock_acquire_raise.finditer(body):
        events.append((m.start(), "acquire", m.group(0)))
    for m in alloc_re.finditer(body):
        events.append((m.start(), "alloc", m.group(0)))

    events.sort(key=lambda e: e[0])

    # This helper is named "*Locked" and is expected to be entered with the lock
    # held by the caller.
    lock_held = True
    for pos, kind, snippet in events:
        if kind == "release":
            lock_held = False
            continue
        if kind == "acquire":
            lock_held = True
            continue

        if kind == "alloc" and lock_held:
            # Convert body-relative offset to a file line number.
            file_line = base_line + body[:pos].count("\n")
            call_name = snippet.rstrip("(")
            errors.append(
                f"{kmd_path.relative_to(ROOT)}:{file_line}: {call_name} called while Adapter->AllocationsLock is held"
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
