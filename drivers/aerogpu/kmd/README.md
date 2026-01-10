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

## Building (WDK 7.1)

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

This repository does not include a production-ready INF; device IDs are part of the VM/device model. You will typically:

1. Create an INF that matches your AeroGPU PCI VEN/DEV.
2. Enable test-signing in the VM:

```bat
bcdedit /set testsigning on
shutdown /r /t 0
```

3. Test-sign the built driver (or use a test certificate).
4. Use **Device Manager → Update Driver** and point it at the INF.

## Debugging

The driver uses `DbgPrintEx` in checked builds (`DBG=1`). Typical workflow:

1. Attach WinDbg to the VM kernel.
2. Enable debug print filtering as needed.
3. Look for messages prefixed with `aerogpu-kmd:`.

## Escape channel

`DxgkDdiEscape` supports a bring-up query:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE` (see `aerogpu_protocol.h`)
  - returns the device MMIO version (`AEROGPU_REG_VERSION`)

This is intended for a small user-mode tool to validate KMD↔emulator communication early.

