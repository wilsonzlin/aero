# AeroGPU Win7 debug/control utility (Escape-based)

`aerogpu_dbgctl.exe` is a small user-mode console tool intended for bring-up and debugging of the AeroGPU WDDM kernel-mode driver (KMD) on **Windows 7 SP1**.

It talks to the installed AeroGPU driver via **`DxgkDdiEscape`** using `D3DKMTEscape` (driver-private escape packets).

## Features

Minimum supported commands:

- `aerogpu_dbgctl --query-version`  
  Prints the AeroGPU MMIO/device version as reported by the KMD via `DxgkDdiEscape`.

- `aerogpu_dbgctl --query-fence`  
  Prints the last submitted fence and last completed fence.

- `aerogpu_dbgctl --dump-ring`  
  Dumps ring head/tail + recent descriptors (if exposed by the driver).

- `aerogpu_dbgctl --selftest`  
  Triggers a simple KMD-side self-test.

## Usage

```
aerogpu_dbgctl [--display \\.\DISPLAY1] [--ring-id N] [--timeout-ms N] <command>
```

Examples:

```
aerogpu_dbgctl --query-version
aerogpu_dbgctl --query-fence
aerogpu_dbgctl --dump-ring --ring-id 0
aerogpu_dbgctl --selftest --timeout-ms 2000
```

## Build (Windows 7)

This tool intentionally **does not require the WDK**: it dynamically loads the required `D3DKMT*` entrypoints from `gdi32.dll`.

### Build with the Visual Studio Developer Command Prompt

From the repo root:

```
cd drivers\aerogpu\tools\win7_dbgctl
cl /nologo /W4 /EHsc /DUNICODE /D_UNICODE ^
  /I ..\..\protocol ^
  src\aerogpu_dbgctl.cpp ^
  /Feaerogpu_dbgctl.exe ^
  user32.lib gdi32.lib
```

The output binary will be:

```
drivers\aerogpu\tools\win7_dbgctl\aerogpu_dbgctl.exe
```

## Protocol

Packet definitions consumed by this tool live in:

- `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h`

The AeroGPU KMD is expected to implement `DxgkDdiEscape` handling for these packets (driver-private escape).

Escape ops used:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE` → `--query-version`
- `AEROGPU_ESCAPE_OP_QUERY_FENCE` → `--query-fence`
- `AEROGPU_ESCAPE_OP_DUMP_RING` → `--dump-ring`
- `AEROGPU_ESCAPE_OP_SELFTEST` → `--selftest`

## Notes / troubleshooting

- If `D3DKMTOpenAdapterFromHdc` or `D3DKMTEscape` cannot be resolved from `gdi32.dll`, the OS is too old or the environment is not WDDM-capable.
- If `D3DKMTEscape` returns an error, ensure the AeroGPU driver is installed and exposes the required escapes.
