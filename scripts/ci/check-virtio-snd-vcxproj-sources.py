#!/usr/bin/env python3
"""
Guardrail: prevent the Windows 7 virtio-snd MSBuild project from silently drifting
back to the legacy virtio-pci I/O-port backend.

Why this exists:
  - `drivers/windows7/virtio-snd` contains both legacy (I/O-port) and modern
    (virtio-pci modern + MMIO) transport implementations.
  - MSBuild will happily succeed even if the project accidentally compiles the
    legacy transport sources, but the resulting `aero_virtio_snd.sys` cannot start
    against Aero's modern virtio device contract.

This check parses `aero_virtio_snd.vcxproj` and enforces:
  - Required modern transport sources are included.
  - Known legacy transport sources are NOT included.
  - The project output name matches the shipped INF's NTMPDriver (`aero_virtio_snd.sys`).

If files are renamed during migration, update REQUIRED_SOURCES / FORBIDDEN_*.
"""

from __future__ import annotations

import sys
import xml.etree.ElementTree as ET
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

VCXPROJ = REPO_ROOT / "drivers/windows7/virtio-snd/aero_virtio_snd.vcxproj"
AERO_INF = REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf"
LEGACY_INF = REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero-virtio-snd-legacy.inf"

# virtio-snd ships with a primary, strict Aero contract INF (`aero_virtio_snd.inf`).
# A legacy filename alias INF may optionally be present as `virtio-snd.inf`. In
# this repo it may be checked in as `virtio-snd.inf.disabled` to avoid accidental
# packaging; treat the alias as best-effort and always validate the Aero INF.
LEGACY_ALIAS_INF = REPO_ROOT / "drivers/windows7/virtio-snd/inf/virtio-snd.inf"
LEGACY_ALIAS_INF_DISABLED = (
    REPO_ROOT / "drivers/windows7/virtio-snd/inf/virtio-snd.inf.disabled"
)

# These are repo-root-relative paths.
REQUIRED_REPO_SOURCES = {
    "drivers/windows7/virtio-snd/src/virtiosnd_hw.c",
    "drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c",
    "drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c",
    "drivers/win7/virtio/virtio-core/portable/virtio_pci_identity.c",
    "drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c",
    "drivers/windows7/virtio/common/src/virtio_pci_contract.c",
    "drivers/windows/virtio/common/virtqueue_split.c",
    "drivers/windows7/virtio-snd/src/virtiosnd_intx.c",
    "drivers/windows7/virtio-snd/src/virtiosnd_control.c",
    "drivers/windows7/virtio-snd/src/virtiosnd_tx.c",
    "drivers/windows7/virtio-snd/src/backend_virtio.c",
}

# These are project-relative paths (relative to drivers/windows7/virtio-snd/).
FORBIDDEN_PROJECT_SOURCES = {
    "src/aero_virtio_snd_ioport_hw.c",
    "src/backend_virtio_legacy.c",
}

# These are repo-root-relative paths.
FORBIDDEN_REPO_SOURCES = {
    "drivers/windows7/virtio/common/src/virtio_pci_legacy.c",
}

# Forbidden by basename/suffix (regardless of where it comes from).
FORBIDDEN_BASENAMES = {
    "virtio_queue.c",
}


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def warn(message: str) -> None:
    print(f"warning: {message}", file=sys.stderr)


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def iter_inf_non_comment_lines(text: str) -> list[str]:
    """
    Return non-empty, non-comment INF lines.

    INF syntax treats ';' as a comment delimiter. This helper strips trailing
    comments so guardrails don't trigger on documentation text.
    """

    out: list[str] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith(";"):
            continue
        if ";" in line:
            line = line.split(";", 1)[0].strip()
        if line:
            out.append(line)
    return out


