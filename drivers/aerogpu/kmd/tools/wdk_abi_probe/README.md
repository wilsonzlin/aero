# Win7 KMD ABI Probe (DXGK vblank interrupt ABI)

This directory contains a **standalone console program** intended to be built
against the **real Win7-era** display miniport headers (`d3dkmddi.h`).

It prints ABI-critical details for Win7 WDDM 1.1 display miniport bring-up, including:

- `sizeof(...)` / `offsetof(...)` for `DXGKARGCB_NOTIFY_INTERRUPT` and its vblank
  union payload (`CrtcVsync.VidPnSourceId`).
- Enum values for vblank-related `DXGK_INTERRUPT_TYPE_*` constants.
- The bitmask values for key flag bitfields:
  - `DXGK_ALLOCATIONINFO::Flags.Value` (decoded by setting individual bitfields)
  - `DXGK_ALLOCATIONLIST::Flags.Value` (notably `WriteOperation`, used for alloc-table READONLY propagation)
  - `DXGKARG_CREATEALLOCATION::Flags.Value`

This makes header/version drift obvious before debugging a Win7 VM.

## Build (Win7-era WDK environment)

1. Install a WDK that provides the Win7 WDDM 1.1 display miniport headers (`d3dkmddi.h`).
   A WinDDK 7600-era kit is known to work.
2. Open the appropriate WDK build environment command prompt:
   - “Windows 7 x86 Free Build Environment”
   - “Windows 7 x64 Free Build Environment”
3. Build and run the probe:

```cmd
cd \path\to\repo\drivers\aerogpu\kmd\tools\wdk_abi_probe

rem x86
cl /nologo /W4 /EHsc kmd_wdk_abi_probe.cpp /Fe:kmd_wdk_abi_probe_x86.exe
kmd_wdk_abi_probe_x86.exe

rem x64 (from the x64 WDK environment)
cl /nologo /W4 /EHsc kmd_wdk_abi_probe.cpp /Fe:kmd_wdk_abi_probe_x64.exe
kmd_wdk_abi_probe_x64.exe
```

If compilation fails because a type/member is missing, you are likely compiling
against a header set that does not match Win7/WDDM 1.1.

## Feeding results back into the driver (optional)

The KMD contains optional compile-time ABI asserts in:

- `drivers/aerogpu/kmd/include/aerogpu_kmd_wdk_abi_asserts.h`

The asserts header is intentionally inert unless the build defines
`AEROGPU_KMD_USE_WDK_DDI=1` (treat `=0` as disabled). The MSBuild project does
this automatically for the Win7 target builds.

To freeze the Win7 ABI in your WDK build, define one or more expected-value
macros (using values printed by this probe), e.g.:

```text
/DAEROGPU_KMD_WDK_ABI_EXPECT_SIZEOF_DXGKARGCB_NOTIFY_INTERRUPT=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync_VidPnSourceId=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_DXGK_INTERRUPT_TYPE_CRTC_VSYNC=...
```

These checks are intentionally optional so repo-local builds can keep working
even if they are not built against the Win7-era headers.
