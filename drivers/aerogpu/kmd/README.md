# AeroGPU Windows 7 WDDM 1.1 Kernel-Mode Display Miniport (KMD)

This directory contains a **minimal** WDDM 1.1 display miniport driver for Windows 7 SP1 (x86/x64). The design goal is bring-up: bind to the AeroGPU PCI device, perform a single-head VidPN modeset, expose a simple system-memory-only segment, and forward render/present submissions to the emulator via the canonical AeroGPU MMIO + ring ABI.

## Display modes (VidPN)

The Win7 AeroGPU KMD exposes a small, deterministic set of common **60 Hz progressive** modes derived from the hard-coded EDID (preferred + standard timings) plus a curated fallback list (single source/target, `X8R8G8B8` / `B8G8R8X8` scanout):

- 640×480
- 800×600
- 1024×768
- 1280×720
- 1280×800
- 1366×768
- 1368×768 *(EDID standard timing quantization)*
- 1600×900
- 1920×1080 *(EDID preferred mode)*

### Timing model

The KMD does **not** attempt to model full CEA/CVT timings. For Win7 bring-up stability it uses a simple, internally-consistent
synthetic timing model:

- `VideoSignalInfo.ActiveSize` is set to the requested mode resolution.
- `VideoSignalInfo.TotalSize` uses conservative synthetic blanking:
  - vertical blanking is based on the same heuristic used by `DxgkDdiGetScanLine` (so scanline/vblank reporting matches the
    advertised total line count)
  - horizontal blanking is a small fixed fraction of the active width (to avoid “0 blanking” edge cases)
- `VSyncFreq` is advertised as 60 Hz; `IsSupportedVidPn` is permissive and accepts typical desktop refresh rates so dxgkrnl can
  keep stable mode selections (e.g. 59.94 Hz encoded as 59940/1000).

### Optional registry overrides (bring-up safety)

Registry path (service key parameters):

`HKLM\SYSTEM\CurrentControlSet\Services\aerogpu\Parameters`

All values are `REG_DWORD` (0 or missing means “unset”):

- `PreferredWidth` + `PreferredHeight`
  - Forces the preferred/default mode used by the driver when constructing mode lists.
  - **Both must be set** (otherwise ignored).
- `MaxWidth` and/or `MaxHeight`
  - Caps the maximum mode exposed to Windows (useful to avoid large primary allocations during early bring-up).
- `MaxDmaBufferBytes`
  - Caps the maximum **effective** DMA buffer size copied into contiguous memory per submission (after any
    command-stream header size shrink).
  - This protects the guest from pathological user-mode submissions attempting extremely large contiguous
    allocations (DMA copy, legacy descriptors, allocation table).
  - Default: 32 MiB (x64), 16 MiB (x86). Clamped to [256 KiB, 256 MiB].

## Layout

```
drivers/aerogpu/kmd/
  include/                 Internal headers
  src/                     Miniport implementation (.c)
```

## WDDM segment size / memory reporting (`NonLocalMemorySizeMB`)

AeroGPU is a **system-memory-only** WDDM adapter: all GPU allocations are backed by **guest system RAM** and the emulator
accesses them via physical addresses (there is no dedicated VRAM segment).

The KMD reports a single WDDM segment (Aperture + CPU-visible) in:

- `DXGKQAITYPE_QUERYSEGMENT` (segment descriptor size), and
- `DXGKQAITYPE_GETSEGMENTGROUPSIZE` (`NonLocalMemorySize`)

Windows uses this as a *budget* for resource allocation. If the budget is too small, D3D9/D3D11 workloads that create many
large textures/buffers can fail with allocation errors even when the guest still has free RAM.

The non-local segment budget can be overridden via a device registry parameter:

- **Key:** `HKR\Parameters\NonLocalMemorySizeMB`
- **Type:** `REG_DWORD`
- **Unit:** megabytes
- **Default:** 512
- **Clamped:** min 128; max 2048 on x64, max 1024 on x86

Notes:

- This value does **not** reserve memory up front; it only changes the reported WDDM budget.
- Setting it too high can increase guest RAM consumption and paging pressure under heavy workloads.

Recommended values:

