#!/usr/bin/env python3
"""Fast, offline sanity check for the repo's `.cargo/config.toml`.

If `.cargo/config.toml` becomes invalid TOML (e.g. merge conflict markers or
duplicate keys), *all* Cargo commands fail before compilation starts.

CI will eventually catch this when it runs `cargo ...`, but the failure can be
confusing and may happen later than necessary. This script is a lightweight
pre-flight check intended to run early in CI so we fail fast with a clear error
message.
"""

from __future__ import annotations

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


def repo_root() -> pathlib.Path:
    try:
        out = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True
        ).strip()
    except (FileNotFoundError, subprocess.CalledProcessError):
        out = ""
    return pathlib.Path(out) if out else pathlib.Path.cwd()


def check_config(path: pathlib.Path) -> None:
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
            raise ValueError(f"{path}:{lineno}:{colno}: {msg}") from None
        raise ValueError(f"{path}: invalid TOML: {exc}") from None


def main(argv: list[str]) -> int:
    if argv:
        print("error: this script does not accept any arguments", file=sys.stderr)
        return 2

    config_path = repo_root() / ".cargo" / "config.toml"
    try:
        check_config(config_path)
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

