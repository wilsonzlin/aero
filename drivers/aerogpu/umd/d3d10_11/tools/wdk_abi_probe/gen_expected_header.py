#!/usr/bin/env python3
"""
Generate `aerogpu_d3d10_11_wdk_abi_expected.h` from `wdk_abi_probe` output.

This script exists to keep the checked-in ABI expectations in sync with the
canonical Win7 D3D10/D3D11 UMDDI header set used for building the UMD.

Usage (from repo root, after building/running the probe for both arches):

  python3 drivers/aerogpu/umd/d3d10_11/tools/wdk_abi_probe/gen_expected_header.py \\
    --x86 out_x86.txt \\
    --x64 out_x64.txt \\
    --out drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_wdk_abi_expected.h
"""

from __future__ import annotations

import argparse
import datetime as _dt
import pathlib
import re
import sys
from dataclasses import dataclass


_SIZEOF_RE = re.compile(r"^sizeof\((?P<type>[^)]+)\)\s*=\s*(?P<value>\d+)\s*$", re.MULTILINE)
_OFFSETOF_RE = re.compile(
    r"^\s*offsetof\((?P<type>[^,]+),\s*(?P<member>[^)]+)\)\s*=\s*(?P<value>\d+)\s*$",
    re.MULTILINE,
)
_OFFSETOF_NA_RE = re.compile(
    r"^\s*offsetof\((?P<type>[^,]+),\s*(?P<member>[^)]+)\)\s*=\s*<n/a>\s*$",
    re.MULTILINE,
)
_EXPORT_RE = re.compile(
    r"^OpenAdapter(?P<which>10_2|10|11)\s*=>\s*_[A-Za-z0-9_]+@(?P<bytes>\d+)\s*$",
    re.MULTILINE,
)
_MSC_VER_RE = re.compile(r"^_MSC_VER\s*=\s*(?P<value>\d+)\s*$", re.MULTILINE)
_UMD_IFACE_VER_RE = re.compile(r"^D3D_UMD_INTERFACE_VERSION\s*=\s*(?P<value>\d+)\s*$", re.MULTILINE)


@dataclass(frozen=True)
class ProbeData:
    sizeof: dict[str, int]
    offsetof: dict[tuple[str, str], int]
    offsetof_na: set[tuple[str, str]]
    exports: dict[str, int]
    msc_ver: int | None
    d3d_umd_interface_version: int | None


def _parse_probe_output(text: str) -> ProbeData:
    sizeof: dict[str, int] = {}
    offsetof: dict[tuple[str, str], int] = {}
    offsetof_na: set[tuple[str, str]] = set()
    exports: dict[str, int] = {}

    for m in _SIZEOF_RE.finditer(text):
        sizeof[m.group("type").strip()] = int(m.group("value"))

    for m in _OFFSETOF_RE.finditer(text):
        offsetof[(m.group("type").strip(), m.group("member").strip())] = int(m.group("value"))

    for m in _OFFSETOF_NA_RE.finditer(text):
        offsetof_na.add((m.group("type").strip(), m.group("member").strip()))

    for m in _EXPORT_RE.finditer(text):
        exports[m.group("which")] = int(m.group("bytes"))

    m_msc = _MSC_VER_RE.search(text)
    m_iface = _UMD_IFACE_VER_RE.search(text)
    return ProbeData(
        sizeof=sizeof,
        offsetof=offsetof,
        offsetof_na=offsetof_na,
        exports=exports,
        msc_ver=int(m_msc.group("value")) if m_msc else None,
        d3d_umd_interface_version=int(m_iface.group("value")) if m_iface else None,
    )


def _get_size(data: ProbeData, type_name: str) -> int:
    try:
        return data.sizeof[type_name]
    except KeyError:
        raise SystemExit(f"Missing sizeof({type_name}) in probe output")


