#!/usr/bin/env python3

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


REQUIRED_PACKAGE_IDS = ("virtio-blk", "virtio-net")


def _load_manifest(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"manifest not found: {path}")
    except json.JSONDecodeError as e:
        raise SystemExit(f"failed to parse manifest JSON ({path}): {e}")


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _find_iso_tool() -> tuple[str, list[str]]:
    """
    Returns (tool_kind, base_cmd).

    tool_kind is used for friendly error messages and to select flags.
    """

    xorriso = shutil.which("xorriso")
    if xorriso:
        # xorriso provides mkisofs-compatible mode with better availability on Linux.
        return ("xorriso", [xorriso, "-as", "mkisofs"])

    genisoimage = shutil.which("genisoimage")
    if genisoimage:
        return ("genisoimage", [genisoimage])

    mkisofs = shutil.which("mkisofs")
    if mkisofs:
        return ("mkisofs", [mkisofs])

    oscdimg = shutil.which("oscdimg")
    if oscdimg:
        return ("oscdimg", [oscdimg])

    raise SystemExit(
        "no ISO authoring tool found. Install one of: xorriso, genisoimage, mkisofs "
        "(or oscdimg on Windows)."
    )


def _build_iso_with_imapi(stage_root: Path, out_path: Path, label: str) -> None:
    """
    Windows fallback: build an ISO using the built-in IMAPI COM APIs.

    This avoids requiring third-party mkisofs/xorriso installs on Windows CI runners.
    """

    repo_root = Path(__file__).resolve().parents[2]
    script = repo_root / "ci" / "lib" / "New-IsoFile.ps1"
    if not script.is_file():
        raise SystemExit(f"IMAPI ISO builder not found: {script}")

    powershell = shutil.which("powershell")
    if not powershell:
        raise SystemExit("powershell.exe not found; required for IMAPI ISO generation fallback")

    subprocess.run(
        [
            powershell,
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
            "-SourcePath",
            str(stage_root),
            "-IsoPath",
            str(out_path),
            "-VolumeLabel",
            label,
        ],
        check=True,
    )


def _arches_for_require_arch(require_arch: str) -> tuple[str, ...]:
    if require_arch == "both":
        return ("x86", "amd64")
    return (require_arch,)


def _validate_required_packages(manifest: dict, drivers_root: Path, require_arch: str) -> None:
    missing: list[str] = []

    packages = manifest.get("packages", [])
    for pkg_id in REQUIRED_PACKAGE_IDS:
        for arch in _arches_for_require_arch(require_arch):
            matches = [pkg for pkg in packages if pkg.get("id") == pkg_id and pkg.get("arch") == arch]
            if not matches:
                missing.append(f"{pkg_id} ({arch}): missing package entry in manifest")
                continue
            if len(matches) > 1:
                missing.append(f"{pkg_id} ({arch}): multiple package entries in manifest")
                continue

            pkg = matches[0]
            inf_rel = pkg.get("inf")
            if not inf_rel:
                missing.append(f"{pkg_id} ({arch}): missing 'inf' in manifest")
                continue
            inf_path = drivers_root / inf_rel
            if not inf_path.is_file():
                missing.append(f"{pkg_id} ({arch}): {inf_rel} not found under {drivers_root}")

    if missing:
        formatted = "\n".join(f"- {m}" for m in missing)
        raise SystemExit(
            f"required driver package files are missing (required by --require-arch {require_arch}):\n"
            f"{formatted}\n\n"
            "Hint: if you intentionally want a single-arch ISO, pass `--require-arch x86` or "
            "`--require-arch amd64`.\n"
            "Hint: for a demo build, use `--drivers-root drivers/virtio/sample`.\n"
            "For a real build, populate `drivers/virtio/prebuilt/` with a Win7-capable virtio driver set."
        )


def _write_readme(stage_root: Path, filename: str, label: str) -> None:
    (stage_root / filename).write_text(
        "\n".join(
            [
                f"{label}",
                "",
                "Aero Virtio Drivers ISO",
                "",
                "This ISO is intended to be mounted inside the Aero Windows 7 VM as a CD-ROM.",
                "Install drivers via Windows Setup (Load Driver) or Device Manager.",
                "",
                "See docs/virtio-windows-drivers.md in the Aero repo for the recommended flow.",
                "",
            ]
        )
        + "\n",
        encoding="utf-8",
    )

