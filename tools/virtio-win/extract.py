#!/usr/bin/env python3

"""
Extract the minimal subset of an upstream virtio-win ISO needed by Aero.

This tool exists because the Windows-only PowerShell workflow (`Mount-DiskImage`)
cannot run on Linux/macOS. The extracted directory can be passed to:

  pwsh drivers/scripts/make-driver-pack.ps1 -VirtioWinRoot <out-root>

The output directory preserves the original virtio-win on-disk structure, but only
for the Win7-relevant subtrees Aero uses.
"""

import argparse
import dataclasses
import datetime as _dt
import hashlib
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any, Iterable, Optional


OS_FOLDER_CANDIDATES = ["w7", "w7.1", "win7"]
ARCH_CANDIDATES_AMD64 = ["amd64", "x64"]
ARCH_CANDIDATES_X86 = ["x86", "i386"]

DEFAULT_PROVENANCE_FILENAME = "virtio-win-provenance.json"


@dataclasses.dataclass(frozen=True)
class DriverSpec:
    # Normalized Aero-facing ID.
    id: str
    # Typical upstream directory name in the virtio-win ISO.
    upstream_dir: str
    # Whether Aero requires this driver to be present.
    required: bool


DRIVERS: list[DriverSpec] = [
    DriverSpec(id="viostor", upstream_dir="viostor", required=True),
    DriverSpec(id="netkvm", upstream_dir="NetKVM", required=True),
    DriverSpec(id="viosnd", upstream_dir="viosnd", required=False),
    DriverSpec(id="vioinput", upstream_dir="vioinput", required=False),
]


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _utc_now_iso() -> str:
    return _dt.datetime.now(tz=_dt.UTC).isoformat().replace("+00:00", "Z")


def _ensure_empty_dir(path: Path, *, clean: bool) -> None:
    if path.exists():
        if not path.is_dir():
            raise SystemExit(f"out-root exists but is not a directory: {path}")
        if clean:
            shutil.rmtree(path)
        elif any(path.iterdir()):
            raise SystemExit(f"out-root is not empty: {path} (use --clean to overwrite)")
    path.mkdir(parents=True, exist_ok=True)


@dataclasses.dataclass
class _IsoNode:
    name: str
    parent: Optional["_IsoNode"]
    children: dict[str, "_IsoNode"] = dataclasses.field(default_factory=dict)

    def child(self, name: str) -> "_IsoNode":
        key = name.casefold()
        hit = self.children.get(key)
        if hit is None:
            hit = _IsoNode(name=name, parent=self)
            self.children[key] = hit
        return hit

    @property
    def path_parts(self) -> list[str]:
        parts: list[str] = []
        node: Optional[_IsoNode] = self
        while node is not None and node.parent is not None:
            parts.append(node.name)
            node = node.parent
        parts.reverse()
        return parts


@dataclasses.dataclass(frozen=True)
class _ExtractTarget:
    driver_id: str
    upstream_dir: str
    os_dir: str
    arch: str
    arch_dir: str
    iso_path_parts: tuple[str, ...]

    def iso_path(self, sep: str) -> str:
        return sep.join(self.iso_path_parts)


def _split_any_sep(p: str) -> list[str]:
    p = p.strip().lstrip("./")
    p = p.replace("\\", "/")
    return [c for c in p.split("/") if c]


def _find_child_dir(node: _IsoNode, names: Iterable[str]) -> Optional[_IsoNode]:
    for name in names:
        hit = node.children.get(name.casefold())
        if hit is not None:
            return hit
    return None


def _build_tree_from_paths(paths: Iterable[str]) -> _IsoNode:
    root = _IsoNode(name="", parent=None)
    for p in paths:
        parts = _split_any_sep(p)
        if not parts:
            continue
        node = root
        for comp in parts:
            node = node.child(comp)
    return root


