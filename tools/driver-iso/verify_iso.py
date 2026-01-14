#!/usr/bin/env python3

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def _load_manifest(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"manifest not found: {path}")
    except json.JSONDecodeError as e:
        raise SystemExit(f"failed to parse manifest JSON ({path}): {e}")


def _normalize_iso_path(path: str) -> str:
    """
    Normalize ISO paths for comparison.

    - ensure forward slashes
    - strip ISO9660 version suffixes like `;1`
    - compare case-insensitively by lowercasing
    """

    p = path.strip()
    if not p:
        return p
    p = p.replace("\\", "/")
    if not p.startswith("/"):
        p = "/" + p
    if p.endswith(";1"):
        p = p[:-2]
    return p.lower()


def _list_iso_files_with_pycdlib(iso_path: Path) -> set[str]:
    """
    List ISO file paths using pycdlib (pure-Python).

    Prefer Joliet paths; fall back to Rock Ridge; finally ISO9660 paths.
    """

    try:
        import pycdlib  # type: ignore
    except ModuleNotFoundError as e:
        raise SystemExit(
            "pycdlib is not installed; install it to verify ISO contents without external tools:\n"
            "  python3 -m pip install pycdlib"
        ) from e

    iso = pycdlib.PyCdlib()
    try:
        iso.open(str(iso_path))
    except Exception as e:
        raise SystemExit(f"pycdlib failed to open ISO: {e}") from e
    try:
        last_err = None
        for mode in ("joliet", "rr", "iso"):
            try:
                files: set[str] = set()
                walk_kwargs: dict[str, str] = {f"{mode}_path": "/"}
                for root, _dirs, filelist in iso.walk(**walk_kwargs):
                    for f in filelist:
                        p = f"{root.rstrip('/')}/{f}"
                        files.add(_normalize_iso_path(p))
                return files
            except Exception as e:
                last_err = e
                continue

        raise SystemExit(f"pycdlib failed to walk the ISO filesystem: {last_err}")
    finally:
        iso.close()