def _copy_third_party_notices(repo_root: Path, drivers_root: Path, stage_root: Path) -> None:
    """
    Ensure the ISO contains third-party redistribution notices.

    Prefer a `THIRD_PARTY_NOTICES.md` next to the chosen driver root (e.g. the
    output of `make-driver-pack.ps1`). Fall back to the repo's template under
    `drivers/virtio/` so even sample builds ship a notices file.
    """

    candidates = [
        drivers_root / "THIRD_PARTY_NOTICES.md",
        repo_root / "drivers/virtio/THIRD_PARTY_NOTICES.md",
    ]
    for src in candidates:
        if src.is_file():
            shutil.copy2(src, stage_root / "THIRD_PARTY_NOTICES.md")
            return

    raise SystemExit(
        "third-party notices file not found; expected one of:\n"
        + "\n".join(f"- {p}" for p in candidates)
    )


def main() -> int:
    repo_root = Path(__file__).resolve().parents[2]

    parser = argparse.ArgumentParser(description="Build an Aero Windows virtio drivers ISO.")
    parser.add_argument(
        "--manifest",
        type=Path,
        default=repo_root / "drivers/virtio/manifest.json",
        help="Path to drivers/virtio/manifest.json",
    )
    parser.add_argument(
        "--drivers-root",
        type=Path,
        default=repo_root / "drivers/virtio/prebuilt",
        help="Root directory containing win7/… driver files",
    )
    parser.add_argument(
        "--require-arch",
        choices=["x86", "amd64", "both"],
        default="both",
        help="Which architecture(s) must be present for required drivers (default: both).",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="Output ISO path",
    )
    parser.add_argument(
        "--label",
        type=str,
        default=None,
        help="Override ISO volume label (defaults to manifest)",
    )
    parser.add_argument(
        "--include-manifest",
        action="store_true",
        help=(
            "Include the drivers/virtio/manifest.json in the ISO root (useful for debugging). "
            "If the chosen --drivers-root already contains manifest.json, the file is written "
            "as virtio-manifest.json to avoid clobbering."
        ),
    )
    args = parser.parse_args()

    manifest = _load_manifest(args.manifest)
    label = args.label or manifest.get("iso", {}).get("volume_label") or "AERO_VIRTIO"
    readme_filename = manifest.get("iso", {}).get("readme_filename") or "README.txt"

    drivers_root = args.drivers_root.resolve()
    _validate_required_packages(manifest, drivers_root, args.require_arch)

    out_path = args.output.resolve()
    out_path.parent.mkdir(parents=True, exist_ok=True)

    try:
        tool_kind, tool_cmd = _find_iso_tool()
    except SystemExit:
        if os.name != "nt":
            raise
        tool_kind, tool_cmd = ("imapi", [])

    with tempfile.TemporaryDirectory(prefix="aero-virtio-iso-") as tmp:
        stage_root = Path(tmp) / "root"
        stage_root.mkdir(parents=True, exist_ok=True)

        # Copy driver directory tree into ISO root.
        #
        # We intentionally copy the directory contents rather than the directory itself so
        # the ISO root contains `win7/…` directly.
        for child in drivers_root.iterdir():
            # Avoid copying local documentation.
            if child.name.lower().endswith(".md"):
                continue
            dest = stage_root / child.name
            if child.is_dir():
                shutil.copytree(child, dest)
            else:
                shutil.copy2(child, dest)

        _copy_third_party_notices(repo_root, drivers_root, stage_root)
        _write_readme(stage_root, readme_filename, label)
        if args.include_manifest:
            # If the chosen drivers root already includes a `manifest.json` (e.g. the
            # output of `make-driver-pack.ps1`), avoid overwriting it.
            dest_name = "manifest.json"
            if (stage_root / dest_name).exists():
                dest_name = "virtio-manifest.json"
            shutil.copy2(args.manifest, stage_root / dest_name)

        if tool_kind == "imapi":
            _build_iso_with_imapi(stage_root, out_path, label)
        elif tool_kind == "oscdimg":
            cmd = [
                *tool_cmd,
                "-m",
                "-o",
                f"-l{label}",
                str(stage_root),
                str(out_path),
            ]
        else:
            # -iso-level 3: allow files >2GB and deeper paths (harmless for small ISOs).
            cmd = [
                *tool_cmd,
                "-iso-level",
                "3",
                "-J",
                "-R",
                "-V",
                label,
                "-o",
                str(out_path),
                str(stage_root),
            ]

        if tool_kind != "imapi":
            subprocess.run(cmd, check=True)

    print(f"Wrote {out_path} (sha256={_sha256(out_path)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
