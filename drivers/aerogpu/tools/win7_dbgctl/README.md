# AeroGPU Win7 debug/control utility (Escape-based)

`aerogpu_dbgctl.exe` is a small user-mode console tool intended for bring-up and debugging of the AeroGPU WDDM kernel-mode driver (KMD) on **Windows 7 SP1**.

It talks to the installed AeroGPU driver via **`DxgkDdiEscape`** using `D3DKMTEscape` (driver-private escape packets).

## Supported device models / ABIs

The in-tree AeroGPU Win7 KMD supports both the **versioned** and **legacy bring-up** AeroGPU PCI devices, auto-detected
via BAR0 MMIO magic:

- **Versioned ABI device**: `PCI\\VEN_A3A0&DEV_0001` ("AGPU")  
  Ring = `aerogpu_ring_header` + `aerogpu_submit_desc` slots.
- **Legacy bring-up ABI device**: `PCI\\VEN_1AED&DEV_0001` ("ARGP")  
  Ring = legacy `aerogpu_legacy_ring_entry` entries (see `drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h`).
  Note: the emulator legacy device model is optional (feature `emulator/aerogpu-legacy`).

Note: the shipped Win7 driver packages (`drivers/aerogpu/packaging/win7`) bind to the canonical `PCI\\VEN_A3A0&DEV_0001`
HWID only. Installing against the legacy bring-up HWID requires a custom INF that matches `PCI\\VEN_1AED&DEV_0001`.

`--query-version` (alias: `--query-device`), `--query-fence`, `--dump-ring`, and `--selftest` rely on dbgctl escape
packets implemented by the installed KMD. The tool is primarily developed against the versioned ("AGPU") path, but
keeps best-effort compatibility decoding for legacy ("ARGP") devices and older KMD builds.

## Features

Minimum supported commands:

- `aerogpu_dbgctl --list-displays`  
  Prints the available `\\.\DISPLAY*` names to use with `--display`.

- `aerogpu_dbgctl --query-version` (alias: `--query-device`)  
  Prints the detected AeroGPU device ABI (**legacy ARGP** vs **new AGPU**), ABI version, and (when exposed) device feature bits.
  Also prints a fence snapshot and (when available) a scanout0 vblank timing snapshot.

- `aerogpu_dbgctl --status` *(alias for `--query-version`)*  
  Prints a short combined snapshot (device/ABI + fences + scanout0 vblank).

- `aerogpu_dbgctl --query-umd-private`  
  Calls `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` and prints the `aerogpu_umd_private_v1` blob used by UMDs to discover the active ABI + feature bits.

- `aerogpu_dbgctl --query-fence`  
  Prints the last submitted fence and last completed fence.

- `aerogpu_dbgctl --dump-ring`  
  Dumps ring head/tail + recent submissions. Fields include:
  - `signal_fence`
  - `cmd_gpa` / `cmd_size_bytes`
  - `flags`
  - (AGPU only) `alloc_table_gpa` / `alloc_table_size_bytes`

- `aerogpu_dbgctl --dump-vblank`  
  Dumps vblank timing counters (seq/last time/period) and IRQ status/enable masks.
  Works on both legacy and new AeroGPU devices as long as the device exposes
  `AEROGPU_FEATURE_VBLANK` in `FEATURES_LO/HI`.
  When available, it also prints `vblank_interrupt_type` (the `DXGK_INTERRUPT_TYPE`
  value dxgkrnl enabled via `DxgkDdiControlInterrupt`).
  Use `--vblank-samples` to observe changes over time and estimate the effective Hz/jitter.

  Alias: `aerogpu_dbgctl --query-vblank`

- `aerogpu_dbgctl --wait-vblank`  
  Calls `D3DKMTWaitForVerticalBlankEvent` in a loop (with `--timeout-ms` safety timeout) and prints the observed pacing.

- `aerogpu_dbgctl --query-scanline`  
  Calls `D3DKMTGetScanLine` and prints the current scanline and vblank state.

- `aerogpu_dbgctl --map-shared-handle <HANDLE>`  
  Calls the KMD escape `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` to map a Win32 shared-handle to a stable 32-bit `share_token`.