def _select_extract_targets(tree: _IsoNode) -> tuple[list[_ExtractTarget], list[dict[str, Any]]]:
    """
    Returns (targets, missing_optional).
    """

    targets: list[_ExtractTarget] = []
    missing_optional: list[dict[str, Any]] = []

    required_errors: list[str] = []

    for drv in DRIVERS:
        driver_node = _find_child_dir(tree, [drv.upstream_dir])
        if driver_node is None:
            if drv.required:
                required_errors.append(f"required driver directory missing at ISO root: {drv.upstream_dir}")
            else:
                missing_optional.append({"driver": drv.id, "reason": "driver directory missing"})
            continue

        os_node = _find_child_dir(driver_node, OS_FOLDER_CANDIDATES)
        if os_node is None:
            if drv.required:
                required_errors.append(
                    f"required driver '{drv.upstream_dir}': could not find Win7 OS dir under {drv.upstream_dir} "
                    f"(tried: {', '.join(OS_FOLDER_CANDIDATES)})"
                )
            else:
                missing_optional.append({"driver": drv.id, "reason": "win7 OS directory missing"})
            continue

        arch_nodes = {
            "amd64": _find_child_dir(os_node, ARCH_CANDIDATES_AMD64),
            "x86": _find_child_dir(os_node, ARCH_CANDIDATES_X86),
        }

        for arch, arch_node in arch_nodes.items():
            if arch_node is None:
                if drv.required:
                    required_errors.append(
                        f"required driver '{drv.upstream_dir}': missing arch dir for {arch} under "
                        f"{drv.upstream_dir}/{os_node.name} (tried: "
                        f"{', '.join(ARCH_CANDIDATES_AMD64 if arch == 'amd64' else ARCH_CANDIDATES_X86)})"
                    )
                else:
                    missing_optional.append({"driver": drv.id, "arch": arch, "reason": "arch directory missing"})
                continue

            targets.append(
                _ExtractTarget(
                    driver_id=drv.id,
                    upstream_dir=driver_node.name,
                    os_dir=os_node.name,
                    arch=arch,
                    arch_dir=arch_node.name,
                    iso_path_parts=tuple(arch_node.path_parts),
                )
            )

    if required_errors:
        formatted = "\n".join(f"- {e}" for e in required_errors)
        raise SystemExit(f"virtio-win ISO is missing required driver content:\n{formatted}")

    return targets, missing_optional


def _find_7z() -> Optional[str]:
    for name in ("7z", "7zz", "7za"):
        hit = shutil.which(name)
        if hit:
            return hit
    return None


def _detect_7z_sep(paths_raw: Iterable[str]) -> str:
    # 7z on some platforms prints paths with "\" even when running on Unix.
    for p in paths_raw:
        if "\\" in p:
            return "\\"
        if "/" in p:
            return "/"
    return "/"  # default


def _list_iso_paths_with_7z(sevenz: str, iso_path: Path) -> tuple[list[str], str]:
    proc = subprocess.run(
        [sevenz, "l", "-slt", str(iso_path)],
        check=True,
        capture_output=True,
        text=True,
    )
    in_entries = False
    paths_raw: list[str] = []
    for line in proc.stdout.splitlines():
        if line.startswith("----------"):
            in_entries = True
            continue
        if not in_entries:
            continue
        if line.startswith("Path = "):
            p = line[len("Path = ") :].strip()
            if p:
                paths_raw.append(p)

    sep = _detect_7z_sep(paths_raw)
    # Normalize to "/" for tree building.
    paths_norm = [p.replace("\\", "/") for p in paths_raw]
    return paths_norm, sep


def _extract_with_7z(sevenz: str, iso_path: Path, out_root: Path, targets: list[_ExtractTarget], sep: str) -> None:
    if not targets:
        raise SystemExit("nothing to extract (no matching driver paths found)")

    # 7z accepts directory paths to extract an entire subtree. We pass the exact
    # paths discovered from the listing (preserving case) for robustness.
    archive_paths = [t.iso_path(sep) for t in targets]

    cmd = [sevenz, "x", "-y", f"-o{out_root}", str(iso_path), *archive_paths]
    subprocess.run(cmd, check=True)


def _extract_with_pycdlib(iso_path: Path, out_root: Path, targets: list[_ExtractTarget]) -> None:
    try:
        import pycdlib  # type: ignore
    except ModuleNotFoundError as e:
        raise SystemExit(
            "pycdlib is not installed and 7z was not found.\n"
            "Install one of:\n"
            "- 7z (p7zip): https://www.7-zip.org/ or your OS package manager\n"
            "- pycdlib: python3 -m pip install pycdlib\n"
        ) from e

    iso = pycdlib.PyCdlib()
    iso.open(str(iso_path))
    try:
        # Build a list of files to extract by walking Joliet paths. Virtio-win ISOs
        # are typically authored with Joliet, which preserves the mixed-case paths.
        files: list[tuple[str, str]] = []  # (joliet_path, dest_rel)
        want_prefixes = ["/" + "/".join(t.iso_path_parts) + "/" for t in targets]
        want_prefixes = [p.replace("//", "/") for p in want_prefixes]

        for root, _dirs, filelist in iso.walk(joliet_path="/"):
            for f in filelist:
                # `root` is a joliet_path like "/viostor/w7.1/amd64"
                jp = f"{root.rstrip('/')}/{f}"
                for pref in want_prefixes:
                    if jp.casefold().startswith(pref.casefold()):
                        dest_rel = jp.lstrip("/")
                        files.append((jp, dest_rel))
                        break

        if not files:
            raise SystemExit("no matching files found to extract (pycdlib)")

        for jp, dest_rel in files:
            dest = out_root / dest_rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            with dest.open("wb") as fp:
                iso.get_file_from_iso_fp(fp, joliet_path=jp)
    finally:
        iso.close()


