#!/usr/bin/env python3

import argparse
import json
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


def _list_iso_files_with_xorriso(iso_path: Path) -> set[str]:
    xorriso = shutil.which("xorriso")
    if not xorriso:
        raise SystemExit("xorriso not found; install xorriso to verify ISO contents")

    proc = subprocess.run(
        [xorriso, "-indev", str(iso_path), "-find", "/", "-type", "f", "-print"],
        check=True,
        capture_output=True,
        text=True,
    )
    return {line.strip() for line in proc.stdout.splitlines() if line.strip()}


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
    files = _list_iso_files_with_xorriso(args.iso.resolve())

    missing: list[str] = []
    if "/THIRD_PARTY_NOTICES.md" not in files:
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
            if want not in files:
                missing.append(want)

    if missing:
        formatted = "\n".join(f"- {m}" for m in missing)
        print(f"ISO is missing required files (required by --require-arch {args.require_arch}):", file=sys.stderr)
        print(formatted, file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