- `aerogpu_dbgctl --selftest`  
  Triggers a simple KMD-side self-test.

## Usage

```
aerogpu_dbgctl [--display \\.\DISPLAY1] [--ring-id N] [--timeout-ms N] \
               [--vblank-samples N] [--vblank-interval-ms N] <command>
```

Examples:

```
aerogpu_dbgctl --list-displays
aerogpu_dbgctl --status
aerogpu_dbgctl --query-device
aerogpu_dbgctl --query-umd-private
aerogpu_dbgctl --query-fence
aerogpu_dbgctl --dump-ring --ring-id 0
aerogpu_dbgctl --dump-vblank
aerogpu_dbgctl --dump-vblank --vblank-samples 10 --vblank-interval-ms 200
aerogpu_dbgctl --wait-vblank --vblank-samples 120 --timeout-ms 2000
aerogpu_dbgctl --query-scanline --vblank-samples 20 --vblank-interval-ms 10
aerogpu_dbgctl --map-shared-handle 0x1234
aerogpu_dbgctl --selftest --timeout-ms 2000
```

Notes:
- `IRQ_ACTIVE` is `IRQ_STATUS & IRQ_ENABLE` (i.e. causes that can currently assert the interrupt line).
- Some environments may return a non-zero `VidPnSourceId` from `D3DKMTOpenAdapterFromHdc`; if `--wait-vblank` or `--query-scanline` fail with `STATUS_INVALID_PARAMETER`, dbgctl retries with source 0 (AeroGPU currently implements a single source).

## Build (Windows 7)

This tool intentionally **does not require the WDK**: it dynamically loads the required `D3DKMT*` entrypoints from `gdi32.dll`.

### Build with the Visual Studio Developer Command Prompt

From the repo root:

```
cd drivers\aerogpu\tools\win7_dbgctl
build_vs2010.cmd
```

Or build directly with `cl.exe`:

```
cd drivers\aerogpu\tools\win7_dbgctl
cl /nologo /W4 /EHsc /DUNICODE /D_UNICODE ^
  /I ..\..\protocol ^
  src\aerogpu_dbgctl.cpp ^
  /Feaerogpu_dbgctl.exe ^
  user32.lib gdi32.lib
```

Outputs:

- `build_vs2010.cmd` → `drivers\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`
- direct `cl` invocation → `drivers\\aerogpu\\tools\\win7_dbgctl\\aerogpu_dbgctl.exe`

## Protocol

Packet definitions consumed by this tool live in:

- `drivers/aerogpu/protocol/aerogpu_escape.h` (base Escape header + `AEROGPU_ESCAPE_OP_QUERY_DEVICE` fallback)
- `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h` (dbgctl ops, including `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2`)

The AeroGPU KMD is expected to implement `DxgkDdiEscape` handling for these packets (driver-private escape).

Escape ops used:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2` (fallback: `AEROGPU_ESCAPE_OP_QUERY_DEVICE`) → `--query-version` / `--query-device`
- `AEROGPU_ESCAPE_OP_QUERY_FENCE` → `--query-fence`
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) → `--dump-ring`
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (alias: `AEROGPU_ESCAPE_OP_DUMP_VBLANK`) → `--dump-vblank`
- `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` → `--map-shared-handle`
- `AEROGPU_ESCAPE_OP_SELFTEST` → `--selftest`

Additional WDDM queries (do not use the escape channel):

- `--query-umd-private` uses `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` to query the KMD-provided discovery blob.
- `--wait-vblank` uses `D3DKMTWaitForVerticalBlankEvent` to measure vblank delivery from the OS.
- `--query-scanline` uses `D3DKMTGetScanLine` to report the current scanline and vblank state.

## Notes / troubleshooting

- If `D3DKMTOpenAdapterFromHdc` or `D3DKMTEscape` cannot be resolved from `gdi32.dll`, the OS is too old or the environment is not WDDM-capable.
- If `D3DKMTEscape` returns an error, ensure the AeroGPU driver is installed and exposes the required escapes.
- If `--wait-vblank` times out, the process may skip `D3DKMTCloseAdapter` to avoid deadlocking on broken vblank implementations.
