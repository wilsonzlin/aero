#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 shared-surface `share_token` contract.

Background:
- User-mode shared `HANDLE` numeric values are not stable cross-process (for real
  NT handles they are process-local; some stacks use token-style shared handles)
  and must NOT be used as the AeroGPU protocol `share_token`.
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
  potentially expensive pool operations (alloc/free) under
  `Adapter->AllocationsLock` (spin lock hold time / contention).
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


POOL_OP_NEEDLES = (
    # Allocation APIs that can take global locks and/or significantly extend the
    # critical section. We conservatively treat lookaside allocs as allocations
    # too: they can fall back to pool allocation when the list is empty.
    "ExAllocatePoolWithTag(",
    "ExAllocatePool(",
    "ExAllocatePool2(",
    "ExAllocateFromNPagedLookasideList(",
    "ExAllocateFromPagedLookasideList(",
    # Pool/allocator frees can also be expensive on some stacks; keep them out
    # of spin-locked regions as well.
    "ExFreePoolWithTag(",
    "ExFreePool(",
    "ExFreeToNPagedLookasideList(",
    "ExFreeToPagedLookasideList(",
)


def check_no_pool_ops_under_allocations_lock_in_func(
    errors: list[str],
    *,
    kmd_path: pathlib.Path,
    text: str,
    func_name: str,
    lock_held_on_entry: bool,
) -> None:
    """
    Enforce: no potentially expensive pool operations while `Adapter->AllocationsLock` is held.
    """

    span = extract_c_function_body_span(text, func_name)
    if span is None:
        errors.append(f"{kmd_path.relative_to(ROOT)}: {func_name} not found")
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

    pool_op_re = re.compile("|".join(re.escape(n) for n in POOL_OP_NEEDLES))

    # Scan events by offset (more robust than line-based scanning if the call spans multiple lines).
    events: list[tuple[int, str, str]] = []
    for m in re_lock_release.finditer(body):
        events.append((m.start(), "release", m.group(0)))
    for m in re_lock_acquire.finditer(body):
        events.append((m.start(), "acquire", m.group(0)))
    for m in re_lock_acquire_raise.finditer(body):
        events.append((m.start(), "acquire", m.group(0)))
    for m in pool_op_re.finditer(body):
        events.append((m.start(), "pool", m.group(0)))

    events.sort(key=lambda e: e[0])

    lock_held = lock_held_on_entry
    for pos, kind, snippet in events:
        if kind == "release":
            lock_held = False
            continue
        if kind == "acquire":
            lock_held = True
            continue

        if kind == "pool" and lock_held:
            file_line = base_line + body[:pos].count("\n")
            call_name = snippet.rstrip("(")
            errors.append(
                f"{kmd_path.relative_to(ROOT)}:{file_line}: {call_name} called while Adapter->AllocationsLock is held ({func_name})"
            )