def _list_iso_files_with_aero_iso_ls(iso_path: Path) -> set[str]:
    """
    List ISO file paths using the in-tree Rust ISO9660/Joliet parser.

    This backend is useful when Rust/cargo is available but Python ISO tooling
    (pycdlib) or external tools (xorriso) are not installed.
    """

    cargo = shutil.which("cargo")
    if not cargo:
        raise SystemExit("cargo not found; cannot list ISO contents via aero_iso_ls")

    repo_root = Path(__file__).resolve().parents[2]
    cargo_toml = repo_root / "tools/packaging/aero_packager/Cargo.toml"
    if not cargo_toml.is_file():
        raise SystemExit(f"aero_packager Cargo.toml not found: {cargo_toml}")

    proc = subprocess.run(
        [
            cargo,
            "run",
            "--quiet",
            "--locked",
            "--manifest-path",
            str(cargo_toml),
            "--bin",
            "aero_iso_ls",
            "--",
            "--iso",
            str(iso_path),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "aero_iso_ls failed while listing ISO files:\n"
            f"{proc.stderr.strip() or proc.stdout.strip() or '<no output>'}"
        )

    return {_normalize_iso_path(line) for line in proc.stdout.splitlines() if line.strip()}


def _list_iso_files_with_xorriso(iso_path: Path) -> set[str]:
    xorriso = shutil.which("xorriso")
    if not xorriso:
        raise SystemExit(
            "xorriso not found; install one of:\n"
            "- pycdlib: python3 -m pip install pycdlib\n"
            "- xorriso (via your OS package manager)"
        )

    proc = subprocess.run(
        [xorriso, "-indev", str(iso_path), "-find", "/", "-type", "f", "-print"],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "xorriso failed while listing ISO files:\n"
            f"{proc.stderr.strip() or proc.stdout.strip() or '<no output>'}"
        )
    return {_normalize_iso_path(line) for line in proc.stdout.splitlines() if line.strip()}


def _list_iso_files_with_powershell_mount(iso_path: Path) -> set[str]:
    powershell = shutil.which("powershell")
    if not powershell:
        raise SystemExit(
            "powershell.exe not found; cannot verify ISO contents.\n"
            "Install one of:\n"
            "- pycdlib: python3 -m pip install pycdlib\n"
            "- xorriso (if available for your platform)"
        )

    script = r"""& {
  param([string]$IsoPath)
  $ErrorActionPreference = 'Stop'
  $img = $null
  try {
    $img = Mount-DiskImage -ImagePath $IsoPath -PassThru
    $vol = $null
    for ($i = 0; $i -lt 20; $i++) {
      $vol = $img | Get-Volume -ErrorAction SilentlyContinue
      if ($vol -and $vol.DriveLetter) { break }
      Start-Sleep -Milliseconds 200
    }
    if (-not $vol -or -not $vol.DriveLetter) {
      throw "Mounted ISO volume has no drive letter."
    }
    $root = "$($vol.DriveLetter):\"
    Get-ChildItem -LiteralPath $root -Recurse -File | ForEach-Object {
      $rel = $_.FullName.Substring($root.Length) -replace '\\','/'
      '/' + $rel
    }
  } finally {
    if ($img) {
      Dismount-DiskImage -ImagePath $IsoPath | Out-Null
    }
  }
}"""

    proc = subprocess.run(
        [
            powershell,
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
            str(iso_path),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "PowerShell Mount-DiskImage failed while listing ISO files:\n"
            f"{proc.stderr.strip() or proc.stdout.strip() or '<no output>'}"
        )
    return {_normalize_iso_path(line) for line in proc.stdout.splitlines() if line.strip()}


def _list_iso_files(iso_path: Path) -> set[str]:
    errors: list[str] = []

    # Prefer the in-tree Rust ISO parser when cargo is available (cross-platform).
    #
    # This keeps verification working even in environments where:
    # - Python packaging is restricted (can't `pip install pycdlib`)
    # - external tooling like `xorriso` isn't installed
    if shutil.which("cargo"):
        try:
            files = _list_iso_files_with_aero_iso_ls(iso_path)
            print("Using ISO listing backend: rust (aero_iso_ls)")
            return files
        except SystemExit as e:
            errors.append(str(e))

    # Prefer a pure-Python implementation when available so this script works on
    # Linux/macOS without requiring external ISO tooling.
    try:
        import pycdlib  # type: ignore  # noqa: F401

        files = _list_iso_files_with_pycdlib(iso_path)
        print("Using ISO listing backend: pycdlib")
        return files
    except ModuleNotFoundError:
        pass
    except SystemExit as e:
        errors.append(str(e))

    # Existing behavior: xorriso on non-Windows hosts (if present).
    xorriso = shutil.which("xorriso")
    if xorriso:
        try:
            files = _list_iso_files_with_xorriso(iso_path)
            print("Using ISO listing backend: xorriso")
            return files
        except SystemExit as e:
            errors.append(str(e))

    # Existing behavior: Mount-DiskImage on Windows hosts.
    if os.name == "nt":
        try:
            files = _list_iso_files_with_powershell_mount(iso_path)
            print("Using ISO listing backend: powershell (Mount-DiskImage)")
            return files
        except SystemExit as e:
            errors.append(str(e))

    install_hint = (
        "Install one of:\n"
        "- Rust/cargo (preferred; uses the in-tree aero_iso_ls backend)\n"
        "- pycdlib: python3 -m pip install pycdlib\n"
        "- xorriso (via your OS package manager)"
    )
    if errors:
        formatted = "\n\n".join(errors)
        raise SystemExit(f"no supported ISO listing backend succeeded:\n\n{formatted}\n\n{install_hint}")

    raise SystemExit(f"no supported ISO listing backend found.\n{install_hint}")


def _arches_for_require_arch(require_arch: str) -> tuple[str, ...]:
    if require_arch == "both":
        return ("x86", "amd64")
    return (require_arch,)


def _validate_manifest(manifest: dict) -> list[dict]:
    """
    Validate the structure of `drivers/virtio/manifest.json`.

    Returns the validated `packages` list.
    """

    if manifest.get("schema_version") != 1:
        raise SystemExit(
            "unsupported manifest schema_version "
            f"(expected 1, got {manifest.get('schema_version')!r})"
        )

    packages = manifest.get("packages")
    if not isinstance(packages, list):
        raise SystemExit(f"invalid manifest: 'packages' must be a list (got {type(packages).__name__})")

    errors: list[str] = []
    seen: dict[tuple[str, str], int] = {}

    for idx, pkg in enumerate(packages):
        if not isinstance(pkg, dict):
            errors.append(f"packages[{idx}]: expected object, got {type(pkg).__name__}")
            continue

        pkg_id = pkg.get("id")
        arch = pkg.get("arch")
        if not isinstance(pkg_id, str) or not pkg_id:
            errors.append(f"packages[{idx}]: missing/invalid 'id'")
            continue
        if not isinstance(arch, str) or not arch:
            errors.append(f"packages[{idx}]: missing/invalid 'arch' (package id={pkg_id!r})")
            continue

        if pkg.get("required") is True:
            inf = pkg.get("inf")
            if not isinstance(inf, str) or not inf:
                errors.append(
                    f"packages[{idx}]: required package missing/invalid 'inf' (id={pkg_id!r}, arch={arch!r})"
                )

        key = (pkg_id, arch)
        if key in seen:
            other = seen[key]
            errors.append(
                f"duplicate package entries for (id={pkg_id!r}, arch={arch!r}): "
                f"packages[{other}] and packages[{idx}]"
            )
        else:
            seen[key] = idx

    if errors:
        formatted = "\n".join(f"- {e}" for e in errors)
        raise SystemExit(f"invalid manifest structure:\n{formatted}")

    # Type-checked via validation above.
    return packages  # type: ignore[return-value]


def main() -> int:
    repo_root = Path(__file__).resolve().parents[2]

    parser = argparse.ArgumentParser(description="Verify an Aero virtio drivers ISO contains required files.")
    parser.add_argument(
        "--manifest",
        type=Path,
        default=repo_root / "drivers/virtio/manifest.json",
        help="Path to drivers/virtio/manifest.json",
    )
    parser.add_argument(
        "--require-arch",
        choices=["x86", "amd64", "both"],
        default="both",
        help="Which architecture(s) must be present for required drivers (default: both).",
    )
    parser.add_argument("--iso", type=Path, required=True, help="ISO to verify")
    args = parser.parse_args()

    manifest = _load_manifest(args.manifest)
    packages = _validate_manifest(manifest)

    iso_path = args.iso.resolve()
    if not iso_path.is_file():
        raise SystemExit(f"ISO not found: {iso_path}")

    files = _list_iso_files(iso_path)

    missing: list[str] = []
    readme_name = manifest.get("iso", {}).get("readme_filename") or "README.txt"
    readme_path = f"/{readme_name}"
    if _normalize_iso_path(readme_path) not in files:
        missing.append(readme_path)
    if _normalize_iso_path("/THIRD_PARTY_NOTICES.md") not in files:
        missing.append("/THIRD_PARTY_NOTICES.md")

    arches = set(_arches_for_require_arch(args.require_arch))
    required_packages = [pkg for pkg in packages if pkg.get("required") is True and pkg.get("arch") in arches]
    for pkg in required_packages:
        pkg_id = pkg.get("id")
        arch = pkg.get("arch")
        inf = pkg.get("inf")
        if not isinstance(pkg_id, str) or not pkg_id or not isinstance(arch, str) or not arch:
            # Should not happen if manifest validation succeeded, but keep this defensive.
            missing.append("<manifest invalid package entry>")
            continue
        if not isinstance(inf, str) or not inf:
            missing.append(f"<manifest missing inf> {pkg_id} ({arch})")
            continue
        want = "/" + inf.lstrip("/\\")
        if _normalize_iso_path(want) not in files:
            missing.append(want)

    if missing:
        formatted = "\n".join(f"- {m}" for m in missing)
        print(f"ISO is missing required files (required by --require-arch {args.require_arch}):", file=sys.stderr)
        print(formatted, file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
