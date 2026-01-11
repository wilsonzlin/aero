# AeroGPU Windows 7 WDDM 1.1 Kernel-Mode Display Miniport (KMD)

This directory contains a **minimal** WDDM 1.1 display miniport driver for Windows 7 SP1 (x86/x64). The design goal is bring-up: bind to the AeroGPU PCI device, perform a single-head VidPN modeset, expose a simple system-memory-only segment, and forward render/present submissions to the emulator via the canonical AeroGPU MMIO + ring ABI.

## Layout

```
drivers/aerogpu/kmd/
  include/                 Internal headers
  src/                     Miniport implementation (.c)
```

## Device ABI status (versioned vs legacy)

The Win7 AeroGPU KMD supports two AeroGPU PCI/MMIO ABIs:

* **Versioned ABI (primary, "AGPU")**
  * Headers:
    * `drivers/aerogpu/protocol/aerogpu_pci.h` (PCI IDs + MMIO register map)
    * `drivers/aerogpu/protocol/aerogpu_ring.h` (ring + submit descriptors + 64-bit fences + optional allocation table)
    * `drivers/aerogpu/protocol/aerogpu_cmd.h` (command stream packets)
  * PCI IDs: `VID=0xA3A0`, `DID=0x0001`
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu.rs`
* **Legacy bring-up ABI (compatibility, "ARGP")**
  * Historical reference: `drivers/aerogpu/protocol/aerogpu_protocol.h`
  * The KMD does **not** include `aerogpu_protocol.h` directly; it uses a minimal internal shim:
    `include/aerogpu_legacy_abi.h`
  * PCI IDs: `VID=0x1AED`, `DID=0x0001`
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu_legacy.rs`

During `DxgkDdiStartDevice`, the KMD reads BAR0 `AEROGPU_MMIO_REG_MAGIC`:

* If `AEROGPU_MMIO_MAGIC` (`"AGPU"`): use the versioned ABI and validate that the reported
  `AEROGPU_MMIO_REG_ABI_VERSION` major matches `AEROGPU_ABI_MAJOR` (reject major mismatches with
  `STATUS_NOT_SUPPORTED`).
* Otherwise: fall back to the legacy register map/ring format (the driver may log if the value is
  not the expected legacy `"ARGP"` magic).

See:
* `drivers/aerogpu/protocol/README.md` for ABI details.
* `docs/abi/aerogpu-pci-identity.md` for the canonical PCI IDs and the matching emulator device models.

## Canonical MMIO discovery (AGPU bring-up checklist)

When running against the **versioned** AGPU device, treat BAR0 as the canonical MMIO block
(`drivers/aerogpu/protocol/aerogpu_pci.h`) and validate:

1. **Magic + ABI version**
   - Read `AEROGPU_MMIO_REG_MAGIC` → must equal `AEROGPU_MMIO_MAGIC`.
   - Read `AEROGPU_MMIO_REG_ABI_VERSION` → `AEROGPU_ABI_VERSION_U32` (`major<<16 | minor`).
2. **Feature bits**
   - Read `AEROGPU_MMIO_REG_FEATURES_LO`/`HI` and combine to a 64-bit mask.
   - Decode/feature-gate optional behavior via `AEROGPU_FEATURE_*` bits:
     - `AEROGPU_FEATURE_FENCE_PAGE` (optional shared fence page)
     - `AEROGPU_FEATURE_VBLANK` (vblank IRQ + timing registers)
3. **Fence completion**
   - Read the 64-bit completed fence value from
     `AEROGPU_MMIO_REG_COMPLETED_FENCE_LO`/`HI`.
4. **Vblank timing + IRQs (when `AEROGPU_FEATURE_VBLANK` is set)**
   - Enable vblank IRQs via `AEROGPU_MMIO_REG_IRQ_ENABLE` (bit `AEROGPU_IRQ_SCANOUT_VBLANK`).
   - Poll status via `AEROGPU_MMIO_REG_IRQ_STATUS` and ack IRQs via `AEROGPU_MMIO_REG_IRQ_ACK`.
   - Consume timing information from:
     - `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO`/`HI`
     - `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO`/`HI`
     - `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS`

## Stable `alloc_id` / `share_token` (WDDM allocation private data)

To support D3D9Ex + DWM redirected surfaces and other cross-process shared allocations, AeroGPU relies on a stable identifier per WDDM allocation:

- `alloc_id` (32-bit, nonzero, stable across opens)
- `share_token` (64-bit, stable across guest processes; `0` for non-shared allocations)

These values live in **WDDM allocation private driver data** (`aerogpu_wddm_alloc_priv`):

- The blob is treated as **UMD → KMD input**: the UMD generates `alloc_id` and `share_token` and attaches them to each allocation.
- For **shared allocations**, dxgkrnl preserves the blob and returns the exact same bytes on `OpenResource`/`DxgkDdiOpenAllocation` in another process, ensuring both processes observe identical IDs.
- The KMD validates and stores the IDs in its allocation bookkeeping and uses `alloc_id` when building the per-submit allocation table for the emulator.
- To avoid silent corruption, the KMD rejects submissions where the allocation list contains the same `alloc_id` for different backing ranges (`gpa/size_bytes`).
- For standard allocations where the runtime does not provide an AeroGPU private-data blob, the KMD synthesizes an `alloc_id` from a reserved namespace (high bit set) and sets `share_token = 0`.

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

Typical dev install flows:

- **Recommended (host build + sign via CI scripts):**
  1. On the build host:
      ```powershell
      pwsh ci/install-wdk.ps1
      pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu
      pwsh ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
      pwsh ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
      ```
  2. Copy `out/packages/aerogpu/<x86|x64>/` and `out/certs/aero-test.cer` into the Win7 VM.
  3. In the Win7 VM (Admin), trust the certificate and enable test signing:
      ```bat
      :: The CI-staged package includes helper scripts under packaging\\win7\\.
      :: Copy aero-test.cer into the package root (next to the INF), then run:
      cd C:\path\to\out\packages\aerogpu\x64
      packaging\win7\trust_test_cert.cmd
      shutdown /r /t 0
      ```
  4. After reboot, install the signed INF from the copied package directory:
      ```bat
      pnputil -i -a C:\path\to\out\packages\aerogpu\x64\aerogpu.inf
      ```

- **Legacy (stage packaging folder + sign inside VM):**
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

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2` (see `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h`)
  - returns the detected device ABI (`detected_mmio_magic`), ABI version, and (for versioned devices) feature bits
  - older tools may use the legacy `AEROGPU_ESCAPE_OP_QUERY_DEVICE` response

Additional debug/control escapes used by `drivers/aerogpu/tools/win7_dbgctl`:

- `AEROGPU_ESCAPE_OP_QUERY_FENCE` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_SELFTEST` (see `aerogpu_dbgctl_escape.h`)

These are intended for a small user-mode tool to validate KMD↔emulator communication early.
