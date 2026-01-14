#!/usr/bin/env python3
"""Fast, offline sanity checks for Cargo.toml files.

If any `Cargo.toml` becomes invalid TOML (e.g. merge conflict markers or duplicate
keys), *all* Cargo commands fail before compilation starts.

CI will eventually catch this when it runs `cargo ...`, but the failure can be
confusing and may happen later than necessary. This script is a lightweight
pre-flight check intended to run early in CI so we fail fast with a clear error
message.
"""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys


CONFLICT_MARKERS = ("<<<<<<<", "=======", ">>>>>>>")

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover - fallback for older Pythons
    try:
        import tomli as tomllib  # type: ignore[import-not-found]
    except ModuleNotFoundError:
        tomllib = None  # type: ignore[assignment]


def git_tracked_cargo_tomls() -> list[str]:
    try:
        out = subprocess.check_output(["git", "ls-files"], text=True)
    except (subprocess.CalledProcessError, FileNotFoundError) as exc:
        raise RuntimeError("failed to list tracked files (is this a git checkout?)") from exc

    tomls = [line for line in out.splitlines() if line.endswith("Cargo.toml")]
    tomls.sort()
    return tomls


def check_toml(path: pathlib.Path) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise RuntimeError(f"failed to read {path}") from exc

    for marker in CONFLICT_MARKERS:
        if marker in text:
            raise ValueError(f"{path}: contains merge conflict marker '{marker}'")

    if tomllib is None:
        raise RuntimeError(
            "python stdlib tomllib (or tomli backport) is required to parse TOML"
        )

    try:
        tomllib.loads(text)
    except Exception as exc:
        lineno = getattr(exc, "lineno", None)
        colno = getattr(exc, "colno", None)
        msg = getattr(exc, "msg", None)
        if lineno is not None and colno is not None and msg is not None:
            # `tomllib` errors are often terse (e.g. duplicate keys just say
            # "Cannot overwrite a value"). Include the offending line to make
            # CI failures actionable without opening the file locally.
            line = ""
            try:
                lines = text.splitlines()
                if 1 <= lineno <= len(lines):
                    line = lines[lineno - 1]
            except Exception:
                # Best-effort: keep the original error formatting if anything
                # about the snippet extraction goes wrong.
                line = ""

            if line:
                prefix = "  "
                caret = prefix + (" " * (max(colno, 1) - 1)) + "^"
                raise ValueError(
                    f"{path}:{lineno}:{colno}: {msg}\n{prefix}{line}\n{caret}"
                ) from None

            raise ValueError(f"{path}:{lineno}:{colno}: {msg}") from None
        raise ValueError(f"{path}: invalid TOML: {exc}") from None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "tomls",
        nargs="*",
        help="Cargo.toml paths to validate (defaults to all tracked Cargo.toml files)",
    )
    args = parser.parse_args(argv)

    tomls = args.tomls or git_tracked_cargo_tomls()
    if not tomls:
        print("error: no Cargo.toml files found", file=sys.stderr)
        return 2

    had_error = False
    for rel in tomls:
        path = pathlib.Path(rel)
        try:
            check_toml(path)
        except Exception as exc:
            print(f"error: {exc}", file=sys.stderr)
            had_error = True

    return 1 if had_error else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