- **Win7 x64:** 1024–2048 (depending on guest RAM and workload)
- **Win7 x86:** 256–1024 (the KMD will clamp larger values down to 1024)

Where to set it:

- The Win7 AeroGPU INFs seed this value during install:
  - `HKR\Parameters\NonLocalMemorySizeMB = 512` (`REG_DWORD`)
  - written with `FLG_ADDREG_NOCLOBBER`, so a user override is preserved across reinstall/upgrade.
- `HKR` is the AeroGPU adapter's device/driver registry key (the same place that stores `InstalledDisplayDrivers`).
- On Win7 this is typically under the display class key
  `HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\`.
  To override it, create/open a `Parameters` subkey and set `NonLocalMemorySizeMB` there (the exact `000X` varies by machine).

### Manual validation guidance

1. Override `NonLocalMemorySizeMB` to a larger value (e.g., 1024 or 2048 on x64).
2. Reboot the guest (or disable/enable the AeroGPU device).
3. Run `drivers/aerogpu/tests/win7/segment_budget_sanity` to confirm the new WDDM budget is visible from user mode
   (it queries `D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE)` and prints the segment size in MiB).
4. Run a D3D11 workload that allocates multiple large textures (for example, repeatedly call
   `ID3D11Device::CreateTexture2D` for 4096×4096 RGBA textures in a loop).
5. Confirm the workload no longer fails early due to segment budget/`E_OUTOFMEMORY` when configured larger (until you hit
   actual guest RAM limits).

## Device ABI status (versioned vs legacy)
  
The legacy bring-up device ABI is defined in `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`.

The Win7 AeroGPU KMD supports two AeroGPU PCI/MMIO ABIs:

* **Versioned ABI (primary, "AGPU")**
  * Headers:
    * `drivers/aerogpu/protocol/aerogpu_pci.h` (PCI IDs + MMIO register map)
    * `drivers/aerogpu/protocol/aerogpu_ring.h` (ring + submit descriptors + 64-bit fences + optional allocation table)
    * `drivers/aerogpu/protocol/aerogpu_cmd.h` (command stream packets)
  * PCI IDs: `VID=0xA3A0`, `DID=0x0001`
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu.rs`
* **Legacy bring-up ABI (compatibility, "ARGP")**
  * Historical reference: `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`
  * The KMD does **not** include `aerogpu_protocol_legacy.h` directly; it uses a minimal internal shim:
    `include/aerogpu_legacy_abi.h`
  * PCI identity: legacy bring-up device model (see `docs/abi/aerogpu-pci-identity.md`)
  * Emulator device model: `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (feature `emulator/aerogpu-legacy`)
* This ABI is deprecated and retained only for optional compatibility/regression testing.

The Win7 packaging INFs (`drivers/aerogpu/packaging/win7/*.inf`) bind to the canonical, versioned device:

* `PCI\VEN_A3A0&DEV_0001` (versioned ABI)

The legacy bring-up device is still supported by the KMD for bring-up/compatibility, but requires:

* enabling the emulator legacy device model (`emulator` feature `aerogpu-legacy`), and
* installing with the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/`.

During `DxgkDdiStartDevice`, the KMD reads BAR0 `AEROGPU_MMIO_REG_MAGIC`:

* If `AEROGPU_MMIO_MAGIC` (`"AGPU"`): use the versioned ABI and validate that the reported
  `AEROGPU_MMIO_REG_ABI_VERSION` major matches `AEROGPU_ABI_MAJOR` (reject major mismatches with
  `STATUS_NOT_SUPPORTED`).
* Otherwise: fall back to the legacy register map/ring format (the driver may log if the value is
  not the expected legacy `"ARGP"` magic).

Some legacy device models (including the in-tree emulator legacy device) also expose the versioned
`FEATURES_*`, `IRQ_*`, and `SCANOUT0_VBLANK_*` registers. The KMD will opportunistically use these
when present (and the reported feature bits contain no unknown values) so Win7 can receive vblank
interrupts and query scanline state even on the legacy `"ARGP"` device model.

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
   - Win7/WDDM note: dxgkrnl gates vblank delivery via `DxgkDdiControlInterrupt` using
     `DXGK_INTERRUPT_TYPE_CRTC_VSYNC`. The miniport ISR must notify vblank via
     `DXGKARGCB_NOTIFY_INTERRUPT.CrtcVsync.VidPnSourceId`.
5. **Error reporting (ABI 1.3+)**
   - If `AEROGPU_IRQ_ERROR` is set in `AEROGPU_MMIO_REG_IRQ_STATUS`, treat this as a fatal device error.
   - When `AEROGPU_FEATURE_ERROR_INFO` is set in `AEROGPU_MMIO_REG_FEATURES_LO/HI`, the device also latches a structured
     error payload into:
     - `AEROGPU_MMIO_REG_ERROR_CODE` (`enum aerogpu_error_code`)
     - `AEROGPU_MMIO_REG_ERROR_FENCE_LO/HI` (submission fence associated with the error, if known)
     - `AEROGPU_MMIO_REG_ERROR_COUNT` (monotonically increasing error counter)
   - Note: acknowledging/clearing `AEROGPU_IRQ_ERROR` via `AEROGPU_MMIO_REG_IRQ_ACK` does **not** clear the latched
     error payload; it remains valid until overwritten by a subsequent error.

Legacy (`ARGP`) note: some legacy device models mirror the above FEATURES/IRQ/vblank timing registers to allow
incremental migration. When a legacy device advertises `AEROGPU_FEATURE_VBLANK`, the KMD will enable vblank
delivery via the versioned `IRQ_*` block; **legacy fence/DMA completion interrupts remain on legacy
`INT_STATUS`/`INT_ACK`.**

## VidPN / mode selection (single monitor)

The Win7 AeroGPU KMD is intentionally conservative: it exposes a **single-head** display pipeline and prunes
VidPN proposals so Windows cannot select unsupported topologies, transforms, or pixel formats.

**Topology invariants**

* Exactly one VidPN source: `SourceId = 0`
* Exactly one target: `TargetId = 0`
* Exactly one present path: `0 → 0`
* No scaling (`VPPS_IDENTITY`) and no rotation (`VPPR_IDENTITY`)

**Format invariants**

The scanout path is 32bpp BGRA/XRGB-like. The KMD only claims support for formats that match the
existing `CommitVidPn` + `SetVidPnSourceAddress` behavior (4 bytes per pixel; pitch is treated as
`>= width*4` and may be aligned up):

* `D3DDDIFMT_X8R8G8B8`
* `D3DDDIFMT_A8R8G8B8` (byte-layout compatible; alpha is ignored by scanout)

**Mode list**

`DxgkDdiRecommendMonitorModes` and `DxgkDdiEnumVidPnCofuncModality` restrict Windows to a small, stable set of
progressive ~60Hz modes (the MVP scanout cadence is fixed to ~60 Hz; other refresh rates are rejected) derived from:

1. a preferred mode (`PreferredWidth`/`PreferredHeight` registry override → EDID preferred timing → fallback), plus
2. a built-in curated list (currently 640×480, 800×600, 1024×768, 1280×720, 1280×800, 1366×768, 1600×900, 1920×1080).

An optional `MaxWidth`/`MaxHeight` registry cap filters out larger modes to keep primary allocation sizes under
control on constrained guests.

This keeps Display Settings deterministic and avoids modes that the emulator scanout path does not support.

## Scanline / raster status (`DxgkDdiGetScanLine`)

The KMD implements `DxgkDdiGetScanLine` when the device advertises `AEROGPU_FEATURE_VBLANK` and
exposes the `SCANOUT0_VBLANK_*` timing registers. This includes both the primary versioned
(`AGPU`) device and the legacy (`ARGP`) device model when it mirrors the newer vblank registers
and reports `AEROGPU_FEATURE_VBLANK` via `FEATURES_LO/HI`.

This path is **approximate** (good enough for most D3D9-era `GetRasterStatus` callers):

- It derives a frame cadence from the device vblank counter/timestamps and the nominal vblank period.
- It maps elapsed time within the frame onto a synthetic `[0, height + vblank_lines)` scanline range, where `vblank_lines`
  is clamped to a small constant range (currently 20–40 lines).
- If vblank timing registers are not available, the driver falls back to a synthetic cadence based on
  `KeQueryInterruptTime()` (to avoid apps busy-waiting forever).

## Power management (`DxgkDdiSetPowerState`)

The KMD implements a minimal WDDM 1.1 `DxgkDdiSetPowerState` callback to improve robustness across adapter
power transitions (guest sleep/hibernate and PnP disable/enable).

Semantics:

- **Transition away from D0** (to any non-D0 state):
  - Block new ring submissions (the KMD will return `STATUS_DEVICE_NOT_READY` from ring push paths).
  - Disable the versioned IRQ block when present:
    - `AEROGPU_MMIO_REG_IRQ_ENABLE = 0`
    - acknowledge any pending bits via `AEROGPU_MMIO_REG_IRQ_ACK`.
  - On legacy devices, also acknowledge any pending fence interrupts via `INT_ACK` (legacy fence IRQ path).
  - If the device advertises `AEROGPU_FEATURE_CURSOR`, disable the hardware cursor and clear its GPA to stop DMA.
  - Disable scanout and clear its framebuffer GPA to stop scanout DMA while the adapter is powered down.
  - Stop ring execution (best-effort) by clearing ring/fence MMIO programming so the device will not touch freed ring memory.

- **Transition to D0**:
  - Block submissions while restoring state.
  - If resuming from non-D0, perform a best-effort **virtual reset**:
    - treat all in-flight work as completed (`LastCompletedFence = LastSubmittedFence`) and notify dxgkrnl,
    - reset ring pointers and reprogram ring/MMIO state (including `RING_CONTROL RESET` on the versioned ABI),
    - reprogram the optional fence page GPA when present.
  - Reset cached vblank timing state so scanline/vblank pacing does not consume stale timestamps across resume.
  - Restore the last programmed scanout configuration (best-effort; a full modeset may arrive later).
  - Restore cached hardware cursor state (shape buffer + position) when `AEROGPU_FEATURE_CURSOR` is available.
  - Restore `IRQ_ENABLE` to the cached enable mask when an ISR has been registered.

Related robustness:

- dbgctl `DxgkDdiEscape` query ops avoid touching MMIO while the adapter is not in D0; they return cached state where possible.

This power callback is intentionally minimal: it prioritizes avoiding stuck IRQ/vblank state after resume over
preserving in-flight rendering across a power cycle.

### Manual validation (Win7 guest)

To validate power-transition robustness end-to-end:

1. Capture a baseline driver snapshot:
   - `aerogpu_dbgctl --status`
2. Exercise **sleep/resume** (and/or hibernate/resume) in the Win7 guest.
3. Exercise a PnP power transition by **disabling and re-enabling** the AeroGPU adapter:
   - Device Manager → Display adapters → AeroGPU → Disable / Enable
4. Re-run:
   - `aerogpu_dbgctl --status`
   - The Win7 guest test suite (recommended: `drivers/aerogpu/tests/win7/bin/aerogpu_test_runner.exe`)

Expected: no IRQ storms or hangs during the transition, the desktop returns, and fences/ring state continue to advance
after resume.

## Post-display ownership (boot/shutdown handoff)

Windows may call the WDDM 1.1 post-display ownership DDIs during boot, shutdown, and other
transitions:

- `DxgkDdiStopDeviceAndReleasePostDisplayOwnership`
- `DxgkDdiAcquirePostDisplayOwnership`

The AeroGPU KMD implements **minimal, safe** behavior:

- On **release**, it disables scanout and disables vblank IRQ delivery (so the device stops
  continuously reading guest memory during the handoff).
- On **acquire**, it re-programs scanout registers using the last cached mode + framebuffer address
  (from `DxgkDdiSetVidPnSourceAddress`) and restores vblank IRQ enable state if it was previously
  enabled by dxgkrnl.

Limitations:

- The driver does **not** snapshot the last scanout into a driver-owned buffer, so a transition may
  briefly blank the display instead of preserving the last frame.
- This is best-effort and intentionally tolerant of being called during partial initialization or
  teardown (e.g. missing BAR mapping).

## Stable `alloc_id` / `share_token` (shared allocations)

To support D3D9Ex + DWM redirected surfaces and other cross-process shared allocations, AeroGPU relies on stable identifiers:

- `alloc_id` (32-bit, nonzero): UMD-owned allocation ID used by the per-submit allocation table (`alloc_id → {gpa, size_bytes, flags}`).
- `share_token` (64-bit, nonzero for shared allocations): KMD-owned stable token used by the AeroGPU command stream shared-surface ops (`EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`).

For robustness against Win7's varying `CloseAllocation` / `DestroyAllocation` call patterns, the KMD also maintains an adapter-global open refcount keyed by `share_token`. When the final cross-process allocation wrapper for a shared surface is released, the KMD emits `RELEASE_SHARED_SURFACE { share_token }` (a best-effort internal ring submission) so the host can remove the `share_token → resource` mapping used for future `IMPORT_SHARED_SURFACE` calls.

These values live in **WDDM allocation private driver data** (`aerogpu_wddm_alloc_priv` / `aerogpu_wddm_alloc_priv_v2`):

- The UMD supplies `alloc_id`/`flags` (and optional metadata) to the KMD.
- The KMD writes back `size_bytes` and (for shared allocations) a stable 64-bit `share_token` during `DxgkDdiCreateAllocation` and again during `DxgkDdiOpenAllocation`.
- For **shared allocations**, dxgkrnl preserves the blob and returns the exact same bytes on `OpenResource`/`DxgkDdiOpenAllocation` in another process, ensuring both processes observe identical IDs.
- Do **not** derive `share_token` from the numeric value of the D3D shared `HANDLE`: handle values are process-local and not stable cross-process.
- The KMD stores the IDs in its allocation bookkeeping and uses `alloc_id` when building the per-submit allocation table for the emulator.
- To avoid silent corruption, the KMD rejects submissions where the allocation list contains the same `alloc_id` for different backing base addresses (`gpa`). (Size may vary due to alignment; the allocation table uses the maximum observed size for a given `alloc_id`.)

The preserved private-data layout is defined in:

- `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h` (canonical definition)
- `drivers/aerogpu/protocol/aerogpu_alloc.h` (stable wrapper include)

### Submission-time `READONLY` (`aerogpu_alloc_entry.flags`)

For versioned ABI submissions (`aerogpu_ring.h`), the KMD can attach a per-submit allocation table (`aerogpu_alloc_table`) so the host can resolve `alloc_id → (gpa, size, flags)` safely for the **current** submission.

`AEROGPU_ALLOC_FLAG_READONLY` is derived **per submission** from the WDDM allocation list access metadata:

- If the runtime marks an allocation as **not written** by the submission (`DXGK_ALLOCATIONLIST::WriteOperation == 0`), the KMD sets `AEROGPU_ALLOC_FLAG_READONLY` for that alloc-table entry.
- The host must reject any command that would write back into guest memory for a READONLY allocation (e.g. `COPY_*` with `WRITEBACK_DST`), preventing guest command streams from requesting writeback into allocations that were not declared writable for that submission.
- If the KMD cannot reliably determine write access for an allocation, it leaves READONLY clear (fail-open for compatibility) and emits a DBG-only, rate-limited log.

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
      pwsh ci/build-aerogpu-dbgctl.ps1 -ToolchainJson out/toolchain.json
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
  4. After reboot, install the signed driver package from the copied package directory:
      ```bat
      cd C:\path\to\out\packages\aerogpu\x64
      :: CI packages stage aerogpu_dx11.inf (DX11-capable) by default.
      :: install.cmd prefers aerogpu_dx11.inf when present at the package root.
      packaging\win7\install.cmd
      :: Or install explicitly via pnputil:
      pnputil -i -a aerogpu_dx11.inf
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

### Optional tracing: `DxgkDdiCreateAllocation` flags

DXGI swapchain backbuffers are often non-shared, single-allocation resources, so they may not show up in the default
CreateAllocation debug logging.

For bring-up/debugging, the KMD maintains a small ring buffer of recent `DxgkDdiCreateAllocation` events and exposes it
via the dbgctl escape `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` (see `drivers/aerogpu/tools/win7_dbgctl`).

On a Win7 guest, `aerogpu_dbgctl.exe` is shipped on the Guest Tools ISO/zip under:

- `<GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
- `<GuestToolsDrive>:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`

```cmd
cd /d <GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin
aerogpu_dbgctl.exe --dump-createalloc
```

If your Guest Tools ISO is mounted as `X:` (common), these are copy/pastable:

```cmd
:: Win7 x64:
X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --dump-createalloc
:: Win7 x86:
X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --dump-createalloc
```

This includes `flags_in` (the incoming `DXGK_ALLOCATIONINFO::Flags.Value` from dxgkrnl/runtime) and `flags_out` (after the
miniport applies its required bits, currently `CpuVisible` + `Aperture`).

For additional verbosity in checked builds (`DBG=1`), build with:

* `AEROGPU_KMD_TRACE_CREATEALLOCATION=1`

This logs the first handful of `DxgkDdiCreateAllocation` calls via `DbgPrintEx` and prints `DXGK_ALLOCATIONINFO::Flags.Value`
both before and after the KMD applies its required bits.

## Escape channel

`DxgkDdiEscape` supports bring-up/debug queries. The stable Escape packet header/common ops
are defined in `drivers/aerogpu/protocol/aerogpu_escape.h`; additional bring-up/tooling ops
are defined in `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h`.

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2` (see `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h`)
  - returns the detected device ABI (`detected_mmio_magic`), ABI version, and (for versioned devices) feature bits
  - older tools may use the legacy `AEROGPU_ESCAPE_OP_QUERY_DEVICE` response (see `drivers/aerogpu/protocol/aerogpu_escape.h`; legacy ABI details in `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`)

Additional debug/control escapes used by `drivers/aerogpu/tools/win7_dbgctl`:

- `AEROGPU_ESCAPE_OP_QUERY_FENCE` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) (see `aerogpu_dbgctl_escape.h`)
  - For `AEROGPU_DBGCTL_RING_FORMAT_AGPU`, the v2 dump returns a recent **tail window** of descriptors ending at `tail - 1`
    (newest is `desc[desc_count - 1]`) so tooling/tests can observe recently completed submissions even when the pending
    `[head, tail)` region is drained quickly.
- `AEROGPU_ESCAPE_OP_READ_GPA` (see `aerogpu_dbgctl_escape.h`)
  - debug-only: allows bring-up tooling to read **bounded** slices of guest physical memory for GPU-owned buffers
    (used by `aerogpu_dbgctl.exe --read-gpa`, `--dump-scanout-bmp`/`--dump-scanout-png`, `--dump-cursor-bmp`/`--dump-cursor-png`,
    and `--dump-last-submit` (alias: `--dump-last-cmd`))
  - disabled by default (returns `STATUS_NOT_SUPPORTED` unless explicitly enabled)
    - enable via `HKLM\SYSTEM\CurrentControlSet\Services\aerogpu\Parameters\EnableReadGpaEscape` (REG_DWORD=1)
    - also requires a privileged caller (Administrator and/or `SeDebugPrivilege`)
  - safety: the KMD enforces a hard maximum payload per call and restricts reads to driver-tracked GPU-related regions
    (e.g. pending submission buffers, ring/fence, scanout/cursor framebuffers). It is not intended to be a generic
    physical-memory read primitive.
- `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (see `aerogpu_dbgctl_escape.h`)
- `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` (see `aerogpu_dbgctl_escape.h`)
  - disabled by default (returns `STATUS_NOT_SUPPORTED` unless explicitly enabled)
    - enable via `HKLM\SYSTEM\CurrentControlSet\Services\aerogpu\Parameters\EnableMapSharedHandleEscape` (REG_DWORD=1)
    - also requires a privileged caller (Administrator and/or `SeDebugPrivilege`)
- `AEROGPU_ESCAPE_OP_SELFTEST` (see `aerogpu_dbgctl_escape.h`)

These are intended for a small user-mode tool to validate KMD↔emulator communication early.

Note: `AEROGPU_ESCAPE_OP_QUERY_VBLANK` is feature-gated; it returns `STATUS_NOT_SUPPORTED`
unless `AEROGPU_FEATURE_VBLANK` is present in `FEATURES_LO/HI` (works for both legacy
bring-up devices (see `docs/abi/aerogpu-pci-identity.md`) and versioned `PCI\VEN_A3A0&DEV_0001`
devices that expose those registers).
