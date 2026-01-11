#!/usr/bin/env python3
"""Fast, offline sanity checks for Cargo.lock files.

CI already validates lockfile *drift* using `cargo metadata --locked` (see
`scripts/ci/check-cargo-lockfiles.sh`), but when a lockfile is corrupted (e.g.
merge conflict markers or duplicate package entries) Cargo errors out before it
can perform that drift check.

This script is a lightweight pre-flight check that runs without invoking Cargo
or requiring the crates.io index:
- fails if a lockfile contains merge conflict markers
- fails if a lockfile contains duplicate (name, version, source) package entries

It is intended to run early in CI to fail fast with a clear error message.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from typing import Iterable


CONFLICT_MARKERS = ("<<<<<<<", "=======", ">>>>>>>")


@dataclass(frozen=True)
class PackageKey:
    name: str
    version: str
    source: str | None


def git_tracked_lockfiles() -> list[str]:
    try:
        out = subprocess.check_output(["git", "ls-files"], text=True)
    except (subprocess.CalledProcessError, FileNotFoundError) as exc:
        raise RuntimeError("failed to list tracked files (is this a git checkout?)") from exc

    lockfiles = [line for line in out.splitlines() if line.endswith("Cargo.lock")]
    lockfiles.sort()
    return lockfiles


def iter_package_keys(lock_text: str) -> Iterable[PackageKey]:
    cur_name: str | None = None
    cur_version: str | None = None
    cur_source: str | None = None
    in_pkg = False

    def flush() -> PackageKey | None:
        nonlocal cur_name, cur_version, cur_source, in_pkg
        if not in_pkg:
            return None
        if cur_name is None or cur_version is None:
            raise ValueError("malformed [[package]] entry (missing name/version)")
        key = PackageKey(cur_name, cur_version, cur_source)
        cur_name = None
        cur_version = None
        cur_source = None
        in_pkg = False
        return key

    name_re = re.compile(r'^name\s*=\s*"([^"]+)"\s*$')
    version_re = re.compile(r'^version\s*=\s*"([^"]+)"\s*$')
    source_re = re.compile(r'^source\s*=\s*"([^"]+)"\s*$')

    for line in lock_text.splitlines():
        s = line.strip()
        if s == "[[package]]":
            prev = flush()
            if prev is not None:
                yield prev
            in_pkg = True
            continue

        if not in_pkg:
            continue

        m = name_re.match(s)
        if m:
            cur_name = m.group(1)
            continue

        m = version_re.match(s)
        if m:
            cur_version = m.group(1)
            continue

        m = source_re.match(s)
        if m:
            cur_source = m.group(1)
            continue

    last = flush()
    if last is not None:
        yield last


def check_lockfile(path: pathlib.Path) -> list[tuple[PackageKey, int]]:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise RuntimeError(f"failed to read {path}") from exc

    for marker in CONFLICT_MARKERS:
        if marker in text:
            raise ValueError(f"{path}: contains merge conflict marker '{marker}'")

    keys = list(iter_package_keys(text))
    counts = Counter(keys)
    dups = [(k, n) for k, n in counts.items() if n > 1]
    dups.sort(key=lambda item: (-item[1], item[0].name, item[0].version, item[0].source or ""))
    return dups


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "lockfiles",
        nargs="*",
        help="Cargo.lock paths to validate (defaults to all tracked Cargo.lock files)",
    )
    args = parser.parse_args(argv)

    lockfiles = args.lockfiles or git_tracked_lockfiles()
    if not lockfiles:
        print("error: no Cargo.lock files found", file=sys.stderr)
        return 2

    had_error = False
    for rel in lockfiles:
        path = pathlib.Path(rel)
        try:
            dups = check_lockfile(path)
        except Exception as exc:
            print(f"error: {exc}", file=sys.stderr)
            had_error = True
            continue

        if dups:
            had_error = True
            print(f"error: {path}: duplicate [[package]] entries detected", file=sys.stderr)
            for key, n in dups[:20]:
                src = key.source or "<none>"
                print(f"  {n}x {key.name} {key.version} {src}", file=sys.stderr)
            if len(dups) > 20:
                print(f"  ... ({len(dups) - 20} more)", file=sys.stderr)

    return 1 if had_error else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

