#!/usr/bin/env python3
"""
Fail CI if any workspace packages would map to the same Rust crate identifier.

Cargo package names may contain `-`, but Rust crate idents use `_` instead. This
means two different packages like:

  - aero-jit
  - aero_jit

both become `aero_jit` in code, forcing dependency renames and creating long-term
maintenance hazards.

This script uses `cargo metadata` (workspace truth) and checks for collisions of:

  normalized_ident = package_name.replace("-", "_")
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from typing import Dict, List


@dataclass(frozen=True)
class Pkg:
    name: str
    normalized: str
    path: str


def cargo_metadata() -> dict:
    try:
        proc = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError:
        print("error: `cargo` not found (required for cargo metadata)", file=sys.stderr)
        sys.exit(2)
    except subprocess.CalledProcessError as e:
        print("error: `cargo metadata` failed:", file=sys.stderr)
        if e.stderr:
            print(e.stderr, file=sys.stderr)
        sys.exit(e.returncode or 1)

    return json.loads(proc.stdout)


def main() -> int:
    meta = cargo_metadata()
    workspace_root = meta.get("workspace_root")
    if not workspace_root:
        print("error: cargo metadata missing `workspace_root`", file=sys.stderr)
        return 2

    pkgs: List[Pkg] = []
    for pkg in meta.get("packages", []):
        name = pkg.get("name")
        manifest = pkg.get("manifest_path")
        if not name or not manifest:
            continue

        pkg_dir = os.path.dirname(manifest)
        rel_path = os.path.relpath(pkg_dir, workspace_root)

        pkgs.append(Pkg(name=name, normalized=name.replace("-", "_"), path=rel_path))

    pkgs.sort(key=lambda p: (p.normalized, p.name, p.path))

    groups: Dict[str, List[Pkg]] = {}
    for p in pkgs:
        groups.setdefault(p.normalized, []).append(p)

    collisions = {k: v for k, v in groups.items() if len(v) > 1}

    if not collisions:
        print(f"crate-name collision check: OK ({len(pkgs)} workspace packages)")
        return 0

    print("error: Rust crate-name collisions detected in the workspace.\n", file=sys.stderr)
    for norm in sorted(collisions.keys()):
        print(f"- normalized ident: {norm}", file=sys.stderr)
        for p in collisions[norm]:
            print(f"    - package: {p.name:25s} path: {p.path}", file=sys.stderr)
        print("", file=sys.stderr)

    print(
        "hint: rename one of the packages so their names do not normalize to the same ident "
        "(`-` → `_`).",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

