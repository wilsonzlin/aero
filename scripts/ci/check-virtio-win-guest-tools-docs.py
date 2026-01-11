#!/usr/bin/env python3
from __future__ import annotations

import pathlib
import re
import sys


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]


def check_file_exists(path: pathlib.Path) -> list[str]:
    if not path.exists():
        return [f"Expected file to exist: {path}"]
    return []


def check_wrapper_defaults() -> list[str]:
    errors: list[str] = []

    ps1_path = REPO_ROOT / "drivers/scripts/make-guest-tools-from-virtio-win.ps1"
    sh_path = REPO_ROOT / "drivers/scripts/make-guest-tools-from-virtio-win.sh"

    errors += check_file_exists(ps1_path)
    errors += check_file_exists(sh_path)
    if errors:
        return errors

    ps1_text = ps1_path.read_text(encoding="utf-8")
    m = re.search(r'\[string\]\$Profile\s*=\s*"([^"]+)"', ps1_text)
    if not m:
        errors.append(f"Could not find default Profile parameter in {ps1_path}")
    else:
        default_profile = m.group(1).strip().lower()
        if default_profile != "full":
            errors.append(
                f"Expected {ps1_path} default Profile to be 'full', got '{default_profile}'"
            )

    sh_text = sh_path.read_text(encoding="utf-8")
    m = re.search(r'(?m)^\s*profile="([^"]+)"\s*$', sh_text)
    if not m:
        errors.append(f"Could not find default profile=... assignment in {sh_path}")
    else:
        default_profile = m.group(1).strip().lower()
        if default_profile != "full":
            errors.append(
                f"Expected {sh_path} default profile to be 'full', got '{default_profile}'"
            )

    return errors


def check_docs() -> list[str]:
    errors: list[str] = []

    # These docs historically drifted, repeatedly claiming `-Profile minimal` was the default even
    # though the wrapper scripts default to `full`. Keep a cheap CI guardrail so drift is caught on PRs.
    doc_paths = [
        REPO_ROOT / "docs/16-guest-tools-packaging.md",
        REPO_ROOT / "docs/virtio-windows-drivers.md",
        REPO_ROOT / "tools/packaging/README.md",
        REPO_ROOT / "tools/packaging/specs/README.md",
        REPO_ROOT / "drivers/README.md",
    ]

    patterns: list[tuple[str, re.Pattern[str]]] = [
        (
            "claims `-Profile minimal` is default",
            re.compile(r"(?i)`?-Profile\s+minimal`?\s*\(default\)"),
        ),
        (
            "claims Default(-Profile minimal)",
            re.compile(r"(?i)\bDefault\s*\(\s*`?-Profile\s+minimal`?\s*\)"),
        ),
        (
            "claims by-default uses -Profile minimal",
            re.compile(r"(?i)\bBy default\b[^\n]{0,200}`?-Profile\s+minimal`?"),
        ),
        (
            "claims `--profile minimal` is default",
            re.compile(r"(?i)`?--profile\s+minimal`?\s*\(default\)"),
        ),
        (
            "claims Default(--profile minimal)",
            re.compile(r"(?i)\bDefault\s*\(\s*`?--profile\s+minimal`?\s*\)"),
        ),
        (
            "claims by-default uses --profile minimal",
            re.compile(r"(?i)\bBy default\b[^\n]{0,200}`?--profile\s+minimal`?"),
        ),
    ]

    for path in doc_paths:
        errors += check_file_exists(path)
    if errors:
        return errors

    for path in doc_paths:
        text = path.read_text(encoding="utf-8")
        for lineno, line in enumerate(text.splitlines(), start=1):
            for label, pat in patterns:
                if pat.search(line):
                    errors.append(f"{path}:{lineno}: {label}: {line.strip()}")

    return errors


def main() -> int:
    errors: list[str] = []
    errors += check_wrapper_defaults()
    errors += check_docs()

    if errors:
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

