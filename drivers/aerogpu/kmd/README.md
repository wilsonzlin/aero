# AeroGPU Windows 7 WDDM 1.1 Kernel-Mode Display Miniport (KMD)

This directory contains a **minimal** WDDM 1.1 display miniport driver for Windows 7 SP1 (x86/x64). The design goal is bring-up: bind to the AeroGPU PCI device, perform a single-head VidPN modeset, expose a simple system-memory-only segment, and forward render/present submissions to the emulator via a shared ring/MMIO ABI.

## Layout

```
drivers/aerogpu/kmd/
  include/                 Internal headers
  src/                     Miniport implementation (.c)
  makefile / sources       WDK 7.1 BUILD project
```

The device ABI is defined in `drivers/aerogpu/protocol/aerogpu_protocol.h`.

> Note: `aerogpu_protocol.h` is the **legacy** “all-in-one” header used by the current bring-up KMD implementation.
> The current protocol is split across:
>
> - `drivers/aerogpu/protocol/aerogpu_pci.h` (PCI/MMIO + versioning)
> - `drivers/aerogpu/protocol/aerogpu_ring.h` (ring + submissions + optional allocation table)
> - `drivers/aerogpu/protocol/aerogpu_cmd.h` (command stream packets, `resource_handle`/`backing_alloc_id`, shared surface export/import)
>
> New/updated UMDs should target the split headers. The KMD will be migrated as the ring/MMIO ABI is updated.

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

## Building (WDK 7.1)

Recommended (build everything via scripts):

```bat
cd \path\to\repo
drivers\aerogpu\build\build_all.cmd fre
```

Manual (single config from a WDK build environment shell):

1. Install **WDK 7.1** (Windows 7 SP1 WDK).
2. Open the appropriate WDK build environment:
   - `x86 Checked Build Environment` for 32-bit
   - `x64 Checked Build Environment` for 64-bit
3. From the build shell:

```bat
cd \path\to\repo\drivers\aerogpu\kmd
build -cZ
```

The output `.sys` will be placed under the WDK `obj*` directory.

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

`DxgkDdiEscape` supports a bring-up query:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE` (see `aerogpu_protocol.h`)
  - returns the device MMIO version (`AEROGPU_REG_VERSION`)

Additional debug/control escapes used by `drivers/aerogpu/tools/win7_dbgctl`:

- `AEROGPU_ESCAPE_OP_QUERY_FENCE` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_DUMP_RING` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_SELFTEST` (see `aerogpu_dbgctl_escape.h`)

These are intended for a small user-mode tool to validate KMD↔emulator communication early.