def validate_virtio_snd_inf_hwid_policy(inf_path: Path) -> None:
    """
    Enforce Aero virtio-snd INF HWID policy (contract v1 strict identity).

    The runtime driver enforces contract identity (`PCI\\VEN_1AF4&DEV_1059&REV_01`), so letting
    an INF bind to transitional or non-revision-gated IDs creates confusing
    "installs but won't start (Code 10)" behavior.
    """

    text = read_text(inf_path)
    lines = iter_inf_non_comment_lines(text)

    for line in lines:
        upper = line.upper()

        if "PCI\\VEN_1AF4&DEV_1018" in upper:
            fail(
                f"virtio-snd INF must not match transitional PCI\\VEN_1AF4&DEV_1018: {inf_path.as_posix()}\n"
                f"  offending line: {line}"
            )

        if "PCI\\VEN_1AF4&DEV_1059" in upper and "&REV_01" not in upper:
            fail(
                f"virtio-snd INF must gate PCI\\VEN_1AF4&DEV_1059 by REV_01: {inf_path.as_posix()}\n"
                f"  offending line: {line}"
            )


def validate_virtio_snd_legacy_inf_policy(inf_path: Path) -> None:
    """
    Enforce the opt-in transitional/QEMU virtio-snd INF policy.

    Requirements:
      - Must match transitional PCI\\VEN_1AF4&DEV_1018 (without requiring REV_01).
      - Must NOT match the Aero contract-v1 modern ID (PCI\\VEN_1AF4&DEV_1059) to avoid HWID overlap with the
        shipped contract package.
      - Must reference wdmaud.sys (wdmaudio.sys is a common typo).
    """

    text = read_text(inf_path)
    lines = iter_inf_non_comment_lines(text)

    has_transitional = False
    has_wdmaud = False

    for line in lines:
        upper = line.upper()

        if "PCI\\VEN_1AF4&DEV_1059" in upper:
            fail(
                f"virtio-snd legacy INF must not match Aero contract PCI\\VEN_1AF4&DEV_1059: {inf_path.as_posix()}\n"
                f"  offending line: {line}"
            )

        if "PCI\\VEN_1AF4&DEV_1018" in upper:
            has_transitional = True
            if "&REV_01" in upper:
                fail(
                    f"virtio-snd legacy INF must not require REV_01: {inf_path.as_posix()}\n"
                    f"  offending line: {line}"
                )

        if "WDMAUDIO.SYS" in upper:
            fail(
                f"virtio-snd legacy INF references wdmaudio.sys (typo; expected wdmaud.sys): {inf_path.as_posix()}\n"
                f"  offending line: {line}"
            )
        if "WDMAUD.SYS" in upper:
            has_wdmaud = True

    if not has_transitional:
        fail(
            f"virtio-snd legacy INF must match transitional PCI\\VEN_1AF4&DEV_1018: {inf_path.as_posix()}\n"
            "  expected a line containing: PCI\\VEN_1AF4&DEV_1018"
        )
    if not has_wdmaud:
        fail(
            f"virtio-snd legacy INF missing wdmaud.sys reference: {inf_path.as_posix()}\n"
            "  expected a line containing: wdmaud.sys"
        )


def normalize_path(value: str) -> str:
    # MSBuild projects are Windows-first and typically use backslashes. Normalize
    # for stable comparisons in CI (Linux).
    return value.strip().replace("\\", "/")


def expand_msbuild_path(raw: str, project_dir: Path) -> set[Path]:
    """
    Resolve a ClCompile Include/Remove value to concrete paths.

    This intentionally supports only the subset we expect in-tree:
      - relative paths
      - simple globbing (*, ?, **)
      - $(ProjectDir) prefix
    """

    value = normalize_path(raw)

    if value.startswith("$(ProjectDir)"):
        # MSBuild's $(ProjectDir) includes a trailing slash.
        value = str(project_dir.as_posix()) + "/" + value[len("$(ProjectDir)") :].lstrip("/")

    if "$(" in value:
        fail(f"unsupported MSBuild macro in ClCompile entry: {raw!r}")

    if any(ch in value for ch in ["*", "?", "["]):
        # Path.glob is relative to the base directory; if the pattern is absolute
        # (unlikely here), fall back to the directory portion.
        if Path(value).is_absolute():
            base = Path(value).parent
            pattern = Path(value).name
        else:
            base = project_dir
            pattern = value
        return {p.resolve() for p in base.glob(pattern) if p.is_file()}

    path = Path(value)
    if not path.is_absolute():
        path = project_dir / path
    return {path.resolve()}