def main() -> int:
    parser = argparse.ArgumentParser(description="Extract a minimal Win7 virtio-win driver root for Aero.")
    parser.add_argument("--virtio-win-iso", type=Path, required=True, help="Path to virtio-win.iso")
    parser.add_argument(
        "--out-root",
        type=Path,
        required=True,
        help="Output directory root; contents will match virtio-win ISO layout for the selected subtrees",
    )
    parser.add_argument(
        "--provenance",
        type=Path,
        default=None,
        help=f"Write provenance JSON (default: <out-root>/{DEFAULT_PROVENANCE_FILENAME})",
    )
    parser.add_argument("--clean", action="store_true", help="Delete out-root if it already exists")
    parser.add_argument(
        "--backend",
        choices=["auto", "7z", "pycdlib"],
        default="auto",
        help="Force extraction backend (default: auto).",
    )
    args = parser.parse_args()

    iso_path = args.virtio_win_iso.resolve()
    if not iso_path.is_file():
        raise SystemExit(f"virtio-win ISO not found: {iso_path}")

    out_root = args.out_root.resolve()
    _ensure_empty_dir(out_root, clean=args.clean)

    provenance_path = (args.provenance or (out_root / DEFAULT_PROVENANCE_FILENAME)).resolve()

    iso_hash = _sha256(iso_path)

    backend: str
    sevenz = _find_7z()
    if args.backend == "7z":
        if not sevenz:
            raise SystemExit("7z backend forced, but 7z was not found on PATH")
        backend = "7z"
    elif args.backend == "pycdlib":
        backend = "pycdlib"
    else:
        backend = "7z" if sevenz else "pycdlib"

    targets: list[_ExtractTarget]
    missing_optional: list[dict[str, Any]]

    sep_for_7z = "/"
    if backend == "7z":
        assert sevenz is not None
        paths_norm, sep_for_7z = _list_iso_paths_with_7z(sevenz, iso_path)
        tree = _build_tree_from_paths(paths_norm)
        targets, missing_optional = _select_extract_targets(tree)
        _extract_with_7z(sevenz, iso_path, out_root, targets, sep_for_7z)
    else:
        # pycdlib: we still need to discover the targets. Since we do not have a
        # full path listing without opening the ISO, we reuse pycdlib walking
        # to build a normalized path list.
        try:
            import pycdlib  # type: ignore
        except ModuleNotFoundError:
            raise SystemExit(
                "no supported ISO extraction backend found.\n"
                "Install either:\n"
                "- 7z (p7zip)\n"
                "- pycdlib: python3 -m pip install pycdlib"
            )

        iso = pycdlib.PyCdlib()
        iso.open(str(iso_path))
        try:
            all_paths: list[str] = []
            for root, dirs, files in iso.walk(joliet_path="/"):
                for d in dirs:
                    all_paths.append(f"{root.rstrip('/')}/{d}")
                for f in files:
                    all_paths.append(f"{root.rstrip('/')}/{f}")
        finally:
            iso.close()

        tree = _build_tree_from_paths(all_paths)
        targets, missing_optional = _select_extract_targets(tree)
        _extract_with_pycdlib(iso_path, out_root, targets)

    # Quick sanity check: ensure each extracted target directory exists.
    # (This protects against 7z selection quirks.)
    extracted: list[dict[str, Any]] = []
    failed: list[str] = []
    for t in targets:
        rel = Path(*t.iso_path_parts)
        dst_dir = out_root / rel
        if not dst_dir.is_dir():
            failed.append(str(rel))
            continue
        extracted.append(
            {
                "driver": t.driver_id,
                "arch": t.arch,
                "iso_path": "/".join(t.iso_path_parts),
            }
        )

    if failed:
        formatted = "\n".join(f"- {p}" for p in failed)
        raise SystemExit(
            "extraction completed but expected directories were not present in the output:\n"
            f"{formatted}\n\n"
            "This usually indicates the extraction backend did not match the ISO paths correctly."
        )

    provenance: dict[str, Any] = {
        "schema_version": 1,
        "created_utc": _utc_now_iso(),
        "virtio_win_iso": {
            "path": str(iso_path),
            "sha256": iso_hash,
        },
        "backend": backend,
        "extracted": extracted,
        "missing_optional": missing_optional,
    }

    provenance_path.write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    print(f"Extracted {len(extracted)} driver directories to {out_root}")
    print(f"Wrote provenance: {provenance_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
