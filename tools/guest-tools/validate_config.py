#!/usr/bin/env python3
"""
Lightweight consistency checker for Guest Tools config vs packaging specs.

Why this exists:
- `guest-tools/config/devices.cmd` drives boot-critical driver installation (service
  names + HWIDs).
- `tools/packaging/specs/*.json` drives `aero_packager` validation when building Guest
  Tools media (ISO/zip) from upstream virtio-win drivers.

If these drift (e.g. emulator PCI IDs change but only one side is updated), we can
produce Guest Tools media that fails to install the correct storage/network drivers.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, Iterable, List, Mapping, Sequence, Tuple


class ValidationError(RuntimeError):
    pass


REPO_ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class DevicesConfig:
    virtio_blk_service: str
    virtio_blk_hwids: Tuple[str, ...]
    virtio_net_hwids: Tuple[str, ...]
    virtio_input_hwids: Tuple[str, ...]
    virtio_snd_hwids: Tuple[str, ...]
    aero_gpu_hwids: Tuple[str, ...]


def _resolve_path(value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        path = REPO_ROOT / path
    return path


def _parse_set_assignment(line: str) -> Tuple[str, str] | None:
    """
    Parse a Windows batch `set` assignment.

    Supported forms:
      set "VAR=value"
      set VAR=value
    """

    m = re.match(r"(?i)^\s*set\s+(.+?)\s*$", line)
    if not m:
        return None

    rest = m.group(1).strip()
    # `set "VAR=value"` is the preferred safe form in .cmd files.
    if rest.startswith('"') and rest.endswith('"') and rest.count('"') >= 2:
        rest = rest[1:-1]

    if "=" not in rest:
        return None

    var, value = rest.split("=", 1)
    var = var.strip()
    value = value.strip()

    # If the RHS is a single quoted string, strip the outer quotes.
    # (HWID lists intentionally contain multiple quoted entries; keep those intact.)
    if value.startswith('"') and value.endswith('"') and value[1:-1].count('"') == 0:
        value = value[1:-1]

    return var, value


def _parse_quoted_list(value: str) -> Tuple[str, ...]:
    if value is None:
        return ()
    items = re.findall(r'"([^"]+)"', value)
    if items:
        return tuple(items)
    value = value.strip()
    if not value:
        return ()
    return (value,)


def load_devices_cmd(path: Path) -> DevicesConfig:
    if not path.exists():
        raise ValidationError(f"devices.cmd not found: {path}")

    raw = path.read_text(encoding="utf-8", errors="replace").splitlines()
    vars_map: Dict[str, str] = {}
    for raw_line in raw:
        line = raw_line.strip()
        if not line:
            continue
        lower = line.lower()
        if lower.startswith("rem") or lower.startswith("::") or lower.startswith("@echo"):
            continue

        parsed = _parse_set_assignment(raw_line)
        if not parsed:
            continue

        key, value = parsed
        vars_map[key] = value

    missing: List[str] = []
    for key in ("AERO_VIRTIO_BLK_SERVICE", "AERO_VIRTIO_BLK_HWIDS", "AERO_VIRTIO_NET_HWIDS"):
        if key not in vars_map:
            missing.append(key)
    if missing:
        raise ValidationError(
            "devices.cmd is missing required variables: "
            + ", ".join(missing)
            + f"\nFile: {path}"
        )

    return DevicesConfig(
        virtio_blk_service=vars_map["AERO_VIRTIO_BLK_SERVICE"],
        virtio_blk_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_BLK_HWIDS", "")),
        virtio_net_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_NET_HWIDS", "")),
        virtio_input_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_INPUT_HWIDS", "")),
        virtio_snd_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_SND_HWIDS", "")),
        aero_gpu_hwids=_parse_quoted_list(vars_map.get("AERO_GPU_HWIDS", "")),
    )


def load_packaging_spec_expected_hwids(path: Path) -> Mapping[str, Tuple[str, ...]]:
    if not path.exists():
        raise ValidationError(f"Packaging spec not found: {path}")

    try:
        spec = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise ValidationError(f"Failed to parse JSON spec: {path}\n{e}") from e

    # Support both schemas:
    # - New: {"drivers": [{"name": "...", "required": true/false, "expected_hardware_ids": [...]}, ...]}
    # - Legacy: {"required_drivers": [{"name": "...", "expected_hardware_ids": [...]}, ...]}
    #
    # We merge both if present, matching aero_packager behavior.
    def add_entries(field: str) -> None:
        entries = spec.get(field)
        if entries is None:
            return
        if not isinstance(entries, list):
            raise ValidationError(f"Spec {path} must contain a list field '{field}'.")
        for entry in entries:
            if not isinstance(entry, dict):
                raise ValidationError(f"Spec {path} contains a non-object entry in {field}: {entry!r}")
            name = entry.get("name")
            hwids = entry.get("expected_hardware_ids")
            if not isinstance(name, str) or not name:
                raise ValidationError(f"Spec {path} driver entry missing valid 'name': {entry!r}")
            if hwids is None:
                hwids = []
            if not isinstance(hwids, list) or not all(isinstance(x, str) for x in hwids):
                raise ValidationError(
                    f"Spec {path} driver {name!r} has invalid 'expected_hardware_ids' (expected list[str])."
                )
            existing = out.setdefault(name, [])
            for pattern in hwids:
                if pattern not in existing:
                    existing.append(pattern)

    out: Dict[str, List[str]] = {}
    add_entries("drivers")
    add_entries("required_drivers")

    if not out:
        raise ValidationError(f"Spec {path} must contain a list field 'drivers' or 'required_drivers'.")

    return {name: tuple(patterns) for name, patterns in out.items()}


def _find_first_match(patterns: Sequence[str], hwids: Sequence[str]) -> Tuple[str, str] | None:
    for pattern in patterns:
        try:
            regex = re.compile(pattern)
        except re.error as e:
            raise ValidationError(f"Invalid regex in packaging spec: {pattern!r}\n{e}") from e
        for hwid in hwids:
            if regex.search(hwid):
                return pattern, hwid
    return None


def _format_bullets(items: Iterable[str]) -> str:
    return "\n".join(f"  - {item}" for item in items)


def validate(devices: DevicesConfig, spec_path: Path, spec_expected: Mapping[str, Tuple[str, ...]]) -> None:
    missing = [name for name in ("viostor", "netkvm") if name not in spec_expected]
    if missing:
        raise ValidationError(
            f"Spec {spec_path} is missing required driver entries: {', '.join(missing)}\n"
            "Remediation: update tools/packaging/specs/win7-virtio-win.json to include them."
        )

    if not devices.virtio_blk_hwids:
        raise ValidationError(
            "AERO_VIRTIO_BLK_HWIDS is empty.\n"
            "Remediation: set AERO_VIRTIO_BLK_HWIDS in guest-tools/config/devices.cmd."
        )
    if not devices.virtio_net_hwids:
        raise ValidationError(
            "AERO_VIRTIO_NET_HWIDS is empty.\n"
            "Remediation: set AERO_VIRTIO_NET_HWIDS in guest-tools/config/devices.cmd."
        )

    blk_patterns = spec_expected["viostor"]
    net_patterns = spec_expected["netkvm"]

    blk_match = _find_first_match(blk_patterns, devices.virtio_blk_hwids)
    if not blk_match:
        raise ValidationError(
            "Mismatch: win7-virtio-win.json expects viostor HWIDs that don't match devices.cmd.\n"
            "\n"
            f"Spec patterns (viostor.expected_hardware_ids):\n{_format_bullets(blk_patterns)}\n"
            "\n"
            f"devices.cmd AERO_VIRTIO_BLK_HWIDS:\n{_format_bullets(devices.virtio_blk_hwids)}\n"
            "\n"
            "Remediation:\n"
            "- If the emulator/device contract changed (new PCI VEN/DEV IDs), update BOTH:\n"
            "  * guest-tools/config/devices.cmd (AERO_VIRTIO_BLK_HWIDS)\n"
            "  * tools/packaging/specs/win7-virtio-win.json (viostor.expected_hardware_ids)\n"
            "- Otherwise, fix the regex in win7-virtio-win.json so it matches the HWIDs used by Guest Tools.\n"
        )

    net_match = _find_first_match(net_patterns, devices.virtio_net_hwids)
    if not net_match:
        raise ValidationError(
            "Mismatch: win7-virtio-win.json expects netkvm HWIDs that don't match devices.cmd.\n"
            "\n"
            f"Spec patterns (netkvm.expected_hardware_ids):\n{_format_bullets(net_patterns)}\n"
            "\n"
            f"devices.cmd AERO_VIRTIO_NET_HWIDS:\n{_format_bullets(devices.virtio_net_hwids)}\n"
            "\n"
            "Remediation:\n"
            "- If the emulator/device contract changed (new PCI VEN/DEV IDs), update BOTH:\n"
            "  * guest-tools/config/devices.cmd (AERO_VIRTIO_NET_HWIDS)\n"
            "  * tools/packaging/specs/win7-virtio-win.json (netkvm.expected_hardware_ids)\n"
            "- Otherwise, fix the regex in win7-virtio-win.json so it matches the HWIDs used by Guest Tools.\n"
        )

    # Provide context in success output to make debugging CI failures easier.
    blk_pattern, blk_hwid = blk_match
    net_pattern, net_hwid = net_match
    print("Guest Tools config/spec validation: OK")
    print(f"- virtio-blk service : {devices.virtio_blk_service}")
    print(f"- viostor HWID match : {blk_pattern!r} matched {blk_hwid!r}")
    print(f"- netkvm HWID match  : {net_pattern!r} matched {net_hwid!r}")


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--devices-cmd",
        default=str(REPO_ROOT / "guest-tools/config/devices.cmd"),
        help="Path to guest-tools/config/devices.cmd (default: repo copy).",
    )
    parser.add_argument(
        "--spec",
        default=str(REPO_ROOT / "tools/packaging/specs/win7-virtio-win.json"),
        help="Path to packaging spec JSON (default: in-repo win7-virtio-win.json).",
    )
    args = parser.parse_args(list(argv))

    devices_path = _resolve_path(args.devices_cmd)
    spec_path = _resolve_path(args.spec)

    try:
        devices = load_devices_cmd(devices_path)
        spec_expected = load_packaging_spec_expected_hwids(spec_path)
        validate(devices, spec_path, spec_expected)
    except ValidationError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