def _get_off(data: ProbeData, type_name: str, member: str) -> int:
    key = (type_name, member)
    try:
        return data.offsetof[key]
    except KeyError:
        raise SystemExit(f"Missing offsetof({type_name}, {member}) in probe output")


def _get_off_opt(data: ProbeData, type_name: str, member: str, *, allow_na: bool) -> int | None:
    key = (type_name, member)
    if key in data.offsetof:
        return data.offsetof[key]
    if key in data.offsetof_na:
        if allow_na:
            return None
        raise SystemExit(f"offsetof({type_name}, {member}) was reported as <n/a>; pass --allow-na to skip optional offsets")
    raise SystemExit(f"Missing offsetof({type_name}, {member}) in probe output")


def _get_export_bytes(data: ProbeData, which: str) -> int:
    try:
        return data.exports[which]
    except KeyError:
        raise SystemExit(f"Missing x86 export decoration for OpenAdapter{which} in probe output")


def _emit_header(x86: ProbeData, x64: ProbeData, *, allow_na: bool) -> str:
    # Use a timezone-aware UTC timestamp; `utcnow()` is deprecated in newer
    # Python versions.
    now = _dt.datetime.now(tz=_dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

    def emit_arch_block(data: ProbeData, *, include_exports: bool) -> list[str]:
        out: list[str] = []

        if include_exports:
            out.extend(
                [
                    "  // Exported entrypoints (x86 stdcall decoration).",
                    f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_STDCALL_BYTES {_get_export_bytes(data, '10')}",
                    f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER10_2_STDCALL_BYTES {_get_export_bytes(data, '10_2')}",
                    f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OPENADAPTER11_STDCALL_BYTES {_get_export_bytes(data, '11')}",
                    "",
                ]
            )

        # OpenAdapter arg struct (runtime -> UMD).
        out.append("  // OpenAdapter arg struct (runtime -> UMD).")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDIARG_OPENADAPTER {_get_size(data, 'D3D10DDIARG_OPENADAPTER')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Interface {_get_off(data, 'D3D10DDIARG_OPENADAPTER', 'Interface')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_Version {_get_off(data, 'D3D10DDIARG_OPENADAPTER', 'Version')}"
        )
        off = _get_off_opt(data, "D3D10DDIARG_OPENADAPTER", "hRTAdapter", allow_na=allow_na)
        if off is not None:
            out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hRTAdapter {off}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_hAdapter {_get_off(data, 'D3D10DDIARG_OPENADAPTER', 'hAdapter')}"
        )
        off = _get_off_opt(data, "D3D10DDIARG_OPENADAPTER", "pAdapterCallbacks", allow_na=allow_na)
        if off is not None:
            out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterCallbacks {off}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDIARG_OPENADAPTER_pAdapterFuncs {_get_off(data, 'D3D10DDIARG_OPENADAPTER', 'pAdapterFuncs')}"
        )
        out.append("")

        # Adapter function tables (UMD -> runtime).
        out.append("  // Adapter function tables (UMD -> runtime).")
        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_ADAPTERFUNCS {_get_size(data, 'D3D10DDI_ADAPTERFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnGetCaps {_get_off(data, 'D3D10DDI_ADAPTERFUNCS', 'pfnGetCaps')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize {_get_off(data, 'D3D10DDI_ADAPTERFUNCS', 'pfnCalcPrivateDeviceSize')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCreateDevice {_get_off(data, 'D3D10DDI_ADAPTERFUNCS', 'pfnCreateDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_ADAPTERFUNCS_pfnCloseAdapter {_get_off(data, 'D3D10DDI_ADAPTERFUNCS', 'pfnCloseAdapter')}"
        )
        out.append("")

        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_ADAPTERFUNCS {_get_size(data, 'D3D10_1DDI_ADAPTERFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnGetCaps {_get_off(data, 'D3D10_1DDI_ADAPTERFUNCS', 'pfnGetCaps')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize {_get_off(data, 'D3D10_1DDI_ADAPTERFUNCS', 'pfnCalcPrivateDeviceSize')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCreateDevice {_get_off(data, 'D3D10_1DDI_ADAPTERFUNCS', 'pfnCreateDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_ADAPTERFUNCS_pfnCloseAdapter {_get_off(data, 'D3D10_1DDI_ADAPTERFUNCS', 'pfnCloseAdapter')}"
        )
        out.append("")

        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_ADAPTERFUNCS {_get_size(data, 'D3D11DDI_ADAPTERFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnGetCaps {_get_off(data, 'D3D11DDI_ADAPTERFUNCS', 'pfnGetCaps')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceSize {_get_off(data, 'D3D11DDI_ADAPTERFUNCS', 'pfnCalcPrivateDeviceSize')}"
        )
        off = _get_off_opt(data, "D3D11DDI_ADAPTERFUNCS", "pfnCalcPrivateDeviceContextSize", allow_na=allow_na)
        if off is not None:
            out.append(
                f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCalcPrivateDeviceContextSize {off}"
            )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCreateDevice {_get_off(data, 'D3D11DDI_ADAPTERFUNCS', 'pfnCreateDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_ADAPTERFUNCS_pfnCloseAdapter {_get_off(data, 'D3D11DDI_ADAPTERFUNCS', 'pfnCloseAdapter')}"
        )
        out.append("")

        # Device function tables (UMD -> runtime).
        out.append("  // Device function tables (UMD -> runtime).")
        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10DDI_DEVICEFUNCS {_get_size(data, 'D3D10DDI_DEVICEFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnDestroyDevice {_get_off(data, 'D3D10DDI_DEVICEFUNCS', 'pfnDestroyDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_pfnCreateResource {_get_off(data, 'D3D10DDI_DEVICEFUNCS', 'pfnCreateResource')}"
        )
        for member in ("pfnPresent", "pfnFlush", "pfnRotateResourceIdentities"):
            off = _get_off_opt(data, "D3D10DDI_DEVICEFUNCS", member, allow_na=allow_na)
            if off is not None:
                out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10DDI_DEVICEFUNCS_{member} {off}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D10_1DDI_DEVICEFUNCS {_get_size(data, 'D3D10_1DDI_DEVICEFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnDestroyDevice {_get_off(data, 'D3D10_1DDI_DEVICEFUNCS', 'pfnDestroyDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_pfnCreateResource {_get_off(data, 'D3D10_1DDI_DEVICEFUNCS', 'pfnCreateResource')}"
        )
        for member in ("pfnPresent", "pfnFlush", "pfnRotateResourceIdentities"):
            off = _get_off_opt(data, "D3D10_1DDI_DEVICEFUNCS", member, allow_na=allow_na)
            if off is not None:
                out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D10_1DDI_DEVICEFUNCS_{member} {off}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICEFUNCS {_get_size(data, 'D3D11DDI_DEVICEFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnDestroyDevice {_get_off(data, 'D3D11DDI_DEVICEFUNCS', 'pfnDestroyDevice')}"
        )
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_pfnCreateResource {_get_off(data, 'D3D11DDI_DEVICEFUNCS', 'pfnCreateResource')}"
        )
        for member in ("pfnPresent", "pfnRotateResourceIdentities"):
            off = _get_off_opt(data, "D3D11DDI_DEVICEFUNCS", member, allow_na=allow_na)
            if off is not None:
                out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICEFUNCS_{member} {off}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_SIZEOF_D3D11DDI_DEVICECONTEXTFUNCS {_get_size(data, 'D3D11DDI_DEVICECONTEXTFUNCS')}")
        out.append(
            f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_pfnVsSetShader {_get_off(data, 'D3D11DDI_DEVICECONTEXTFUNCS', 'pfnVsSetShader')}"
        )
        for member in ("pfnDraw", "pfnFlush", "pfnPresent", "pfnRotateResourceIdentities"):
            off = _get_off_opt(data, "D3D11DDI_DEVICECONTEXTFUNCS", member, allow_na=allow_na)
            if off is not None:
                out.append(f"  #define AEROGPU_D3D10_11_WDK_ABI_EXPECT_OFFSETOF_D3D11DDI_DEVICECONTEXTFUNCS_{member} {off}")
        out.append("")

        return out

    lines: list[str] = []
    lines.extend(
        [
            "// Expected Win7 D3D10/11 UMD ABI values when building against the Windows WDK DDI",
            "// headers (d3dumddi.h / d3d10umddi.h / d3d10_1umddi.h / d3d11umddi.h).",
            "//",
            "// This file is generated from `tools/wdk_abi_probe` output. See:",
            "//   drivers/aerogpu/umd/d3d10_11/tools/wdk_abi_probe/README.md",
            f"// Generated: {now}",
            "//",
            "// clang-format off",
            "// clang-format on",
            "",
            "#pragma once",
            "",
            "#if !(defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)",
            '  #error "aerogpu_d3d10_11_wdk_abi_expected.h is only valid for WDK DDI builds (set AEROGPU_UMD_USE_WDK_HEADERS=1)."',
            "#endif",
            "",
            "// Probe metadata (best-effort; taken from probe output).",
            f'#define AEROGPU_D3D10_11_WDK_ABI_EXPECTED_GENERATED_UTC "{now}"',
        ]
    )

    if x86.msc_ver is not None:
        lines.append(f"#define AEROGPU_D3D10_11_WDK_ABI_PROBE_X86_MSC_VER {x86.msc_ver}")
    if x64.msc_ver is not None:
        lines.append(f"#define AEROGPU_D3D10_11_WDK_ABI_PROBE_X64_MSC_VER {x64.msc_ver}")
    if x86.d3d_umd_interface_version is not None:
        lines.append(
            f"#define AEROGPU_D3D10_11_WDK_ABI_PROBE_X86_D3D_UMD_INTERFACE_VERSION {x86.d3d_umd_interface_version}"
        )
    if x64.d3d_umd_interface_version is not None:
        lines.append(
            f"#define AEROGPU_D3D10_11_WDK_ABI_PROBE_X64_D3D_UMD_INTERFACE_VERSION {x64.d3d_umd_interface_version}"
        )

    lines.extend(
        [
            "",
            "// -----------------------------------------------------------------------------",
            "// x86 (Win32 / WOW64)",
            "// -----------------------------------------------------------------------------",
            "#if defined(_M_IX86)",
            "",
            *emit_arch_block(x86, include_exports=True),
            "// -----------------------------------------------------------------------------",
            "// x64 (Win64)",
            "// -----------------------------------------------------------------------------",
            "#elif defined(_M_X64) || defined(_M_AMD64)",
            "",
            *emit_arch_block(x64, include_exports=False),
            "#else",
            "  #error \"Unsupported MSVC architecture for AeroGPU D3D10/11 WDK ABI expectations.\"",
            "#endif",
            "",
        ]
    )

    return "\n".join(lines)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--x86", required=True, type=pathlib.Path, help="Probe output from the x86 build (text file).")
    ap.add_argument("--x64", required=True, type=pathlib.Path, help="Probe output from the x64 build (text file).")
    ap.add_argument("--out", required=True, type=pathlib.Path, help="Output header path.")
    ap.add_argument(
        "--allow-na",
        action="store_true",
        help="Allow optional offsetof(...) entries to be <n/a> in probe output (emits no macro for that field).",
    )
    args = ap.parse_args(argv)

    x86_text = args.x86.read_text(encoding="utf-8", errors="replace")
    x64_text = args.x64.read_text(encoding="utf-8", errors="replace")

    x86 = _parse_probe_output(x86_text)
    x64 = _parse_probe_output(x64_text)

    header = _emit_header(x86, x64, allow_na=args.allow_na)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(header, encoding="utf-8", newline="\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
