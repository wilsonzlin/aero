#!/usr/bin/env python3
"""
Lightweight consistency checker for Guest Tools config vs packaging specs.

Why this exists:
- `guest-tools/config/devices.cmd` drives boot-critical driver installation (service
  names + HWIDs).
- `tools/packaging/specs/*.json` drives `aero_packager` validation when building Guest
  Tools media (ISO/zip) from driver packages (either upstream virtio-win or the
  in-repo Aero drivers produced by CI).

If these drift (e.g. emulator PCI IDs change but only one side is updated), we can
produce Guest Tools media that fails to install the correct drivers (storage/network
and any optional devices that the selected packaging spec declares, e.g. input/audio).
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


@dataclass(frozen=True)
class SpecDriver:
    required: bool
    expected_hardware_ids: Tuple[str, ...]


@dataclass(frozen=True)
class ContractDevice:
    driver_service_name: str
    hardware_id_patterns: Tuple[str, ...]


def load_windows_device_contract(path: Path) -> Mapping[str, ContractDevice]:
    """
    Load the machine-readable Windows device contract (docs/windows-device-contract.json).

    We use this as the source of truth for boot-critical service names like virtio-blk:
    packaging specs intentionally focus on driver folder names + HWID regexes and do not
    encode Windows service names.
    """

    if not path.exists():
        raise ValidationError(f"Windows device contract not found: {path}")

    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise ValidationError(f"Failed to parse Windows device contract JSON: {path}\n{e}") from e

    devices = raw.get("devices")
    if not isinstance(devices, list):
        raise ValidationError(f"Windows device contract {path} must contain a list field 'devices'.")

    out: Dict[str, ContractDevice] = {}
    for entry in devices:
        if not isinstance(entry, dict):
            raise ValidationError(f"Windows device contract {path} contains a non-object device entry: {entry!r}")
        name = entry.get("device")
        service = entry.get("driver_service_name")
        hwids = entry.get("hardware_id_patterns")
        if not isinstance(name, str) or not name:
            raise ValidationError(f"Windows device contract {path} has a device entry missing valid 'device': {entry!r}")
        if not isinstance(service, str) or not service:
            raise ValidationError(
                f"Windows device contract {path} device {name!r} is missing a valid 'driver_service_name': {entry!r}"
            )
        if hwids is None:
            hwids = []
        if not isinstance(hwids, list) or not all(isinstance(x, str) for x in hwids):
            raise ValidationError(
                f"Windows device contract {path} device {name!r} has invalid/missing 'hardware_id_patterns' (expected list[str])."
            )

        out[name] = ContractDevice(driver_service_name=service, hardware_id_patterns=tuple(hwids))

    if not out:
        raise ValidationError(f"Windows device contract {path} contains no devices.")

    return out


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

    virtio_blk_service = vars_map["AERO_VIRTIO_BLK_SERVICE"].strip()
    if not virtio_blk_service:
        raise ValidationError(
            "devices.cmd AERO_VIRTIO_BLK_SERVICE is empty.\n"
            "Remediation: set AERO_VIRTIO_BLK_SERVICE to the storage driver's INF AddService name.\n"
            f"File: {path}"
        )

    return DevicesConfig(
        virtio_blk_service=virtio_blk_service,
        virtio_blk_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_BLK_HWIDS", "")),
        virtio_net_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_NET_HWIDS", "")),
        virtio_input_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_INPUT_HWIDS", "")),
        virtio_snd_hwids=_parse_quoted_list(vars_map.get("AERO_VIRTIO_SND_HWIDS", "")),
        aero_gpu_hwids=_parse_quoted_list(vars_map.get("AERO_GPU_HWIDS", "")),
    )


def load_packaging_spec(path: Path) -> Mapping[str, SpecDriver]:
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
    out: Dict[str, Dict[str, object]] = {}

    def merge_driver(*, name: str, required: bool, patterns: Sequence[str]) -> None:
        entry = out.setdefault(name, {"required": False, "patterns": []})
        entry["required"] = bool(entry["required"]) or required
        existing_patterns = entry["patterns"]
        assert isinstance(existing_patterns, list)
        for pattern in patterns:
            if pattern not in existing_patterns:
                existing_patterns.append(pattern)

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

            required = False
            if field == "drivers":
                required_value = entry.get("required")
                if not isinstance(required_value, bool):
                    raise ValidationError(
                        f"Spec {path} driver {name!r} has invalid/missing 'required' (expected boolean)."
                    )
                required = required_value
            elif field == "required_drivers":
                required = True

            if hwids is None:
                hwids = []
            if not isinstance(hwids, list) or not all(isinstance(x, str) for x in hwids):
                raise ValidationError(
                    f"Spec {path} driver {name!r} has invalid 'expected_hardware_ids' (expected list[str])."
                )

            merge_driver(name=name, required=required, patterns=hwids)

    add_entries("drivers")
    add_entries("required_drivers")

    if not out:
        raise ValidationError(f"Spec {path} must contain a list field 'drivers' or 'required_drivers'.")

    parsed: Dict[str, SpecDriver] = {}
    for name, entry in out.items():
        required_val = entry.get("required", False)
        patterns_val = entry.get("patterns", [])
        if not isinstance(required_val, bool) or not isinstance(patterns_val, list):
            raise ValidationError(f"Spec {path} contains an invalid driver entry for {name!r}.")
        if not all(isinstance(p, str) for p in patterns_val):
            raise ValidationError(
                f"Spec {path} driver {name!r} has invalid expected_hardware_ids (expected list[str])."
            )
        parsed[name] = SpecDriver(required=required_val, expected_hardware_ids=tuple(patterns_val))

    return parsed


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


def _find_unmatched_patterns(patterns: Sequence[str], hwids: Sequence[str]) -> List[str]:
    """Return spec patterns that match none of the configured HWIDs."""
    unmatched: List[str] = []
    for pattern in patterns:
        compiled = _compile_patterns([pattern])
        assert len(compiled) == 1
        regex = compiled[0]
        if not any(regex.search(hwid) for hwid in hwids):
            unmatched.append(pattern)
    return unmatched


def _normalize_hwid(hwid: str) -> str:
    return hwid.strip().lower()


def _find_missing_exact_patterns(patterns: Sequence[str], hwids: Sequence[str]) -> List[str]:
    hwid_set = {_normalize_hwid(h) for h in hwids}
    missing: List[str] = []
    for pattern in patterns:
        if _normalize_hwid(pattern) not in hwid_set:
            missing.append(pattern)
    return missing


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

    # Enforce that every "base" PCI HWID pattern in the spec is represented by at
    # least one HWID in devices.cmd.
    #
    # This catches regressions where the packager spec is updated to require
    # multiple PCI HWID families (for example: a device temporarily supports both
    # a new and legacy vendor/device ID), but devices.cmd only lists one of them.
    #
    # We intentionally *do not* require patterns that include SUBSYS/REV/class-code
    # qualifiers to match the `devices.cmd` list, since Guest Tools config usually
    # lists only the vendor/device pair.
    patterns_requiring_match = [
        p
        for p in patterns
        if re.search(r"(?i)(VEN_|VID_)", p)
        and re.search(r"(?i)(DEV_|DID_)", p)
        and not re.search(r"(?i)&SUBSYS_", p)
        and not re.search(r"(?i)&REV_", p)
        and not re.search(r"(?i)&CC_", p)
    ]
    unmatched_patterns = _find_unmatched_patterns(patterns_requiring_match, hwids)
    if unmatched_patterns:
        raise ValidationError(
            f"Mismatch: {spec_path.name} expects additional {driver_kind} HWID pattern(s) not present in devices.cmd.\n"
            "\n"
            f"Spec patterns ({driver_name}.expected_hardware_ids):\n{_format_bullets(patterns)}\n"
            "\n"
            f"Unmatched pattern(s):\n{_format_bullets(unmatched_patterns)}\n"
            "\n"
            f"devices.cmd {devices_var}:\n{_format_bullets(hwids)}\n"
            "\n"
            "Remediation:\n"
            f"- If the emulator/device contract supports multiple HWID families, include each supported HWID in {devices_var}.\n"
            f"- Otherwise, remove/adjust the extra pattern(s) in {spec_path.name}.\n"
        )

    return match


def validate(devices: DevicesConfig, spec_path: Path, spec_expected: Mapping[str, SpecDriver]) -> None:
    # Storage service name: `devices.cmd` must declare the storage driver's INF AddService
    # name so `guest-tools/setup.cmd` can preseed BOOT_START + CriticalDeviceDatabase keys.
    #
    # The in-repo Guest Tools config tracks the Aero device contract
    # (`docs/windows-device-contract.json`). When packaging Guest Tools from other driver
    # sets (e.g. virtio-win), wrapper scripts are responsible for patching the staged
    # `devices.cmd` to match the packaged INF(s).
    contract_path = REPO_ROOT / "docs/windows-device-contract.json"
    contract = load_windows_device_contract(contract_path)
    virtio_blk_contract = contract.get("virtio-blk")
    if virtio_blk_contract is None:
        raise ValidationError(f"Windows device contract {contract_path} is missing the required 'virtio-blk' entry.")
    expected_blk_service = virtio_blk_contract.driver_service_name
    if devices.virtio_blk_service.strip().lower() != expected_blk_service.strip().lower():
        raise ValidationError(
            "Mismatch: devices.cmd storage service name does not match the Windows device contract.\n"
            "\n"
            f"devices.cmd AERO_VIRTIO_BLK_SERVICE: {devices.virtio_blk_service!r}\n"
            f"windows-device-contract.json virtio-blk.driver_service_name: {expected_blk_service!r}\n"
            "\n"
            "Remediation:\n"
            "- Update guest-tools/config/devices.cmd (AERO_VIRTIO_BLK_SERVICE) to match the virtio-blk INF AddService name.\n"
            "- Update docs/windows-device-contract.json if the contract/service name intentionally changed.\n"
        )

    virtio_net_contract = contract.get("virtio-net")
    if virtio_net_contract is None:
        raise ValidationError(f"Windows device contract {contract_path} is missing the required 'virtio-net' entry.")

    def require_contract_hwids(
        *, device_name: str, devices_var: str, hwids: Tuple[str, ...], contract_hwids: Tuple[str, ...]
    ) -> None:
        if not contract_hwids:
            raise ValidationError(f"Windows device contract {contract_path} device {device_name!r} has no hardware_id_patterns.")
        missing_patterns = _find_missing_exact_patterns(contract_hwids, hwids)
        if missing_patterns:
            raise ValidationError(
                f"Mismatch: devices.cmd is missing {device_name} HWID(s) declared by the Windows device contract.\n"
                "\n"
                f"Contract: {contract_path}\n"
                f"Contract {device_name}.hardware_id_patterns:\n{_format_bullets(contract_hwids)}\n"
                "\n"
                f"devices.cmd {devices_var}:\n{_format_bullets(hwids)}\n"
                "\n"
                f"Missing from devices.cmd {devices_var}:\n{_format_bullets(missing_patterns)}\n"
                "\n"
                "Remediation:\n"
                "- If the emulator/device contract changed, update BOTH:\n"
                f"  * {contract_path} ({device_name}.hardware_id_patterns)\n"
                f"  * guest-tools/config/devices.cmd ({devices_var})\n"
                "- Otherwise, revert the contract change or fix devices.cmd so the boot-critical IDs are seeded correctly.\n"
            )

    # Validate boot-critical/early-boot device HWID lists against the contract. The packager specs
    # validate driver binding via regex, but do not encode subsystem/revision-specific IDs; those
    # are captured in the device contract and must stay in sync with the installer’s seeding list.
    require_contract_hwids(
        device_name="virtio-blk",
        devices_var="AERO_VIRTIO_BLK_HWIDS",
        hwids=devices.virtio_blk_hwids,
        contract_hwids=virtio_blk_contract.hardware_id_patterns,
    )
    require_contract_hwids(
        device_name="virtio-net",
        devices_var="AERO_VIRTIO_NET_HWIDS",
        hwids=devices.virtio_net_hwids,
        contract_hwids=virtio_net_contract.hardware_id_patterns,
    )

    # Some specs have an expected minimum set of drivers. Enforce that so the
    # validator fails loudly if someone accidentally edits the spec to remove a
    # boot-critical entry.
    if spec_path.name in ("win7-virtio-win.json", "win7-virtio-full.json"):
        required_groups = (("viostor",), ("netkvm",))
    elif spec_path.name == "win7-aero-guest-tools.json":
        # The in-repo driver folder name for AeroGPU is `aerogpu`, but keep
        # backwards-compatible aliases to avoid renames breaking CI history.
        required_groups = (("aerogpu", "aero-gpu"), ("virtio-blk",), ("virtio-net",), ("virtio-input",))
    else:
        required_groups = ()

    missing = ["/".join(group) for group in required_groups if not any(name in spec_expected for name in group)]
    if missing:
        raise ValidationError(
            f"Spec {spec_path} is missing required driver entries: {', '.join(missing)}\n"
            f"Remediation: update {spec_path} to include them."
        )
    matches: List[Tuple[str, Tuple[str, str]]] = []

    def maybe_validate(
        driver_name: str, *, devices_var: str, hwids: Tuple[str, ...], driver_kind: str
    ) -> None:
        drv = spec_expected.get(driver_name)
        if drv is None:
            return
        match = _validate_hwid_contract(
            spec_path=spec_path,
            driver_name=driver_name,
            patterns=drv.expected_hardware_ids,
            hwids=hwids,
            devices_var=devices_var,
            driver_kind=driver_kind,
        )
        matches.append((driver_name, match))

    maybe_validate(
        "viostor",
        devices_var="AERO_VIRTIO_BLK_HWIDS",
        hwids=devices.virtio_blk_hwids,
        driver_kind="virtio-blk",
    )
    maybe_validate(
        "netkvm",
        devices_var="AERO_VIRTIO_NET_HWIDS",
        hwids=devices.virtio_net_hwids,
        driver_kind="virtio-net",
    )
    # Upstream virtio-win naming.
    maybe_validate(
        "vioinput",
        devices_var="AERO_VIRTIO_INPUT_HWIDS",
        hwids=devices.virtio_input_hwids,
        driver_kind="virtio-input",
    )
    maybe_validate(
        "viosnd",
        devices_var="AERO_VIRTIO_SND_HWIDS",
        hwids=devices.virtio_snd_hwids,
        driver_kind="virtio-snd",
    )
    # Aero driver naming.
    maybe_validate(
        "virtio-input",
        devices_var="AERO_VIRTIO_INPUT_HWIDS",
        hwids=devices.virtio_input_hwids,
        driver_kind="virtio-input",
    )
    maybe_validate(
        "virtio-blk",
        devices_var="AERO_VIRTIO_BLK_HWIDS",
        hwids=devices.virtio_blk_hwids,
        driver_kind="virtio-blk",
    )
    maybe_validate(
        "virtio-net",
        devices_var="AERO_VIRTIO_NET_HWIDS",
        hwids=devices.virtio_net_hwids,
        driver_kind="virtio-net",
    )
    maybe_validate(
        "virtio-snd",
        devices_var="AERO_VIRTIO_SND_HWIDS",
        hwids=devices.virtio_snd_hwids,
        driver_kind="virtio-snd",
    )
    maybe_validate(
        "aero-gpu",
        devices_var="AERO_GPU_HWIDS",
        hwids=devices.aero_gpu_hwids,
        driver_kind="aero-gpu",
    )
    maybe_validate(
        "aerogpu",
        devices_var="AERO_GPU_HWIDS",
        hwids=devices.aero_gpu_hwids,
        driver_kind="aero-gpu",
    )

    if not matches:
        supported = [
            "viostor",
            "netkvm",
            "vioinput",
            "viosnd",
            "virtio-blk",
            "virtio-net",
            "virtio-input",
            "virtio-snd",
            "aero-gpu",
            "aerogpu",
        ]
        raise ValidationError(
            f"Spec {spec_path} does not contain any driver entries that this validator knows how to check.\n"
            f"Supported driver names: {', '.join(supported)}"
        )

    print("Guest Tools config/spec validation: OK")
    print(f"- spec: {spec_path}")
    print(f"- virtio-blk service : {devices.virtio_blk_service} (contract: {expected_blk_service})")
    for name, (pattern, hwid) in matches:
        print(f"- {name} HWID match : {pattern!r} matched {hwid!r}")


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
        spec_expected = load_packaging_spec(spec_path)
        validate(devices, spec_path, spec_expected)
    except ValidationError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
