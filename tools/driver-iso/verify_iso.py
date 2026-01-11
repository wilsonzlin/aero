#!/usr/bin/env python3

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


REQUIRED_PACKAGE_IDS = ("virtio-blk", "virtio-net")


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


def _list_iso_files_with_xorriso(iso_path: Path) -> set[str]:
    xorriso = shutil.which("xorriso")
    if not xorriso:
        raise SystemExit("xorriso not found; install xorriso to verify ISO contents")

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
        raise SystemExit("powershell.exe not found; cannot verify ISO contents without xorriso")

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
    xorriso = shutil.which("xorriso")
    if xorriso:
        return _list_iso_files_with_xorriso(iso_path)
    if os.name == "nt":
        return _list_iso_files_with_powershell_mount(iso_path)
    raise SystemExit("xorriso not found; install xorriso to verify ISO contents")


def _arches_for_require_arch(require_arch: str) -> tuple[str, ...]:
    if require_arch == "both":
        return ("x86", "amd64")
    return (require_arch,)


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
    files = _list_iso_files(args.iso.resolve())

    missing: list[str] = []
    readme_name = manifest.get("iso", {}).get("readme_filename") or "README.txt"
    readme_path = f"/{readme_name}"
    if _normalize_iso_path(readme_path) not in files:
        missing.append(readme_path)
    if _normalize_iso_path("/THIRD_PARTY_NOTICES.md") not in files:
        missing.append("/THIRD_PARTY_NOTICES.md")
    packages = manifest.get("packages", [])
    for pkg_id in REQUIRED_PACKAGE_IDS:
        for arch in _arches_for_require_arch(args.require_arch):
            matches = [pkg for pkg in packages if pkg.get("id") == pkg_id and pkg.get("arch") == arch]
            if not matches:
                missing.append(f"<manifest entry missing> {pkg_id} ({arch})")
                continue
            if len(matches) > 1:
                missing.append(f"<manifest has multiple entries> {pkg_id} ({arch})")
                continue

            inf = matches[0].get("inf")
            if not inf:
                missing.append(f"<manifest missing inf> {pkg_id} ({arch})")
                continue
            want = f"/{inf}"
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
