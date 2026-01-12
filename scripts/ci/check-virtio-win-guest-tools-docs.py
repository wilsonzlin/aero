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
    m = re.search(
        r"\[\s*string\s*\]\s*\$Profile\s*=\s*['\"]([^'\"]+)['\"]",
        ps1_text,
        flags=re.IGNORECASE,
    )
    if not m:
        errors.append(f"Could not find default Profile parameter in {ps1_path}")
    else:
        default_profile = m.group(1).strip().lower()
        if default_profile != "full":
            errors.append(
                f"Expected {ps1_path} default Profile to be 'full', got '{default_profile}'"
            )

    sh_text = sh_path.read_text(encoding="utf-8")
    m = re.search(r"(?m)^\s*profile=['\"]([^'\"]+)['\"]\s*(?:#.*)?$", sh_text)
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
        # Workstream onboarding docs sometimes mention virtio-win-derived packaging flows.
        # Keep them aligned with wrapper defaults too.
        REPO_ROOT / "instructions/windows-drivers.md",
    ]

    patterns: list[tuple[str, re.Pattern[str]]] = [
        (
            "claims `-Profile minimal` is default",
            re.compile(r"(?i)`?-Profile\s+minimal`?\s*\(default\)"),
        ),
        (
            "claims `minimal` is default",
            re.compile(r"(?i)`?minimal`?\s*\(default\)"),
        ),
        (
            "claims Default(-Profile minimal)",
            re.compile(r"(?i)\bDefault\s*\(\s*`?-Profile\s+minimal`?\s*\)"),
        ),
        (
            "claims defaults to -Profile minimal",
            re.compile(r"(?i)\bdefault(?:s)?\s+to\s+(?:the\s+)?`?-Profile\s+minimal`?"),
        ),
        (
            "claims defaults to minimal profile",
            re.compile(r"(?i)\bdefault(?:s)?\s+to\s+(?:the\s+)?`?minimal`?\s+profile\b"),
        ),
        (
            "claims default profile is minimal",
            re.compile(r"(?i)\bdefault\s+profile\s*(?:is|:|=)\s*`?minimal`?\b"),
        ),
        (
            "claims by-default uses -Profile minimal",
            # Allow line-wrapping but avoid crossing sentence boundaries (so we don't flag
            # "By default ... -Profile full. ... use -Profile minimal").
            # Also avoid crossing semicolons because docs sometimes write:
            # "By default ... -Profile full; use -Profile minimal ..." (which should not fail CI).
            # Additionally, avoid flagging sentences that mention `-Profile full` before mentioning
            # `-Profile minimal` as an alternative.
            re.compile(
                r"(?i)\bBy default\b(?:(?!`?-Profile\s+full`?)[^.;]){0,200}`?-Profile\s+minimal`?"
            ),
        ),
        (
            "claims -Profile minimal is used by default",
            re.compile(r"(?i)`?-Profile\s+minimal`?[^.;]{0,200}\bby default\b"),
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
            "claims defaults to --profile minimal",
            re.compile(r"(?i)\bdefault(?:s)?\s+to\s+(?:the\s+)?`?--profile\s+minimal`?"),
        ),
        (
            "claims by-default uses --profile minimal",
            re.compile(
                r"(?i)\bBy default\b(?:(?!`?--profile\s+full`?)[^.;]){0,200}`?--profile\s+minimal`?"
            ),
        ),
        (
            "claims --profile minimal is used by default",
            re.compile(r"(?i)`?--profile\s+minimal`?[^.;]{0,200}\bby default\b"),
        ),
    ]

    for path in doc_paths:
        errors += check_file_exists(path)
    if errors:
        return errors

    for path in doc_paths:
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        matched_ranges: list[tuple[int, int]] = []
        for label, pat in patterns:
            for m in pat.finditer(text):
                # Avoid spamming multiple errors for the same underlying match (e.g. the generic
                # `minimal (default)` pattern is a substring of `-Profile minimal (default)`).
                if any(start <= m.start() and m.end() <= end for start, end in matched_ranges):
                    continue
                matched_ranges.append((m.start(), m.end()))
                lineno = text.count("\n", 0, m.start()) + 1
                # Show a compact excerpt since matches may span multiple lines.
                excerpt = re.sub(r"\s+", " ", m.group(0)).strip()
                # If the excerpt is very long, also include the line containing the match start for context.
                if len(excerpt) > 200 and 1 <= lineno <= len(lines):
                    excerpt = f"{excerpt[:200]}… (line: {lines[lineno - 1].strip()})"
                errors.append(f"{path}:{lineno}: {label}: {excerpt}")

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
