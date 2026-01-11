#!/usr/bin/env python3

import argparse
import json
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


def main() -> int:
    repo_root = Path(__file__).resolve().parents[2]

    parser = argparse.ArgumentParser(description="Verify an Aero virtio drivers ISO contains required files.")
    parser.add_argument(
        "--manifest",
        type=Path,
        default=repo_root / "drivers/virtio/manifest.json",
        help="Path to drivers/virtio/manifest.json",
    )
    parser.add_argument("--iso", type=Path, required=True, help="ISO to verify")
    args = parser.parse_args()

    manifest = _load_manifest(args.manifest)
    files = _list_iso_files_with_xorriso(args.iso.resolve())

    missing: list[str] = []
    if "/THIRD_PARTY_NOTICES.md" not in files:
        missing.append("/THIRD_PARTY_NOTICES.md")
    for pkg in manifest.get("packages", []):
        if not pkg.get("required"):
            continue
        inf = pkg.get("inf")
        if not inf:
            continue
        want = f"/{inf}"
        if want not in files:
            missing.append(want)

    if missing:
        formatted = "\n".join(f"- {m}" for m in missing)
        print("ISO is missing required files:", file=sys.stderr)
        print(formatted, file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