def parse_vcxproj_compiled_sources(vcxproj: Path) -> tuple[set[str], set[str], set[Path]]:
    """
    Returns:
      - project-relative POSIX paths
      - repo-root-relative POSIX paths (for entries within the repo)
      - absolute paths (best effort; may include paths outside repo)
    """

    project_dir = vcxproj.parent

    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    include_raw: list[str] = []
    remove_raw: list[str] = []

    for elem in root.findall(".//{*}ClCompile"):
        if "Include" in elem.attrib:
            include_raw.append(elem.attrib["Include"])
        if "Remove" in elem.attrib:
            remove_raw.append(elem.attrib["Remove"])

    if not include_raw:
        fail(f"no <ClCompile Include=...> items found in {vcxproj.as_posix()}")

    included: set[Path] = set()
    for raw in include_raw:
        paths = expand_msbuild_path(raw, project_dir)
        if not paths:
            fail(f"ClCompile Include matched no files: {raw!r}")
        included |= paths

    removed: set[Path] = set()
    for raw in remove_raw:
        removed |= expand_msbuild_path(raw, project_dir)

    compiled = {p for p in included if p not in removed}

    project_rel: set[str] = set()
    repo_rel: set[str] = set()

    for p in compiled:
        try:
            project_rel.add(p.relative_to(project_dir).as_posix())
        except ValueError:
            # Unlikely, but keep going; repo_rel will also likely fail.
            pass

        try:
            repo_rel.add(p.relative_to(REPO_ROOT).as_posix())
        except ValueError:
            pass

    return project_rel, repo_rel, compiled


def parse_vcxproj_output_name(vcxproj: Path) -> str:
    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    target_name_elem = root.find(".//{*}TargetName")
    target_ext_elem = root.find(".//{*}TargetExt")

    if target_name_elem is None or not (target_name := (target_name_elem.text or "").strip()):
        fail(f"missing <TargetName> in {vcxproj.as_posix()}")

    target_ext = ".sys"
    if target_ext_elem is not None and (text := (target_ext_elem.text or "").strip()):
        target_ext = text
    if not target_ext.startswith("."):
        target_ext = "." + target_ext

    return f"{target_name}{target_ext}"


def vcxproj_has_target_name(vcxproj: Path, expected: str) -> bool:
    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    for elem in root.findall(".//{*}TargetName"):
        if (elem.text or "").strip().lower() == expected.lower():
            return True
    return False


def extract_inf_ntmpdriver(inf_path: Path) -> str:
    # Match `HKR,,NTMPDriver,,aero_virtio_snd.sys` and tolerate
    # optional flags or quoted strings.
    for line in read_text(inf_path).splitlines():
        # Strip inline comments.
        line = line.split(";", 1)[0].strip()
        if not line:
            continue
        if "NTMPDriver" not in line:
            continue

        fields = [f.strip() for f in line.split(",")]
        if len(fields) < 3:
            continue
        if fields[0].upper() != "HKR":
            continue
        if fields[2].upper() != "NTMPDRIVER":
            continue

        # Value is typically the last field.
        value = fields[-1].strip().strip('"')
        if value:
            return value

    raise ValueError(f"could not find NTMPDriver registry value in {inf_path.as_posix()}")


def extract_inf_ntmpdriver_required(inf_path: Path) -> str:
    try:
        return extract_inf_ntmpdriver(inf_path)
    except FileNotFoundError:
        fail(f"missing required file: {inf_path.as_posix()}")
        raise AssertionError("unreachable")
    except ValueError as e:
        fail(str(e))
        raise AssertionError("unreachable")


def extract_inf_ntmpdriver_optional(inf_path: Path) -> str | None:
    if not inf_path.exists():
        return None
    try:
        return extract_inf_ntmpdriver(inf_path)
    except ValueError as e:
        warn(str(e))
        return None


