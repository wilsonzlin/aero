#!/usr/bin/env python3
"""
Generate `docs/windows-device-contract-virtio-win.json` from the canonical
`docs/windows-device-contract.json`.

Why:
- The virtio-win variant is used when packaging Guest Tools with upstream
  virtio-win drivers (viostor/netkvm/vioinput/viosnd).
- It must mirror PCI IDs + HWID patterns from the canonical contract to avoid
  silent drift.
- Only the driver naming surface differs for virtio devices:
    - driver_service_name
    - inf_name

This generator makes the canonical contract the single source of truth and
provides a `--check` mode suitable for CI.
"""

from __future__ import annotations

import argparse
import copy
import difflib
import json
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONTRACT_PATH = REPO_ROOT / "docs/windows-device-contract.json"
DEFAULT_OUTPUT_PATH = REPO_ROOT / "docs/windows-device-contract-virtio-win.json"

VIRTIO_WIN_CONTRACT_NAME = "aero-windows-pci-device-contract-virtio-win"

VIRTIO_OVERRIDES: dict[str, tuple[str, str]] = {
    "virtio-blk": ("viostor", "viostor.inf"),
    "virtio-net": ("netkvm", "netkvm.inf"),
    "virtio-input": ("vioinput", "vioinput.inf"),
    "virtio-snd": ("viosnd", "viosnd.inf"),
}


class GenerationError(RuntimeError):
    pass


def _fail(message: str) -> None:
    raise GenerationError(message)


def _require_dict(value: Any, *, ctx: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        _fail(f"Expected object for {ctx}, got {type(value).__name__}")
    return value


def _require_list(value: Any, *, ctx: str) -> list[Any]:
    if not isinstance(value, list):
        _fail(f"Expected array for {ctx}, got {type(value).__name__}")
    return list(value)


def _require_str(value: Any, *, ctx: str) -> str:
    if not isinstance(value, str) or not value:
        _fail(f"Expected non-empty string for {ctx}, got {value!r}")
    return value


def _load_contract(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        _fail(f"Missing contract file: {path.as_posix()}")
    except json.JSONDecodeError as e:
        _fail(f"Invalid JSON in {path.as_posix()}: {e}")

    root = _require_dict(data, ctx=path.as_posix())
    _require_list(root.get("devices"), ctx=f"{path.as_posix()}: devices")
    # Validate required top-level fields early so error messages are obvious.
    _require_str(root.get("contract_version"), ctx=f"{path.as_posix()}: contract_version")
    # schema_version is currently an int; just check presence.
    if "schema_version" not in root:
        _fail(f"{path.as_posix()}: missing required field schema_version")
    return root


def _generate_variant(base: dict[str, Any]) -> dict[str, Any]:
    out = copy.deepcopy(base)
    out["contract_name"] = VIRTIO_WIN_CONTRACT_NAME

    devices = _require_list(out.get("devices"), ctx="devices")
    new_devices: list[dict[str, Any]] = []
    for idx, dev_any in enumerate(devices):
        dev = _require_dict(dev_any, ctx=f"devices[{idx}]")
        name = _require_str(dev.get("device"), ctx=f"devices[{idx}].device")

        # We treat the presence of virtio_device_type as the "is virtio device" signal.
        # This matches the canonical contract schema used throughout the repo.
        if "virtio_device_type" in dev:
            if name not in VIRTIO_OVERRIDES:
                _fail(
                    f"Unsupported virtio device {name!r} in contract: no virtio-win override is defined. "
                    f"Update {Path(__file__).name} (VIRTIO_OVERRIDES) and the docs accordingly."
                )
            service, inf = VIRTIO_OVERRIDES[name]
            dev["driver_service_name"] = service
            dev["inf_name"] = inf

        new_devices.append(dev)

    out["devices"] = new_devices
    return out


def _render_json(obj: Any) -> str:
    # Stable formatting:
    # - 2-space indent
    # - preserve insertion order (sort_keys=False)
    # - trailing newline
    return json.dumps(obj, indent=2, sort_keys=False) + "\n"


def _unified_diff(a: str, b: str, *, fromfile: str, tofile: str) -> str:
    return "".join(
        difflib.unified_diff(
            a.splitlines(keepends=True),
            b.splitlines(keepends=True),
            fromfile=fromfile,
            tofile=tofile,
        )
    )


def _write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as f:
        f.write(content)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--contract",
        type=Path,
        default=DEFAULT_CONTRACT_PATH,
        help="Path to docs/windows-device-contract.json (default: repo copy).",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT_PATH,
        help="Path to docs/windows-device-contract-virtio-win.json (default: repo copy).",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check whether the output file is up-to-date (do not write).",
    )

    args = parser.parse_args(argv)

    base = _load_contract(args.contract)
    generated_obj = _generate_variant(base)
    rendered = _render_json(generated_obj)

    if args.check:
        existing = ""
        if args.output.exists():
            existing = args.output.read_text(encoding="utf-8", errors="replace")
        if existing != rendered:
            diff = _unified_diff(
                existing,
                rendered,
                fromfile=args.output.as_posix(),
                tofile="(generated)",
            )
            sys.stderr.write(diff if diff else "virtio-win contract file is out of date\n")
            sys.stderr.write(
                "\nERROR: docs/windows-device-contract-virtio-win.json is out of sync with docs/windows-device-contract.json.\n"
                "Run: python3 scripts/generate-windows-device-contract-virtio-win.py\n"
            )
            return 1
        return 0

    _write_text(args.output, rendered)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except GenerationError as e:
        print(f"error: {e}", file=sys.stderr)
        raise SystemExit(2)

