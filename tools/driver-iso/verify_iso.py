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
    iso.open(str(iso_path))
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
            except BaseException as e:
                last_err = e
                continue

        raise SystemExit(f"pycdlib failed to walk the ISO filesystem: {last_err}")
    finally:
        iso.close()


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
    # Prefer a pure-Python implementation when available so this script works on
    # Linux/macOS without requiring external ISO tooling.
    try:
        import pycdlib  # type: ignore  # noqa: F401

        return _list_iso_files_with_pycdlib(iso_path)
    except ModuleNotFoundError:
        pass

    xorriso = shutil.which("xorriso")
    if xorriso:
        return _list_iso_files_with_xorriso(iso_path)
    if os.name == "nt":
        return _list_iso_files_with_powershell_mount(iso_path)
    raise SystemExit(
        "no supported ISO listing backend found.\n"
        "Install one of:\n"
        "- pycdlib: python3 -m pip install pycdlib\n"
        "- xorriso (via your OS package manager)"
    )


def _arches_for_require_arch(require_arch: str) -> tuple[str, ...]:
    if require_arch == "both":
        return ("x86", "amd64")
    return (require_arch,)


def _require_unique_manifest_packages(packages: list[dict]) -> None:
    """
    Ensure each (id, arch) pair appears only once in the manifest.
    """

    seen: set[tuple[str, str]] = set()
    dupes: set[tuple[str, str]] = set()
    for i, pkg in enumerate(packages):
        pkg_id = pkg.get("id")
        arch = pkg.get("arch")
        if not isinstance(pkg_id, str) or not pkg_id:
            raise SystemExit(f"manifest package entry #{i} is missing a valid 'id'")
        if not isinstance(arch, str) or not arch:
            raise SystemExit(f"manifest package entry #{i} ({pkg_id}) is missing a valid 'arch'")
        key = (pkg_id, arch)
        if key in seen:
            dupes.add(key)
        else:
            seen.add(key)

    if dupes:
        formatted = "\n".join(f"- {pkg_id} ({arch})" for (pkg_id, arch) in sorted(dupes))
        raise SystemExit(f"manifest has duplicate package entries for the same (id, arch):\n{formatted}")


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
    packages = manifest.get("packages", [])
    if not isinstance(packages, list):
        raise SystemExit(f"manifest field 'packages' must be a list: {args.manifest}")
    _require_unique_manifest_packages(packages)
    files = _list_iso_files(args.iso.resolve())

    missing: list[str] = []
    readme_name = manifest.get("iso", {}).get("readme_filename") or "README.txt"
    readme_path = f"/{readme_name}"
    if _normalize_iso_path(readme_path) not in files:
        missing.append(readme_path)
    if _normalize_iso_path("/THIRD_PARTY_NOTICES.md") not in files:
        missing.append("/THIRD_PARTY_NOTICES.md")
    required_arches = set(_arches_for_require_arch(args.require_arch))
    required_packages = [
        pkg for pkg in packages if pkg.get("required") is True and pkg.get("arch") in required_arches
    ]
    for pkg in required_packages:
        pkg_id = pkg.get("id")
        arch = pkg.get("arch")
        inf = pkg.get("inf")
        if not isinstance(pkg_id, str) or not pkg_id:
            missing.append("<manifest missing id> <unknown>")
            continue
        if not isinstance(arch, str) or not arch:
            missing.append(f"<manifest missing arch> {pkg_id}")
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