def main() -> None:
    project_rel, repo_rel, compiled_paths = parse_vcxproj_compiled_sources(VCXPROJ)

    missing = sorted(p for p in REQUIRED_REPO_SOURCES if p not in repo_rel)
    if missing:
        fail(
            "aero_virtio_snd.vcxproj is missing required modern sources:\n"
            + "\n".join(f"  - {p}" for p in missing)
        )

    forbidden_found: list[str] = []
    for p in sorted(FORBIDDEN_PROJECT_SOURCES):
        if p in project_rel:
            forbidden_found.append(p)
    for p in sorted(FORBIDDEN_REPO_SOURCES):
        if p in repo_rel:
            forbidden_found.append(p)
    for abs_path in sorted(compiled_paths, key=lambda p: p.as_posix()):
        if abs_path.name in FORBIDDEN_BASENAMES:
            try:
                forbidden_found.append(abs_path.relative_to(REPO_ROOT).as_posix())
            except ValueError:
                forbidden_found.append(abs_path.as_posix())

    if forbidden_found:
        fail(
            "aero_virtio_snd.vcxproj includes forbidden legacy transport sources:\n"
            + "\n".join(f"  - {p}" for p in forbidden_found)
        )

    # Ensure the produced SYS name matches the INF's NTMPDriver.
    output_name = parse_vcxproj_output_name(VCXPROJ)
    expected = extract_inf_ntmpdriver_required(AERO_INF)
    validate_virtio_snd_inf_hwid_policy(AERO_INF)
    if output_name.lower() != expected.lower():
        fail(
            "virtio-snd output name mismatch between MSBuild project and INF:\n"
            f"  {VCXPROJ.as_posix()}: {output_name}\n"
            f"  {AERO_INF.as_posix()}: NTMPDriver={expected}"
        )

    # Validate the opt-in transitional/QEMU package (aero-virtio-snd-legacy.inf).
    # This is intentionally not part of the default CI driver bundle, but should remain
    # buildable/installable for QEMU bring-up without overlapping the contract HWIDs.
    legacy_ntmp = extract_inf_ntmpdriver_required(LEGACY_INF)
    validate_virtio_snd_legacy_inf_policy(LEGACY_INF)
    if legacy_ntmp.lower() != "virtiosnd_legacy.sys":
        fail(
            "virtio-snd legacy INF must install virtiosnd_legacy.sys:\n"
            f"  {LEGACY_INF.as_posix()}: NTMPDriver={legacy_ntmp}"
        )
    if not vcxproj_has_target_name(VCXPROJ, "virtiosnd_legacy"):
        fail(
            "aero_virtio_snd.vcxproj missing Legacy build target name "
            "(expected <TargetName>virtiosnd_legacy</TargetName>)\n"
            f"  file: {VCXPROJ.as_posix()}"
        )

    # Best-effort: keep the legacy alias INF (if present) in sync with the shipped
    # Aero INF. In-tree this may be stored as `virtio-snd.inf.disabled`, so this
    # must not be a hard requirement.
    legacy_expected = expected.lower()
    legacy_checked = False

    if (name := extract_inf_ntmpdriver_optional(LEGACY_ALIAS_INF)) is not None:
        legacy_checked = True
        validate_virtio_snd_inf_hwid_policy(LEGACY_ALIAS_INF)
        if name.lower() != legacy_expected:
            fail(
                "virtio-snd legacy alias INF disagrees on NTMPDriver:\n"
                f"  {LEGACY_ALIAS_INF.as_posix()}: NTMPDriver={name}\n"
                f"  {AERO_INF.as_posix()}: NTMPDriver={expected}"
            )

    elif (name := extract_inf_ntmpdriver_optional(LEGACY_ALIAS_INF_DISABLED)) is not None:
        legacy_checked = True
        validate_virtio_snd_inf_hwid_policy(LEGACY_ALIAS_INF_DISABLED)
        if name.lower() != legacy_expected:
            fail(
                "virtio-snd legacy alias INF (.disabled) disagrees on NTMPDriver:\n"
                f"  {LEGACY_ALIAS_INF_DISABLED.as_posix()}: NTMPDriver={name}\n"
                f"  {AERO_INF.as_posix()}: NTMPDriver={expected}"
            )

    if not legacy_checked and LEGACY_ALIAS_INF_DISABLED.exists():
        # File exists but didn't pass best-effort validation (e.g. missing
        # NTMPDriver). Keep this as a warning so the guardrail doesn't brick when
        # the alias INF is intentionally not shipped.
        warn(
            f"legacy alias INF present but not validated: {LEGACY_ALIAS_INF_DISABLED.as_posix()}"
        )


if __name__ == "__main__":
    main()
