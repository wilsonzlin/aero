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


def _compile_patterns(patterns: Sequence[str]) -> List[re.Pattern[str]]:
    compiled: List[re.Pattern[str]] = []
    for pattern in patterns:
        try:
            compiled.append(re.compile(pattern, re.IGNORECASE))
        except re.error as e:
            raise ValidationError(f"Invalid regex in packaging spec: {pattern!r}\n{e}") from e
    return compiled


def _find_first_match(patterns: Sequence[str], hwids: Sequence[str]) -> Tuple[str, str] | None:
    compiled = _compile_patterns(patterns)
    for regex in compiled:
        for hwid in hwids:
            if regex.search(hwid):
                return regex.pattern, hwid
    return None


def _format_bullets(items: Iterable[str]) -> str:
    return "\n".join(f"  - {item}" for item in items)


def _find_uncovered_hwids(patterns: Sequence[str], hwids: Sequence[str]) -> List[str]:
    compiled = _compile_patterns(patterns)
    uncovered: List[str] = []
    for hwid in hwids:
        if not any(regex.search(hwid) for regex in compiled):
            uncovered.append(hwid)
    return uncovered


def _validate_hwid_contract(
    *,
    spec_path: Path,
    driver_name: str,
    patterns: Sequence[str],
    hwids: Sequence[str],
    devices_var: str,
    driver_kind: str,
) -> Tuple[str, str]:
    """
    Validate that:
    - there is at least one regex match between `patterns` and `hwids`
    - every HWID in `hwids` is covered by at least one pattern

    Returns (pattern, hwid) for the first match, for debug output.
    """

    if not hwids:
        raise ValidationError(
            f"{devices_var} is empty.\n"
            f"Remediation: set {devices_var} in guest-tools/config/devices.cmd to the emulator-presented PCI HWIDs for {driver_kind}."
        )

    if not patterns:
        raise ValidationError(
            f"Spec {spec_path} driver {driver_name!r} has an empty expected_hardware_ids list.\n"
            f"Remediation: add the {driver_kind} PCI HWID regex(es) under {driver_name}.expected_hardware_ids."
        )

    match = _find_first_match(patterns, hwids)
    if not match:
        raise ValidationError(
            f"Mismatch: {spec_path.name} expects {driver_name} HWIDs that don't match devices.cmd.\n"
            f"Spec: {spec_path}\n"
            "\n"
            f"Spec patterns ({driver_name}.expected_hardware_ids):\n{_format_bullets(patterns)}\n"
            "\n"
            f"devices.cmd {devices_var}:\n{_format_bullets(hwids)}\n"
            "\n"
            "Remediation:\n"
            "- If the emulator/device contract changed (new PCI VEN/DEV IDs), update BOTH:\n"
            f"  * guest-tools/config/devices.cmd ({devices_var})\n"
            f"  * {spec_path} ({driver_name}.expected_hardware_ids)\n"
            f"- Otherwise, fix the regex in {spec_path.name} so it matches the HWIDs used by Guest Tools.\n"
        )

    uncovered = _find_uncovered_hwids(patterns, hwids)
    if uncovered:
        raise ValidationError(
            f"Mismatch: devices.cmd contains {driver_kind} HWIDs not covered by {spec_path.name}.\n"
            "\n"
            f"Spec patterns ({driver_name}.expected_hardware_ids):\n{_format_bullets(patterns)}\n"
            "\n"
            f"Uncovered devices.cmd {devices_var}:\n{_format_bullets(uncovered)}\n"
            "\n"
            "Remediation:\n"
            f"- If the emulator/device contract changed, expand {driver_name}.expected_hardware_ids in the spec to cover the new IDs.\n"
            f"- If devices.cmd is wrong/out-of-date, update {devices_var} to match the supported IDs.\n"
        )

    return match


def validate(devices: DevicesConfig, spec_path: Path, spec_expected: Mapping[str, Tuple[str, ...]]) -> None:
    missing = [name for name in ("viostor", "netkvm") if name not in spec_expected]
    if missing:
        raise ValidationError(
            f"Spec {spec_path} is missing required driver entries: {', '.join(missing)}\n"
            f"Remediation: update {spec_path} to include them."
        )

    if devices.virtio_blk_service.strip().lower() != "viostor":
        raise ValidationError(
            "Mismatch: devices.cmd storage service name does not match the virtio-win storage driver.\n"
            "\n"
            f"devices.cmd AERO_VIRTIO_BLK_SERVICE: {devices.virtio_blk_service!r}\n"
            "Expected (virtio-win): 'viostor'\n"
            "\n"
            "Remediation:\n"
            "- If you are using the upstream virtio-win storage driver, set:\n"
            "    set \"AERO_VIRTIO_BLK_SERVICE=viostor\"\n"
            "- If you intentionally changed the storage miniport/service name, ensure the INF AddService name,\n"
            "  Guest Tools preseed (devices.cmd), and packaging inputs are all updated together.\n"
        )

    blk_match = _validate_hwid_contract(
        spec_path=spec_path,
        driver_name="viostor",
        patterns=spec_expected["viostor"],
        hwids=devices.virtio_blk_hwids,
        devices_var="AERO_VIRTIO_BLK_HWIDS",
        driver_kind="virtio-blk",
    )
    net_match = _validate_hwid_contract(
        spec_path=spec_path,
        driver_name="netkvm",
        patterns=spec_expected["netkvm"],
        hwids=devices.virtio_net_hwids,
        devices_var="AERO_VIRTIO_NET_HWIDS",
        driver_kind="virtio-net",
    )

    optional_matches: List[Tuple[str, Tuple[str, str]]] = []
    if "vioinput" in spec_expected and devices.virtio_input_hwids:
        optional_matches.append(
            (
                "vioinput",
                _validate_hwid_contract(
                    spec_path=spec_path,
                    driver_name="vioinput",
                    patterns=spec_expected["vioinput"],
                    hwids=devices.virtio_input_hwids,
                    devices_var="AERO_VIRTIO_INPUT_HWIDS",
                    driver_kind="virtio-input",
                ),
            )
        )
    if "viosnd" in spec_expected and devices.virtio_snd_hwids:
        optional_matches.append(
            (
                "viosnd",
                _validate_hwid_contract(
                    spec_path=spec_path,
                    driver_name="viosnd",
                    patterns=spec_expected["viosnd"],
                    hwids=devices.virtio_snd_hwids,
                    devices_var="AERO_VIRTIO_SND_HWIDS",
                    driver_kind="virtio-snd",
                ),
            )
            )
        )

    # Provide context in success output to make debugging CI failures easier.
    blk_pattern, blk_hwid = blk_match
    net_pattern, net_hwid = net_match
    print("Guest Tools config/spec validation: OK")
    print(f"- virtio-blk service : {devices.virtio_blk_service}")
    print(f"- viostor HWID match : {blk_pattern!r} matched {blk_hwid!r}")
    print(f"- netkvm HWID match  : {net_pattern!r} matched {net_hwid!r}")
    for name, (pattern, hwid) in optional_matches:
        print(f"- {name} HWID match  : {pattern!r} matched {hwid!r}")


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