def check_no_pool_alloc_under_allocations_lock(errors: list[str]) -> None:
    """
    Guardrail for AGPU-ShareToken refcount tracking:

    - `AeroGpuShareTokenRefIncrementLocked` must not perform pool operations
      while `Adapter->AllocationsLock` is held.
    - The implementation is expected to drop the spin lock, allocate, then
      re-acquire and re-check before inserting.
    """

    kmd_path = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"
    if not kmd_path.exists():
        errors.append(f"{kmd_path.relative_to(ROOT)}: missing (cannot validate share-token locking guardrail)")
        return

    text = read_text(kmd_path)
    check_no_pool_ops_under_allocations_lock_in_func(
        errors,
        kmd_path=kmd_path,
        text=text,
        func_name="AeroGpuShareTokenRefIncrementLocked",
        lock_held_on_entry=True,
    )
    check_no_pool_ops_under_allocations_lock_in_func(
        errors,
        kmd_path=kmd_path,
        text=text,
        func_name="AeroGpuShareTokenRefDecrement",
        lock_held_on_entry=False,
    )
    check_no_pool_ops_under_allocations_lock_in_func(
        errors,
        kmd_path=kmd_path,
        text=text,
        func_name="AeroGpuFreeAllShareTokenRefs",
        lock_held_on_entry=False,
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
    check_track_allocation_share_token_order(errors)
    check_track_allocation_returns_boolean(errors)
    check_track_allocation_return_value_checked(errors)

    if errors:
        print("ERROR: AeroGPU shared-surface ShareToken contract regression detected.", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print("OK: AeroGPU shared-surface ShareToken contract checks passed.")
    return 0


def check_track_allocation_share_token_order(errors: list[str]) -> None:
    """
    Correctness guardrail for the share-token refcount tracking refactor:

    `AeroGpuShareTokenRefIncrementLocked` is permitted to temporarily drop and
    re-acquire `Adapter->AllocationsLock` in order to allocate a tracking node
    outside the spin lock.

    As a result, `AeroGpuTrackAllocation` must not make an allocation visible in
    `Adapter->Allocations` *before* incrementing the share-token refcount; doing
    so would allow another thread to observe/untrack the allocation while the
    share-token state is transiently untracked.
    """

    kmd_path = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"
    if not kmd_path.exists():
        errors.append(f"{kmd_path.relative_to(ROOT)}: missing (cannot validate AeroGpuTrackAllocation share-token ordering)")
        return

    text = read_text(kmd_path)
    span = extract_c_function_body_span(text, "AeroGpuTrackAllocation")
    if span is None:
        errors.append(f"{kmd_path.relative_to(ROOT)}: AeroGpuTrackAllocation not found")
        return
    brace_start, body = span
    base_line = text[:brace_start].count("\n") + 1

    re_inc = re.compile(r"\bAeroGpuShareTokenRefIncrementLocked\s*\(")
    re_insert_alloc = re.compile(
        r"\bInsert(?:Tail|Head)List\s*\(\s*&\s*(?:Adapter|adapter)\s*->\s*Allocations\b"
    )

    m_inc = re_inc.search(body)
    if m_inc is None:
        errors.append(f"{kmd_path.relative_to(ROOT)}:{base_line}: AeroGpuTrackAllocation missing call to AeroGpuShareTokenRefIncrementLocked")
        return
    m_insert = re_insert_alloc.search(body)
    if m_insert is None:
        errors.append(
            f"{kmd_path.relative_to(ROOT)}:{base_line}: AeroGpuTrackAllocation missing insertion into Adapter->Allocations (cannot validate share-token ordering)"
        )
        return

    if m_insert.start() < m_inc.start():
        file_line = base_line + body[: m_insert.start()].count("\n")
        errors.append(
            f"{kmd_path.relative_to(ROOT)}:{file_line}: AeroGpuTrackAllocation inserts into Adapter->Allocations before incrementing ShareTokenRefs; keep increment before insertion (increment helper may drop AllocationsLock)"
        )


def check_track_allocation_return_value_checked(errors: list[str]) -> None:
    """
    Guardrail: `AeroGpuTrackAllocation` returns `BOOLEAN` and must not be called
    as a standalone statement (ignored return value).

    If callers ignore the return value, a shared allocation can be exposed to
    dxgkrnl/user-mode without being inserted into `Adapter->Allocations` (tracking
    failed due to OOM), which breaks teardown/untrack logic and can leak
    resources.
    """

    kmd_path = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"
    if not kmd_path.exists():
        errors.append(f"{kmd_path.relative_to(ROOT)}: missing (cannot validate AeroGpuTrackAllocation call sites)")
        return

    text = read_text(kmd_path)
    for idx, line in enumerate(text.splitlines(), start=1):
        if re.match(r"^\s*AeroGpuTrackAllocation\s*\(", line):
            errors.append(
                f"{kmd_path.relative_to(ROOT)}:{idx}: AeroGpuTrackAllocation return value must be checked (do not ignore it)"
            )


def check_track_allocation_returns_boolean(errors: list[str]) -> None:
    """
    Guardrail: `AeroGpuTrackAllocation` must return `BOOLEAN`.

    The KMD must be able to fail CreateAllocation/OpenAllocation when share-token
    tracking cannot be established (OOM). Keeping this function's boolean return
    type makes it hard for future refactors to accidentally ignore that failure
    mode.
    """

    kmd_path = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"
    if not kmd_path.exists():
        errors.append(f"{kmd_path.relative_to(ROOT)}: missing (cannot validate AeroGpuTrackAllocation signature)")
        return

    text = read_text(kmd_path)
    m = re.search(r"(?m)^\s*static\b[^\n;]*\bAeroGpuTrackAllocation\s*\(", text)
    if m is None:
        errors.append(f"{kmd_path.relative_to(ROOT)}: AeroGpuTrackAllocation not found (cannot validate return type)")
        return

    sig = m.group(0)
    if "BOOLEAN" not in sig or "VOID" in sig:
        line_no = text[: m.start()].count("\n") + 1
        errors.append(
            f"{kmd_path.relative_to(ROOT)}:{line_no}: AeroGpuTrackAllocation must return BOOLEAN (found signature: {sig.strip()})"
        )


if __name__ == "__main__":
    raise SystemExit(main())
