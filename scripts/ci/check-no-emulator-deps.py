#!/usr/bin/env python3
"""
Fail CI if a workspace crate depends on `crates/emulator` as a *normal* dependency.

Context
-------

After ADR 0008 and ADR 0014, the repo has exactly one canonical VM wiring layer:

  - `crates/aero-machine` (`aero_machine::Machine`)

`crates/emulator` is a legacy/compat device stack and should not be treated as the
default place to "wire up a VM" for new code. In practice, the easiest accidental
regression is adding:

  [dependencies]
  emulator = { path = "../emulator" }

to another crate, which then silently makes `crates/emulator` part of the
"canonical" dependency graph.

Policy
------

- `emulator` MUST NOT appear in any crate's `[dependencies]` or `[build-dependencies]`
  (including target-specific dependency tables).
- `emulator` MAY appear in `dev-dependencies` for transitional tests/benchmarks.

See:
- `docs/adr/0008-canonical-vm-core.md`
- `docs/adr/0014-canonical-machine-stack.md`
- `docs/21-emulator-crate-migration.md`
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover - fallback for older Pythons
    try:
        import tomli as tomllib  # type: ignore[import-not-found]
    except ModuleNotFoundError:
        tomllib = None  # type: ignore[assignment]


def repo_root() -> Path:
    try:
        out = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
    except (FileNotFoundError, subprocess.CalledProcessError):
        out = ""
    return Path(out) if out else Path.cwd()


def workspace_cargo_tomls(root: Path) -> list[Path]:
    """
    Return the Cargo.toml files for the root package + all workspace members.

    We intentionally do *not* scan every tracked Cargo.toml in the repo. Some
    non-workspace crates (e.g. `fuzz/`) are excluded from normal CI builds and
    may carry cargo-fuzz-specific structure; this guardrail is about preventing
    the legacy `emulator` crate from becoming a dependency in the *canonical*
    workspace build graph.
    """

    root_toml_path = root / "Cargo.toml"
    try:
        root_doc = tomllib.loads(root_toml_path.read_text(encoding="utf-8"))
    except Exception as exc:  # pragma: no cover
        raise RuntimeError(f"failed to parse root Cargo.toml: {exc}") from exc

    members = root_doc.get("workspace", {}).get("members", [])
    if not isinstance(members, list):  # pragma: no cover
        raise RuntimeError("root Cargo.toml: workspace.members must be an array")

    out: list[Path] = [root_toml_path]
    for member in members:
        if not isinstance(member, str):
            continue
        out.append(root / member / "Cargo.toml")
    return out


def workspace_emulator_aliases(root_doc: dict, *, path: str) -> set[str]:
    """
    Return the set of `[workspace.dependencies]` keys that resolve to the `emulator` package.

    Why this matters
    ----------------

    Cargo lets crates inherit dependency specs from the workspace root:

      [workspace.dependencies]
      emulator_compat = { package = "emulator", path = "crates/emulator" }

      # crates/some-crate/Cargo.toml
      [dependencies]
      emulator_compat = { workspace = true }

    Without resolving `workspace = true`, a policy check that only looks for a
    literal `package = "emulator"` in the crate-local dependency table would miss
    this (and accidentally allow the legacy `crates/emulator` stack to creep back
    into the canonical dependency graph).
    """

    deps = root_doc.get("workspace", {}).get("dependencies", {})
    if deps is None:
        deps = {}
    if not isinstance(deps, dict):  # pragma: no cover
        raise RuntimeError(f"{path}: workspace.dependencies must be a table")

    aliases: set[str] = set()
    for dep_name, dep_spec in deps.items():
        if not isinstance(dep_name, str):
            continue

        # Simple case: `emulator = { ... }`
        if dep_name == "emulator":
            aliases.add(dep_name)
            continue

        pkg = None
        if isinstance(dep_spec, dict):
            pkg = dep_spec.get("package")
        # Cargo defaults the package name to the dependency key.
        if pkg is None:
            pkg = dep_name

        if pkg == "emulator":
            aliases.add(dep_name)

    return aliases


def dep_table_violations(doc: dict, *, path: str, workspace_emulator_deps: set[str]) -> list[str]:
    """Return human-readable violations for a parsed Cargo.toml."""

    def check_table(table: dict, label: str) -> list[str]:
        if not isinstance(table, dict):
            return []
        violations: list[str] = []

        for dep_name, dep_spec in table.items():
            # Simple case: `emulator = ...`
            if dep_name == "emulator":
                violations.append(f"{path}: {label} contains forbidden dependency key 'emulator'")
                continue

            # Renamed dependency case:
            #   emulator_compat = { package = "emulator", path = "../emulator" }
            #
            # Cargo treats `package = ...` as the actual crate name; this still introduces a
            # dependency edge to `crates/emulator` and must be blocked for canonical crates.
            if isinstance(dep_spec, dict):
                pkg = dep_spec.get("package")
                if pkg == "emulator":
                    violations.append(
                        f"{path}: {label} dependency '{dep_name}' renames forbidden package 'emulator'"
                    )
                    continue

                # Workspace-inherited dependency case:
                #   emulator_compat = { workspace = true }
                # with `[workspace.dependencies.emulator_compat] package = "emulator" ...`
                if dep_spec.get("workspace") is True and dep_name in workspace_emulator_deps:
                    violations.append(
                        f"{path}: {label} dependency '{dep_name}' uses workspace dependency that resolves to forbidden package 'emulator'"
                    )

        return violations

    violations: list[str] = []

    violations.extend(check_table(doc.get("dependencies", {}), "[dependencies]"))
    violations.extend(check_table(doc.get("build-dependencies", {}), "[build-dependencies]"))

    target = doc.get("target")
    if isinstance(target, dict):
        for target_key, target_tables in target.items():
            if not isinstance(target_tables, dict):
                continue
            violations.extend(
                check_table(
                    target_tables.get("dependencies", {}),
                    f"[target.{target_key}.dependencies]",
                )
            )
            violations.extend(
                check_table(
                    target_tables.get("build-dependencies", {}),
                    f"[target.{target_key}.build-dependencies]",
                )
            )

    return violations


def main() -> int:
    if tomllib is None:
        print(
            "error: python stdlib tomllib (or tomli backport) is required to parse TOML",
            file=sys.stderr,
        )
        return 2

    root = repo_root()
    root_toml_path = root / "Cargo.toml"
    try:
        root_doc = tomllib.loads(root_toml_path.read_text(encoding="utf-8"))
    except Exception as exc:  # pragma: no cover
        print(f"error: failed to parse root Cargo.toml: {exc}", file=sys.stderr)
        return 2

    workspace_emulator_deps = workspace_emulator_aliases(root_doc, path="Cargo.toml")
    violations: list[str] = []

    for cargo_toml in workspace_cargo_tomls(root):
        rel = cargo_toml.relative_to(root).as_posix()

        # The emulator crate itself obviously declares its own package name.
        if rel == "crates/emulator/Cargo.toml":
            continue

        try:
            text = cargo_toml.read_text(encoding="utf-8")
        except OSError as exc:
            violations.append(f"{rel}: failed to read: {exc}")
            continue

        try:
            doc = tomllib.loads(text)
        except Exception as exc:  # pragma: no cover
            violations.append(f"{rel}: invalid TOML: {exc}")
            continue

        violations.extend(
            dep_table_violations(doc, path=rel, workspace_emulator_deps=workspace_emulator_deps)
        )

    if violations:
        sys.stderr.write(
            "Found forbidden `emulator` crate dependencies. `crates/emulator` is legacy/compat; "
            "canonical VM wiring is `crates/aero-machine`.\n\n"
        )
        sys.stderr.write("\n".join(violations))
        sys.stderr.write("\n")
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
