# AeroGPU Windows 7 WDDM 1.1 Kernel-Mode Display Miniport (KMD)

This directory contains a **minimal** WDDM 1.1 display miniport driver for Windows 7 SP1 (x86/x64). The design goal is bring-up: bind to the AeroGPU PCI device, perform a single-head VidPN modeset, expose a simple system-memory-only segment, and forward render/present submissions to the emulator via a shared ring/MMIO ABI.

## Layout

```
drivers/aerogpu/kmd/
  include/                 Internal headers
  src/                     Miniport implementation (.c)
```

## ABI status (legacy vs versioned)

The KMD supports two AeroGPU device ABIs:

* **Versioned ABI (primary, new device)**:
  * `drivers/aerogpu/protocol/aerogpu_pci.h` (PCI IDs + MMIO register map)
  * `drivers/aerogpu/protocol/aerogpu_ring.h` (ring + submit descriptors + fence page + optional allocation table)
  * `drivers/aerogpu/protocol/aerogpu_cmd.h` (command stream packets)
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu.rs`
* **Legacy bring-up ABI (compatibility)**:
  * Historical reference: `drivers/aerogpu/protocol/aerogpu_protocol.h`
  * The KMD does **not** include `aerogpu_protocol.h` directly; it uses a minimal internal shim:
    `include/aerogpu_legacy_abi.h`
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu_legacy.rs`

The KMD detects which ABI is active by reading BAR0[0] (MMIO magic):
* New ABI: `"AGPU"` (`AEROGPU_MMIO_MAGIC`)
* Legacy ABI: `"ARGP"` (`AEROGPU_LEGACY_MMIO_MAGIC`)

Note that the legacy and versioned ABIs use **different PCI IDs**:
* Legacy (`aerogpu_protocol.h`): `VID=0x1AED`, `DID=0x0001`
* Versioned (`aerogpu_pci.h`): `VID=0xA3A0`, `DID=0x0001`

Make sure your Win7 INF and your emulator device model agree on which VID/DID to expose.

See:
* `drivers/aerogpu/protocol/README.md` for ABI details.
* `docs/abi/aerogpu-pci-identity.md` for the canonical PCI IDs and the matching emulator device models.

## Stable `alloc_id` / `share_token` (WDDM allocation private data)

To support D3D9Ex + DWM redirected surfaces and other cross-process shared allocations, the KMD exposes a stable identifier for every WDDM allocation:

- `alloc_id` (32-bit, nonzero, stable across opens)
- `share_token` (64-bit, stable across guest processes; `0` for non-shared allocations)

These are returned to UMDs via **WDDM allocation private driver data**:

- The KMD writes the struct in `DxgkDdiCreateAllocation`.
- When an allocation is opened via a shared handle in another process, dxgkrnl passes the same private blob back to the KMD in `DxgkDdiOpenAllocation`, allowing the KMD to reconstruct the same IDs.

The shared layout is defined in:

- `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`

## Building (WDK 10 / MSBuild)

This miniport can be built via the **WDK 10** MSBuild project at:

* `drivers/aerogpu/aerogpu_kmd.vcxproj`
* or the combined driver stack solution: `drivers/aerogpu/aerogpu.sln`

From a VS2022 Developer Command Prompt (with WDK 10 installed):

```bat
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
```

Configuration mapping:

* `Debug` ~= `chk` (defines `DBG=1`, enabling `DbgPrintEx` logging)
* `Release` ~= `fre`

The MSBuild entrypoint for the KMD is `drivers/aerogpu/aerogpu_kmd.vcxproj` (it builds the sources in this directory).

## Building

Recommended (CI-like, builds and stages packages under `out/`):

```powershell
pwsh ci/install-wdk.ps1
pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu
```

Manual (single configuration via MSBuild):

```cmd
msbuild drivers\aerogpu\aerogpu_kmd.vcxproj /m /p:Configuration=Release /p:Platform=x64
msbuild drivers\aerogpu\aerogpu_kmd.vcxproj /m /p:Configuration=Release /p:Platform=Win32
```

The output `.sys` will be placed under the MSBuild output directory (or whatever `OutDir` you provide).

## Installing (Windows 7 VM)

Use the in-tree Win7 packaging folder (INF + signing + install helpers):

* `drivers/aerogpu/packaging/win7/`

Typical dev install flow:

1. Stage the packaging folder with built binaries (from repo root, on the build machine):

```bat
drivers\aerogpu\build\stage_packaging_win7.cmd fre x64
```

2. Copy `drivers\aerogpu\packaging\win7\` into the Win7 VM (or share the repo).
3. In the Win7 VM, run as Administrator:

```bat
cd drivers\aerogpu\packaging\win7
sign_test.cmd
install.cmd
```

## Debugging

The driver uses `DbgPrintEx` in checked builds (`DBG=1`). Typical workflow:

1. Attach WinDbg to the VM kernel.
2. Enable debug print filtering as needed.
3. Look for messages prefixed with `aerogpu-kmd:`.

## Escape channel

`DxgkDdiEscape` supports bring-up/debug queries:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2` (see `drivers/aerogpu/protocol/aerogpu_escape.h`)
  - returns the detected device ABI (`detected_mmio_magic`), ABI version, and (for versioned devices) feature bits
  - older tools may use the legacy `AEROGPU_ESCAPE_OP_QUERY_DEVICE` response

Additional debug/control escapes used by `drivers/aerogpu/tools/win7_dbgctl`:

- `AEROGPU_ESCAPE_OP_QUERY_FENCE` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_SELFTEST` (see `aerogpu_dbgctl_escape.h`)

These are intended for a small user-mode tool to validate KMD↔emulator communication early.
