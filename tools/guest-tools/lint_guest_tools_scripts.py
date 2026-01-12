#!/usr/bin/env python3
"""
Static guardrail linter for Aero Guest Tools scripts.

Why this exists:
- `guest-tools/setup.cmd` and `guest-tools/verify.ps1` implement safety-critical
  behaviours (boot-critical virtio-blk pre-seeding + signature enforcement policy).
- These behaviours are hard to integration-test in CI (Win7-only, registry/BCD changes).
- This linter provides a lightweight, cross-platform check that fails CI if critical
  logic is accidentally removed or renamed.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, List, Sequence


class LintError(RuntimeError):
    pass


REPO_ROOT = Path(__file__).resolve().parents[2]


DEFAULT_SETUP_CMD = REPO_ROOT / "guest-tools" / "setup.cmd"
DEFAULT_UNINSTALL_CMD = REPO_ROOT / "guest-tools" / "uninstall.cmd"
DEFAULT_VERIFY_PS1 = REPO_ROOT / "guest-tools" / "verify.ps1"


@dataclass(frozen=True)
class Invariant:
    description: str
    expected_hint: str
    predicate: Callable[[str], bool]


def _contains(substr: str) -> Callable[[str], bool]:
    return lambda text: substr in text


def _all_contains(substrings: Sequence[str]) -> Callable[[str], bool]:
    return lambda text: all(s in text for s in substrings)


def _regex(pattern: str, *, flags: int = re.IGNORECASE | re.MULTILINE) -> Callable[[str], bool]:
    rx = re.compile(pattern, flags)
    return lambda text: rx.search(text) is not None


def _all_regex(patterns: Sequence[str], *, flags: int = re.IGNORECASE | re.MULTILINE) -> Callable[[str], bool]:
    compiled = [re.compile(p, flags) for p in patterns]
    return lambda text: all(rx.search(text) is not None for rx in compiled)


def _read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError as e:
        raise LintError(f"File not found: {path}") from e
    except OSError as e:
        raise LintError(f"Failed to read {path}: {e}") from e


def lint_text(*, path: Path, text: str, invariants: Sequence[Invariant]) -> List[str]:
    errors: List[str] = []
    for inv in invariants:
        if inv.predicate(text):
            continue
        errors.append(
            "\n".join(
                [
                    f"{path}: missing invariant: {inv.description}",
                    f"  Expected: {inv.expected_hint}",
                ]
            )
        )
    return errors


def lint_files(*, setup_cmd: Path, uninstall_cmd: Path, verify_ps1: Path) -> List[str]:
    errors: List[str] = []

    setup_text = _read_text(setup_cmd)
    uninstall_text = _read_text(uninstall_cmd)
    verify_text = _read_text(verify_ps1)

    setup_invariants = [
        Invariant(
            description="CriticalDeviceDatabase base path is referenced (boot-critical storage preseed)",
            expected_hint=r"HKLM\\SYSTEM\\CurrentControlSet\\Control\\CriticalDeviceDatabase",
            predicate=_contains(r"HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase"),
        ),
        Invariant(
            description="CriticalDeviceDatabase is referenced (boot-critical storage preseed)",
            expected_hint="CriticalDeviceDatabase",
            predicate=_contains("CriticalDeviceDatabase"),
        ),
        Invariant(
            description="Uses AERO_VIRTIO_BLK_SERVICE (storage service name from config/devices.cmd)",
            expected_hint="AERO_VIRTIO_BLK_SERVICE",
            predicate=_contains("AERO_VIRTIO_BLK_SERVICE"),
        ),
        Invariant(
            description="Uses AERO_VIRTIO_BLK_HWIDS (HWID list used for CriticalDeviceDatabase preseed)",
            expected_hint="AERO_VIRTIO_BLK_HWIDS",
            predicate=_contains("AERO_VIRTIO_BLK_HWIDS"),
        ),
        Invariant(
            description="Sets storage service Start=0 (BOOT_START) via reg.exe",
            expected_hint="/v Start /t REG_DWORD /d 0",
            predicate=_regex(r"/v\s+Start\s+/t\s+REG_DWORD\s+/d\s+0\b"),
        ),
        Invariant(
            description="Supports signing_policy surface (test|production|none)",
            expected_hint='Validates SIGNING_POLICY against "test", "production", and "none"',
            predicate=_all_regex(
                [
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"test"',
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"production"',
                    r'(?i)%SIGNING_POLICY%"\s*==\s*"none"',
                ]
            ),
        ),
        Invariant(
            description="Supports /testsigning flag (x64 test driver signing)",
            expected_hint="/testsigning",
            predicate=_contains("/testsigning"),
        ),
        Invariant(
            description="Supports /nointegritychecks flag (signature enforcement off; not recommended)",
            expected_hint="/nointegritychecks",
            predicate=_contains("/nointegritychecks"),
        ),
        Invariant(
            description="Supports /forcesigningpolicy:none|test|production flag",
            expected_hint="/forcesigningpolicy:none, /forcesigningpolicy:test, /forcesigningpolicy:production",
            predicate=_all_contains(
                ["/forcesigningpolicy:none", "/forcesigningpolicy:test", "/forcesigningpolicy:production"]
            ),
        ),
    ]

    verify_invariants = [
        Invariant(
            description="virtio-blk boot-critical registry check exists (CriticalDeviceDatabase)",
            expected_hint="CriticalDeviceDatabase",
            predicate=_contains("CriticalDeviceDatabase"),
        ),
        Invariant(
            description="virtio-blk boot-critical registry check key is present (virtio_blk_boot_critical)",
            expected_hint="virtio_blk_boot_critical",
            predicate=_contains("virtio_blk_boot_critical"),
        ),
        Invariant(
            description="manifest.json signing_policy is parsed (verify reports effective signing policy)",
            expected_hint="manifest.json + signing_policy",
            predicate=_all_contains(["manifest.json", "signing_policy"]),
        ),
    ]

    uninstall_invariants = [
        Invariant(
            description="Uninstaller references marker file for testsigning enabled by Guest Tools",
            expected_hint="testsigning.enabled-by-aero.txt",
            predicate=_contains("testsigning.enabled-by-aero.txt"),
        ),
        Invariant(
            description="Uninstaller references marker file for nointegritychecks enabled by Guest Tools",
            expected_hint="nointegritychecks.enabled-by-aero.txt",
            predicate=_contains("nointegritychecks.enabled-by-aero.txt"),
        ),
    ]

    errors.extend(lint_text(path=setup_cmd, text=setup_text, invariants=setup_invariants))
    errors.extend(lint_text(path=verify_ps1, text=verify_text, invariants=verify_invariants))
    errors.extend(lint_text(path=uninstall_cmd, text=uninstall_text, invariants=uninstall_invariants))

    return errors


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Static linter for Guest Tools safety-critical scripts.")
    parser.add_argument("--setup-cmd", type=Path, default=DEFAULT_SETUP_CMD, help="Path to guest-tools/setup.cmd")
    parser.add_argument("--uninstall-cmd", type=Path, default=DEFAULT_UNINSTALL_CMD, help="Path to guest-tools/uninstall.cmd")
    parser.add_argument("--verify-ps1", type=Path, default=DEFAULT_VERIFY_PS1, help="Path to guest-tools/verify.ps1")
    args = parser.parse_args(argv)

    try:
        errors = lint_files(setup_cmd=args.setup_cmd, uninstall_cmd=args.uninstall_cmd, verify_ps1=args.verify_ps1)
    except LintError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2

    if errors:
        for msg in errors:
            print(f"ERROR: {msg}", file=sys.stderr)
        print(f"Guest Tools script lint failed: {len(errors)} invariant(s) missing.", file=sys.stderr)
        return 1

    print("Guest Tools script lint OK.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
