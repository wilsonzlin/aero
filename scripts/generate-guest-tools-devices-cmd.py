#!/usr/bin/env python3
"""
Generate `guest-tools/config/devices.cmd` from `docs/windows-device-contract.json`.

Why:
- `devices.cmd` is consumed by Guest Tools install/verify scripts.
- Manually editing PCI HWIDs / service names tends to drift from:
  - `docs/windows7-virtio-driver-contract.md` (AERO-W7-VIRTIO),
  - `docs/windows-device-contract.json` (machine-readable manifest),
  - in-tree Windows driver INFs (AddService + HWIDs).

This generator makes `docs/windows-device-contract.json` the single source of truth
and provides a `--check` mode suitable for CI.
"""

from __future__ import annotations

import argparse
import difflib
import json
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONTRACT_PATH = REPO_ROOT / "docs/windows-device-contract.json"
DEFAULT_OUTPUT_PATH = REPO_ROOT / "guest-tools/config/devices.cmd"


class GenerationError(RuntimeError):
    pass


def _fail(message: str) -> None:
    raise GenerationError(message)


def _require_dict(value: Any, *, ctx: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        _fail(f"Expected object for {ctx}, got {type(value).__name__}")
    return value


def _require_str(value: Any, *, ctx: str) -> str:
    if not isinstance(value, str) or not value:
        _fail(f"Expected non-empty string for {ctx}, got {value!r}")
    return value


def _require_str_list(value: Any, *, ctx: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
        _fail(f"Expected list[str] for {ctx}, got {value!r}")
    return list(value)


def _cmd_quote(value: str) -> str:
    # We intentionally quote each HWID individually so `&` is safe in CMD.
    if '"' in value:
        _fail(f"devices.cmd values must not contain quotes (\") but got: {value!r}")
    return f'"{value}"'


def _load_contract(contract_path: Path) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    try:
        data = json.loads(contract_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        _fail(f"Missing contract file: {contract_path.as_posix()}")
    except json.JSONDecodeError as e:
        _fail(f"Invalid JSON in {contract_path.as_posix()}: {e}")

    root = _require_dict(data, ctx=contract_path.as_posix())
    devices_value = root.get("devices")
    if not isinstance(devices_value, list):
        _fail(f"{contract_path.as_posix()}: expected top-level 'devices' array")

    devices: dict[str, dict[str, Any]] = {}
    for i, entry in enumerate(devices_value):
        if not isinstance(entry, dict):
            _fail(f"{contract_path.as_posix()}: devices[{i}] must be an object")
        name = entry.get("device")
        if not isinstance(name, str) or not name:
            _fail(f"{contract_path.as_posix()}: devices[{i}].device must be a non-empty string")
        if name in devices:
            _fail(f"{contract_path.as_posix()}: duplicate device entry: {name!r}")
        devices[name] = entry

    return root, devices

def _render_devices_cmd(contract: dict[str, Any], devices: dict[str, dict[str, Any]]) -> str:
    contract_version = contract.get("contract_version")
    contract_version = contract_version if isinstance(contract_version, str) else None
    contract_name = contract.get("contract_name")
    contract_name = contract_name if isinstance(contract_name, str) and contract_name else None
    schema_version = contract.get("schema_version")
    schema_version = schema_version if isinstance(schema_version, int) else None

    def device(name: str) -> dict[str, Any]:
        if name not in devices:
            _fail(f"windows-device-contract.json is missing required device entry: {name!r}")
        return devices[name]

    virtio_blk = device("virtio-blk")
    virtio_net = device("virtio-net")
    virtio_input = device("virtio-input")
    virtio_snd = device("virtio-snd")
    aero_gpu = device("aero-gpu")

    blk_service = _require_str(virtio_blk.get("driver_service_name"), ctx="virtio-blk.driver_service_name")
    net_service = _require_str(virtio_net.get("driver_service_name"), ctx="virtio-net.driver_service_name")
    input_service = _require_str(virtio_input.get("driver_service_name"), ctx="virtio-input.driver_service_name")
    snd_service = _require_str(virtio_snd.get("driver_service_name"), ctx="virtio-snd.driver_service_name")
    gpu_service = _require_str(aero_gpu.get("driver_service_name"), ctx="aero-gpu.driver_service_name")

    blk_hwids = _require_str_list(virtio_blk.get("hardware_id_patterns"), ctx="virtio-blk.hardware_id_patterns")
    net_hwids = _require_str_list(virtio_net.get("hardware_id_patterns"), ctx="virtio-net.hardware_id_patterns")
    input_hwids = _require_str_list(virtio_input.get("hardware_id_patterns"), ctx="virtio-input.hardware_id_patterns")
    snd_hwids = _require_str_list(virtio_snd.get("hardware_id_patterns"), ctx="virtio-snd.hardware_id_patterns")
    gpu_hwids = _require_str_list(aero_gpu.get("hardware_id_patterns"), ctx="aero-gpu.hardware_id_patterns")

    lines: list[str] = []
    lines.append("@echo off")
    lines.append("rem -----------------------------------------------------------------------------")
    lines.append("rem GENERATED FILE - DO NOT EDIT MANUALLY")
    lines.append("rem")
    lines.append("rem Source of truth: Windows device contract JSON")
    lines.append("rem Generator: scripts/generate-guest-tools-devices-cmd.py")
    if contract_name:
        lines.append(f"rem Contract name: {contract_name}")
    if schema_version is not None:
        lines.append(f"rem Contract schema_version: {schema_version}")
    if contract_version:
        lines.append(f"rem Contract version: {contract_version}")
    lines.append("rem -----------------------------------------------------------------------------")
    lines.append("")
    lines.append(r"rem This file is sourced by guest-tools\setup.cmd and guest-tools\uninstall.cmd.")
    lines.append("")
    lines.append("rem ---------------------------")
    lines.append("rem Boot-critical storage (virtio-blk)")
    lines.append("rem ---------------------------")
    lines.append("")
    lines.append(f'set "AERO_VIRTIO_BLK_SERVICE={blk_service}"')
    lines.append('set "AERO_VIRTIO_BLK_SYS="')
    lines.append(f"set AERO_VIRTIO_BLK_HWIDS={' '.join(_cmd_quote(h) for h in blk_hwids)}")
    lines.append("")
    lines.append("rem ---------------------------")
    lines.append("rem Non-boot-critical devices (used by verify.ps1)")
    lines.append("rem ---------------------------")
    lines.append("")
    lines.append(f'set "AERO_VIRTIO_NET_SERVICE={net_service}"')
    lines.append(f"set AERO_VIRTIO_NET_HWIDS={' '.join(_cmd_quote(h) for h in net_hwids)}")
    lines.append(f'set "AERO_VIRTIO_INPUT_SERVICE={input_service}"')
    lines.append(f"set AERO_VIRTIO_INPUT_HWIDS={' '.join(_cmd_quote(h) for h in input_hwids)}")
    lines.append(f'set "AERO_VIRTIO_SND_SERVICE={snd_service}"')
    lines.append('set "AERO_VIRTIO_SND_SYS="')
    lines.append(f"set AERO_VIRTIO_SND_HWIDS={' '.join(_cmd_quote(h) for h in snd_hwids)}")
    lines.append("rem")
    lines.append("rem AeroGPU HWIDs:")
    lines.append(r"rem   - PCI\VEN_A3A0&DEV_0001  (canonical / current)")
    lines.append(r"rem   - PCI\VEN_1AED&DEV_0001  (legacy bring-up ABI; emulator/aerogpu-legacy)")
    lines.append("rem The Win7 driver package includes INFs for both; Guest Tools should accept either.")
    lines.append(f'set "AERO_GPU_SERVICE={gpu_service}"')
    lines.append(f"set AERO_GPU_HWIDS={' '.join(_cmd_quote(h) for h in gpu_hwids)}")
    lines.append("")

    # Use `\n` internally; writing uses explicit CRLF for `.cmd` checkout consistency.
    return "\n".join(lines) + "\n"


def _write_cmd_file(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\r\n") as f:
        f.write(content)


def _unified_diff(a: str, b: str, *, fromfile: str, tofile: str) -> str:
    return "".join(
        difflib.unified_diff(
            a.splitlines(keepends=True),
            b.splitlines(keepends=True),
            fromfile=fromfile,
            tofile=tofile,
        )
    )


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
        help="Path to guest-tools/config/devices.cmd (default: repo copy).",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check whether the output file is up-to-date (do not write).",
    )

    args = parser.parse_args(argv)

    contract, devices = _load_contract(args.contract)
    rendered = _render_devices_cmd(contract, devices)

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
            sys.stderr.write(diff if diff else "devices.cmd is out of date\n")
            sys.stderr.write(
                "\nERROR: guest-tools/config/devices.cmd is out of sync with docs/windows-device-contract.json.\n"
                "Run: python3 scripts/generate-guest-tools-devices-cmd.py\n"
            )
            return 1
        return 0

    _write_cmd_file(args.output, rendered)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except GenerationError as e:
        print(f"error: {e}", file=sys.stderr)
        raise SystemExit(2)
