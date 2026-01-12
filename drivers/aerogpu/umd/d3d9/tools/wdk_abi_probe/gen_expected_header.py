#!/usr/bin/env python3
"""
Generate `aerogpu_d3d9_wdk_abi_expected.h` from `wdk_abi_probe` output.

This script exists to keep the checked-in ABI expectations in sync with the
canonical Win7 D3D9 UMDDI header set used for building the UMD.

Usage (from repo root, after building/running the probe for both arches):

  python3 drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/gen_expected_header.py \
    --x86 out_x86.txt \
    --x64 out_x64.txt \
    --out drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_wdk_abi_expected.h

By default this script emits a small curated set of high-value ABI anchors.
Pass `--all` to additionally emit *all* `sizeof(...)` / `offsetof(...)` values
captured by the probe (useful when expanding `aerogpu_d3d9_wdk_abi_expected.h` to
cover more structs without hand-editing).
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
_EXPORT_RE = re.compile(
    r"^PFND3DDDI_(?P<which>OPENADAPTER2?|OPENADAPTERFROMHDC|OPENADAPTERFROMLUID)\s*=>\s*_[A-Za-z0-9]+@(?P<bytes>\d+)\s*$",
    re.MULTILINE,
)
_MSC_VER_RE = re.compile(r"^_MSC_VER\s*=\s*(?P<value>\d+)\s*$", re.MULTILINE)
_UMD_IFACE_VER_RE = re.compile(r"^D3D_UMD_INTERFACE_VERSION\s*=\s*(?P<value>\d+)\s*$", re.MULTILINE)
_MACRO_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


@dataclass(frozen=True)
class ProbeData:
    sizeof: dict[str, int]
    offsetof: dict[tuple[str, str], int]
    exports: dict[str, int]
    msc_ver: int | None
    d3d_umd_interface_version: int | None


def _parse_probe_output(text: str) -> ProbeData:
    sizeof: dict[str, int] = {}
    offsetof: dict[tuple[str, str], int] = {}
    exports: dict[str, int] = {}

    for m in _SIZEOF_RE.finditer(text):
        sizeof[m.group("type").strip()] = int(m.group("value"))
    for m in _OFFSETOF_RE.finditer(text):
        offsetof[(m.group("type").strip(), m.group("member").strip())] = int(m.group("value"))
    for m in _EXPORT_RE.finditer(text):
        exports[m.group("which")] = int(m.group("bytes"))

    m_msc = _MSC_VER_RE.search(text)
    m_iface = _UMD_IFACE_VER_RE.search(text)

    return ProbeData(
        sizeof=sizeof,
        offsetof=offsetof,
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


def _get_export_bytes(data: ProbeData, which: str) -> int:
    try:
        return data.exports[which]
    except KeyError:
        raise SystemExit(f"Missing x86 export decoration for {which} in probe output")


def _emit_header(x86: ProbeData, x64: ProbeData, *, emit_all: bool) -> str:
    # Use a timezone-aware UTC timestamp; `utcnow()` is deprecated in newer
    # Python versions.
    now = _dt.datetime.now(tz=_dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

    # Key invariants we pin for both architectures.
    openadapter_fields = [
        ("D3DDDIARG_OPENADAPTER", "pAdapterCallbacks"),
        ("D3DDDIARG_OPENADAPTER", "hAdapter"),
        ("D3DDDIARG_OPENADAPTER", "pAdapterFuncs"),
    ]
    openadapter2_fields = [
        ("D3DDDIARG_OPENADAPTER2", "pAdapterCallbacks"),
        ("D3DDDIARG_OPENADAPTER2", "hAdapter"),
        ("D3DDDIARG_OPENADAPTER2", "pAdapterFuncs"),
    ]
    adapterfuncs_fields = [
        ("D3D9DDI_ADAPTERFUNCS", "pfnCloseAdapter"),
        ("D3D9DDI_ADAPTERFUNCS", "pfnGetCaps"),
        ("D3D9DDI_ADAPTERFUNCS", "pfnCreateDevice"),
        ("D3D9DDI_ADAPTERFUNCS", "pfnQueryAdapterInfo"),
    ]
    devicefuncs_fields = [
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroyDevice"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCreateResource"),
        ("D3D9DDI_DEVICEFUNCS", "pfnOpenResource"),
        ("D3D9DDI_DEVICEFUNCS", "pfnOpenResource2"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroyResource"),
        ("D3D9DDI_DEVICEFUNCS", "pfnLock"),
        ("D3D9DDI_DEVICEFUNCS", "pfnUnlock"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetRenderTarget"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetDepthStencil"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetViewport"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetScissorRect"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetTexture"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetSamplerState"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetRenderState"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCreateVertexDecl"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetVertexDecl"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroyVertexDecl"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetFVF"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCreateShader"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetShader"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroyShader"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetShaderConstF"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetStreamSource"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetIndices"),
        ("D3D9DDI_DEVICEFUNCS", "pfnBeginScene"),
        ("D3D9DDI_DEVICEFUNCS", "pfnEndScene"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCreateSwapChain"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroySwapChain"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetSwapChain"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetSwapChain"),
        ("D3D9DDI_DEVICEFUNCS", "pfnReset"),
        ("D3D9DDI_DEVICEFUNCS", "pfnResetEx"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCheckDeviceState"),
        ("D3D9DDI_DEVICEFUNCS", "pfnWaitForVBlank"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetGPUThreadPriority"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetGPUThreadPriority"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCheckResourceResidency"),
        ("D3D9DDI_DEVICEFUNCS", "pfnQueryResourceResidency"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetDisplayModeEx"),
        ("D3D9DDI_DEVICEFUNCS", "pfnComposeRects"),
        ("D3D9DDI_DEVICEFUNCS", "pfnRotateResourceIdentities"),
        ("D3D9DDI_DEVICEFUNCS", "pfnPresent"),
        ("D3D9DDI_DEVICEFUNCS", "pfnPresentEx"),
        ("D3D9DDI_DEVICEFUNCS", "pfnFlush"),
        ("D3D9DDI_DEVICEFUNCS", "pfnSetMaximumFrameLatency"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetMaximumFrameLatency"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetPresentStats"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetLastPresentCount"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCreateQuery"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDestroyQuery"),
        ("D3D9DDI_DEVICEFUNCS", "pfnIssueQuery"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetQueryData"),
        ("D3D9DDI_DEVICEFUNCS", "pfnGetRenderTargetData"),
        ("D3D9DDI_DEVICEFUNCS", "pfnCopyRects"),
        ("D3D9DDI_DEVICEFUNCS", "pfnWaitForIdle"),
        ("D3D9DDI_DEVICEFUNCS", "pfnBlt"),
        ("D3D9DDI_DEVICEFUNCS", "pfnColorFill"),
        ("D3D9DDI_DEVICEFUNCS", "pfnUpdateSurface"),
        ("D3D9DDI_DEVICEFUNCS", "pfnUpdateTexture"),
        ("D3D9DDI_DEVICEFUNCS", "pfnClear"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDrawPrimitive"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDrawPrimitiveUP"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDrawIndexedPrimitive"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDrawPrimitive2"),
        ("D3D9DDI_DEVICEFUNCS", "pfnDrawIndexedPrimitive2"),
    ]
    callbacks_fields = [
        ("D3DDDI_DEVICECALLBACKS", "pfnAllocateCb"),
        ("D3DDDI_DEVICECALLBACKS", "pfnDeallocateCb"),
        ("D3DDDI_DEVICECALLBACKS", "pfnSubmitCommandCb"),
        ("D3DDDI_DEVICECALLBACKS", "pfnRenderCb"),
        ("D3DDDI_DEVICECALLBACKS", "pfnPresentCb"),
        ("D3DDDI_DEVICECALLBACKS", "pfnCreateContextCb2"),
        ("D3DDDI_DEVICECALLBACKS", "pfnCreateContextCb"),
    ]
    createcontext_fields = [
        ("D3DDDIARG_CREATECONTEXT", "hDevice"),
        ("D3DDDIARG_CREATECONTEXT", "NodeOrdinal"),
        ("D3DDDIARG_CREATECONTEXT", "EngineAffinity"),
        ("D3DDDIARG_CREATECONTEXT", "Flags"),
        ("D3DDDIARG_CREATECONTEXT", "hContext"),
        ("D3DDDIARG_CREATECONTEXT", "pPrivateDriverData"),
        ("D3DDDIARG_CREATECONTEXT", "PrivateDriverDataSize"),
    ]
    submitcommand_fields = [
        ("D3DDDIARG_SUBMITCOMMAND", "hContext"),
        ("D3DDDIARG_SUBMITCOMMAND", "pCommandBuffer"),
        ("D3DDDIARG_SUBMITCOMMAND", "CommandLength"),
        ("D3DDDIARG_SUBMITCOMMAND", "CommandBufferSize"),
        ("D3DDDIARG_SUBMITCOMMAND", "pAllocationList"),
        ("D3DDDIARG_SUBMITCOMMAND", "AllocationListSize"),
        ("D3DDDIARG_SUBMITCOMMAND", "pPatchLocationList"),
        ("D3DDDIARG_SUBMITCOMMAND", "PatchLocationListSize"),
    ]
    create_device_fields = [
        ("D3D9DDIARG_CREATEDEVICE", "hAdapter"),
        ("D3D9DDIARG_CREATEDEVICE", "hDevice"),
        ("D3D9DDIARG_CREATEDEVICE", "Flags"),
        ("D3D9DDIARG_CREATEDEVICE", "pCallbacks"),
    ]
    create_resource_fields = [
        ("D3D9DDIARG_CREATERESOURCE", "Type"),
        ("D3D9DDIARG_CREATERESOURCE", "Format"),
        ("D3D9DDIARG_CREATERESOURCE", "Width"),
        ("D3D9DDIARG_CREATERESOURCE", "Height"),
        ("D3D9DDIARG_CREATERESOURCE", "Depth"),
        ("D3D9DDIARG_CREATERESOURCE", "MipLevels"),
        ("D3D9DDIARG_CREATERESOURCE", "Usage"),
        ("D3D9DDIARG_CREATERESOURCE", "Pool"),
        ("D3D9DDIARG_CREATERESOURCE", "Size"),
        ("D3D9DDIARG_CREATERESOURCE", "hResource"),
        ("D3D9DDIARG_CREATERESOURCE", "pSharedHandle"),
        ("D3D9DDIARG_CREATERESOURCE", "pPrivateDriverData"),
        ("D3D9DDIARG_CREATERESOURCE", "PrivateDriverDataSize"),
        ("D3D9DDIARG_CREATERESOURCE", "hAllocation"),
    ]
    open_resource_fields = [
        ("D3D9DDIARG_OPENRESOURCE", "pPrivateDriverData"),
        ("D3D9DDIARG_OPENRESOURCE", "PrivateDriverDataSize"),
        ("D3D9DDIARG_OPENRESOURCE", "hAllocation"),
        ("D3D9DDIARG_OPENRESOURCE", "hResource"),
    ]
    lock_fields = [
        ("D3D9DDIARG_LOCK", "hResource"),
        ("D3D9DDIARG_LOCK", "OffsetToLock"),
        ("D3D9DDIARG_LOCK", "SizeToLock"),
        ("D3D9DDIARG_LOCK", "Flags"),
    ]
    unlock_fields = [
        ("D3D9DDIARG_UNLOCK", "hResource"),
        ("D3D9DDIARG_UNLOCK", "OffsetToUnlock"),
        ("D3D9DDIARG_UNLOCK", "SizeToUnlock"),
    ]
    locked_box_fields = [
        ("D3D9DDI_LOCKED_BOX", "pData"),
        ("D3D9DDI_LOCKED_BOX", "rowPitch"),
        ("D3D9DDI_LOCKED_BOX", "slicePitch"),
    ]
    present_fields = [
        ("D3D9DDIARG_PRESENT", "hSrc"),
        ("D3D9DDIARG_PRESENT", "hSwapChain"),
        ("D3D9DDIARG_PRESENT", "hWnd"),
        ("D3D9DDIARG_PRESENT", "SyncInterval"),
        ("D3D9DDIARG_PRESENT", "Flags"),
    ]
    presentex_fields = [
        ("D3D9DDIARG_PRESENTEX", "hSrc"),
        ("D3D9DDIARG_PRESENTEX", "hWnd"),
        ("D3D9DDIARG_PRESENTEX", "SyncInterval"),
        ("D3D9DDIARG_PRESENTEX", "Flags"),
    ]

    def emit_arch_block(kind: str, data: ProbeData, include_exports: bool) -> list[str]:
        out: list[str] = []

        if emit_all:
            if include_exports:
                out.extend(
                    [
                        "  // Exported entrypoints (x86 stdcall decoration).",
                        f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTER')}",
                        f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER2_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTER2')}",
                        f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMHDC_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTERFROMHDC')}",
                        f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMLUID_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTERFROMLUID')}",
                        "",
                    ]
                )

            out.append("  // sizeof(...)")
            for type_name, value in sorted(data.sizeof.items()):
                # Probe output includes `sizeof(void*)`; skip any names that are
                # not valid preprocessor identifiers.
                if not _MACRO_IDENT_RE.match(type_name):
                    continue
                out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_{type_name} {value}")
            out.append("")

            out.append("  // offsetof(...)")
            for (type_name, member), value in sorted(data.offsetof.items()):
                if not _MACRO_IDENT_RE.match(type_name) or not _MACRO_IDENT_RE.match(member):
                    continue
                out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{type_name}_{member} {value}")
            out.append("")

            return out

        if include_exports:
            out.extend(
                [
                    "  // Exported entrypoints (x86 stdcall decoration).",
                    f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTER')}",
                    f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTER2_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTER2')}",
                    f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMHDC_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTERFROMHDC')}",
                    f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OPENADAPTERFROMLUID_STDCALL_BYTES {_get_export_bytes(data, 'OPENADAPTERFROMLUID')}",
                    "",
                ]
            )

        out.extend(
            [
                "  // OpenAdapter arg structs.",
                f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER {_get_size(data, 'D3DDDIARG_OPENADAPTER')}",
            ]
        )
        for t, m in openadapter_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")
        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3DDDIARG_OPENADAPTER2 {_get_size(data, 'D3DDDIARG_OPENADAPTER2')}")
        for t, m in openadapter2_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append("  // Adapter function table.")
        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_ADAPTERFUNCS {_get_size(data, 'D3D9DDI_ADAPTERFUNCS')}")
        for t, m in adapterfuncs_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append("  // Device function table (subset of high-value anchors).")
        for t, m in devicefuncs_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append("  // Runtime callback table (WDDM).")
        for t, m in callbacks_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append("  // Submission-related structs (WDDM).")
        for t, m in createcontext_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")
        for t, m in submitcommand_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append("  // D3D9UMDDI device argument structs (Win7 D3D9 runtime -> UMD).")
        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATEDEVICE {_get_size(data, 'D3D9DDIARG_CREATEDEVICE')}")
        for t, m in create_device_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_CREATERESOURCE {_get_size(data, 'D3D9DDIARG_CREATERESOURCE')}")
        for t, m in create_resource_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_OPENRESOURCE {_get_size(data, 'D3D9DDIARG_OPENRESOURCE')}")
        for t, m in open_resource_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_LOCK {_get_size(data, 'D3D9DDIARG_LOCK')}")
        for t, m in lock_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_UNLOCK {_get_size(data, 'D3D9DDIARG_UNLOCK')}")
        for t, m in unlock_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDI_LOCKED_BOX {_get_size(data, 'D3D9DDI_LOCKED_BOX')}")
        for t, m in locked_box_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENT {_get_size(data, 'D3D9DDIARG_PRESENT')}")
        for t, m in present_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_SIZEOF_D3D9DDIARG_PRESENTEX {_get_size(data, 'D3D9DDIARG_PRESENTEX')}")
        for t, m in presentex_fields:
            out.append(f"  #define AEROGPU_D3D9_WDK_ABI_EXPECT_OFFSETOF_{t}_{m} {_get_off(data, t, m)}")
        out.append("")

        return out

    lines: list[str] = []
    lines.extend(
        [
            "// Expected Win7 D3D9 UMD ABI values when building against the Windows WDK DDI",
            "// headers (d3dumddi.h / d3d9umddi.h).",
            "//",
            "// This file is generated from `tools/wdk_abi_probe` output. See:",
            "//   drivers/aerogpu/umd/d3d9/tools/wdk_abi_probe/README.md",
            f"// Generated: {now}",
            "//",
            "// clang-format off",
            "// clang-format on",
            "",
            "#pragma once",
            "",
            "#if !(defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI)",
            '  #error \"aerogpu_d3d9_wdk_abi_expected.h is only valid for WDK DDI builds (set AEROGPU_D3D9_USE_WDK_DDI=1).\"',
            "#endif",
            "",
            "// Probe metadata (best-effort; taken from probe output).",
            "#define AEROGPU_D3D9_WDK_ABI_EXPECTED_GENERATED_UTC \"" + now + "\"",
        ]
    )

    if x86.msc_ver is not None:
        lines.append(f"#define AEROGPU_D3D9_WDK_ABI_PROBE_X86_MSC_VER {x86.msc_ver}")
    if x86.d3d_umd_interface_version is not None:
        lines.append(f"#define AEROGPU_D3D9_WDK_ABI_PROBE_X86_D3D_UMD_INTERFACE_VERSION {x86.d3d_umd_interface_version}")
    if x64.msc_ver is not None:
        lines.append(f"#define AEROGPU_D3D9_WDK_ABI_PROBE_X64_MSC_VER {x64.msc_ver}")
    if x64.d3d_umd_interface_version is not None:
        lines.append(f"#define AEROGPU_D3D9_WDK_ABI_PROBE_X64_D3D_UMD_INTERFACE_VERSION {x64.d3d_umd_interface_version}")

    lines.extend(
        [
            "",
            "// -----------------------------------------------------------------------------",
            "// x86 (Win32 / WOW64)",
            "// -----------------------------------------------------------------------------",
            "#if defined(_M_IX86)",
            "",
            *emit_arch_block("x86", x86, include_exports=True),
            "// -----------------------------------------------------------------------------",
            "// x64 (Win64)",
            "// -----------------------------------------------------------------------------",
            "#elif defined(_M_X64) || defined(_M_AMD64)",
            "",
            *emit_arch_block("x64", x64, include_exports=False),
            "#else",
            "  #error \"Unsupported MSVC architecture for AeroGPU D3D9 WDK ABI expectations.\"",
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
        "--all",
        action="store_true",
        help="Emit all parsed sizeof/offsetof values from the probe output (not just curated anchors).",
    )
    args = ap.parse_args(argv)

    x86_text = args.x86.read_text(encoding="utf-8", errors="replace")
    x64_text = args.x64.read_text(encoding="utf-8", errors="replace")

    x86 = _parse_probe_output(x86_text)
    x64 = _parse_probe_output(x64_text)

    header = _emit_header(x86, x64, emit_all=args.all)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(header, encoding="utf-8", newline="\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
