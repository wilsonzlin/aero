#!/usr/bin/env python3
"""
Fail CI if the workspace violates Rust/Cargo crate naming policy.

Cargo package names may contain `-`, but Rust crate idents use `_` instead. This
means two different packages like:

  - aero-jit
  - aero_jit

both become `aero_jit` in code, forcing dependency renames and creating long-term
maintenance hazards.

This script uses `cargo metadata` (workspace truth) and checks for collisions of:

  normalized_ident = package_name.replace("-", "_")

Additionally, it enforces the workspace naming convention from ADR 0007:

  - workspace packages must use kebab-case (no `_`) in `[package].name`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import PurePath
from typing import Dict, List


@dataclass(frozen=True)
class Pkg:
    name: str
    normalized: str
    path: str
    lib_crate: str | None


def cargo_metadata() -> dict:
    try:
        proc = subprocess.run(
            # Use `--locked` to enforce the repository-wide Cargo.lock policy (ADR 0012)
            # and to ensure CI doesn't silently re-resolve dependencies if a lockfile is stale.
            ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
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

        lib_crate = None
        for target in pkg.get("targets", []):
            kinds = target.get("kind", [])
            if any(
                k in {"lib", "rlib", "cdylib", "staticlib", "proc-macro"} for k in kinds
            ):
                lib_crate = target.get("name")
                break

        pkg_dir = os.path.dirname(manifest)
        rel_path = os.path.relpath(pkg_dir, workspace_root)

        pkgs.append(
            Pkg(
                name=name,
                normalized=name.replace("-", "_"),
                path=rel_path,
                lib_crate=lib_crate,
            )
        )

    pkgs.sort(key=lambda p: (p.normalized, p.name, p.path))

    groups: Dict[str, List[Pkg]] = {}
    for p in pkgs:
        groups.setdefault(p.normalized, []).append(p)

    normalized_collisions = {k: v for k, v in groups.items() if len(v) > 1}
    underscored = [p for p in pkgs if "_" in p.name]
    underscored_crates_dirs = []
    for p in pkgs:
        parts = PurePath(p.path).parts
        if len(parts) >= 2 and parts[0] == "crates" and "_" in parts[1]:
            underscored_crates_dirs.append(p)

    lib_name_mismatches = [p for p in pkgs if p.lib_crate and p.lib_crate != p.normalized]

    lib_groups: Dict[str, List[Pkg]] = {}
    for p in pkgs:
        if p.lib_crate:
            lib_groups.setdefault(p.lib_crate, []).append(p)
    lib_collisions = {k: v for k, v in lib_groups.items() if len(v) > 1}

    if (
        not normalized_collisions
        and not underscored
        and not underscored_crates_dirs
        and not lib_name_mismatches
        and not lib_collisions
    ):
        print(f"crate-name policy check: OK ({len(pkgs)} workspace packages)")
        return 0

    print("error: workspace crate naming policy violations detected.\n", file=sys.stderr)

    if underscored:
        print("underscore package names detected (use kebab-case):", file=sys.stderr)
        for p in sorted(underscored, key=lambda p: (p.name, p.path)):
            print(f"  - package: {p.name:25s} path: {p.path}", file=sys.stderr)
        print("", file=sys.stderr)
        print(
            "hint: rename the package(s) to kebab-case (example: `qemu_diff` → `qemu-diff`).\n",
            file=sys.stderr,
        )

    if underscored_crates_dirs:
        print("crate directories under `crates/` must be kebab-case (no underscores):", file=sys.stderr)
        for p in sorted(underscored_crates_dirs, key=lambda p: (p.path, p.name)):
            print(f"  - dir: {p.path:30s} package: {p.name}", file=sys.stderr)
        print("", file=sys.stderr)

    if lib_name_mismatches:
        print("custom library crate names detected (lib target name must match package name):", file=sys.stderr)
        for p in sorted(lib_name_mismatches, key=lambda p: (p.path, p.name)):
            print(
                f"  - package: {p.name:25s} lib: {p.lib_crate:25s} path: {p.path}",
                file=sys.stderr,
            )
        print("", file=sys.stderr)

    if lib_collisions:
        print("Rust crate-name collisions detected between workspace library targets:\n", file=sys.stderr)
        for crate in sorted(lib_collisions.keys()):
            print(f"- crate ident: {crate}", file=sys.stderr)
            for p in lib_collisions[crate]:
                print(f"    - package: {p.name:25s} path: {p.path}", file=sys.stderr)
            print("", file=sys.stderr)

    if normalized_collisions:
        print("Rust crate-name collisions detected after `-` → `_` normalization:\n", file=sys.stderr)
        for norm in sorted(normalized_collisions.keys()):
            print(f"- normalized ident: {norm}", file=sys.stderr)
            for p in normalized_collisions[norm]:
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
