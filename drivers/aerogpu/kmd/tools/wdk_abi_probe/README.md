# Win7 WDK 7.1 KMD ABI Probe (DXGK vblank interrupt ABI)

This directory contains a **standalone console program** intended to be built
against the **real Windows 7 WDK 7.1** display miniport headers (`d3dkmddi.h`).

It prints ABI-critical details for Win7 WDDM 1.1 vblank interrupt delivery:

- `sizeof(...)` / `offsetof(...)` for `DXGKARGCB_NOTIFY_INTERRUPT` and its vblank
  union payload (`CrtcVsync.VidPnSourceId`).
- Enum values for vblank-related `DXGK_INTERRUPT_TYPE_*` constants.

This makes header/version drift obvious before debugging a Win7 VM.

## Build (WDK 7.1 environment)

1. Install **Windows 7 WDK 7.1**.
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

To freeze the Win7 ABI in your WDK build, define one or more expected-value
macros (using values printed by this probe), e.g.:

```text
/DAEROGPU_KMD_WDK_ABI_EXPECT_SIZEOF_DXGKARGCB_NOTIFY_INTERRUPT=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync_VidPnSourceId=...
/DAEROGPU_KMD_WDK_ABI_EXPECT_DXGK_INTERRUPT_TYPE_CRTC_VSYNC=...
```

These checks are intentionally optional so repo-local builds can keep working
even if they are not built against the WDK 7.1 headers.

