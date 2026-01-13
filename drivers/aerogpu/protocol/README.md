# AeroGPU Guest↔Emulator Protocol (PCI/MMIO + Rings + Command Stream)

This directory defines the **stable, versioned ABI contract** between:

- the Windows 7 AeroGPU WDDM driver stack (KMD + UMD), and
- the Aero emulator’s virtual GPU device model.

The contract is expressed as C/C++ headers suitable for **Windows 7-targeted WDK builds** (WDK10+ supported) and for host-side parsing.

> Note: This directory contains both the **versioned ABI** (the canonical long-term contract) and a
> **legacy bring-up ABI** (`legacy/aerogpu_protocol_legacy.h`).
>
> The legacy header is kept for reference and for the emulator’s legacy AeroGPU device model
> (`crates/emulator/src/devices/pci/aerogpu_legacy.rs`, feature `emulator/aerogpu-legacy`).
>
> The in-tree Win7 KMD does **not** include the legacy protocol header directly (it uses the minimal internal shim
> `drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h`), but it can still speak the legacy transport for compatibility.
>
> The in-tree Win7 driver package binds only to the versioned device by default; install against the legacy device model
> using `drivers/aerogpu/packaging/win7/legacy/` and enable the emulator legacy device model feature.
>
> Current status: UMDs in this repo emit the versioned command stream (`aerogpu_cmd.h`). The Win7 KMD supports both the versioned and legacy submission transports and auto-detects which ABI is active based on the device MMIO magic; see `drivers/aerogpu/kmd/README.md`.

> Note: The repository also contains older/prototype GPU ABIs with similar “AeroGPU” naming.
> New work intended for Windows 7 should target the protocol in this directory.
> See `docs/graphics/aerogpu-protocols.md` for an overview of the in-tree protocols.

## Files

- `aerogpu_pci.h` – PCI IDs, BAR layout, MMIO register map, shared enums.
- `aerogpu_ring.h` – ring header layout, submission descriptor, allocation table, fence page.
- `aerogpu_cmd.h` – command stream packet formats and opcodes (“AeroGPU IR”).
- `aerogpu_umd_private.h` – `DXGKQAITYPE_UMDRIVERPRIVATE` blob used by UMDs/tools to discover active ABI + feature bits.
- `aerogpu_wddm_alloc.h` – WDDM allocation private-data contract (UMD↔KMD via dxgkrnl; preserved across OpenResource) for stable per-allocation metadata (`alloc_id`/`share_token`) across CreateAllocation/OpenAllocation.
- `aerogpu_win7_abi.h` – driver-private WOW64-stable user↔kernel ABI blobs (no pointers; fixed layout across x86/x64).
- `aerogpu_escape.h` – driver-private `DxgkDdiEscape` packet header + base ops (UMD/tool ↔ KMD control channel).
- `aerogpu_dbgctl_escape.h` – driver-private `DxgkDdiEscape` packets used by bring-up tooling (`drivers/aerogpu/tools/win7_dbgctl`). (Layered on top of `aerogpu_escape.h`; ring dumps report canonical submit fields like `cmd_gpa`/`cmd_size_bytes`/`signal_fence`.)
- `legacy/aerogpu_protocol_legacy.h` – legacy bring-up ABI (monolithic header; PCI `1AED:0001`). Deprecated and kept for backwards compatibility/testing.
- `vblank.md` – vblank IRQ + timing registers required for Win7 DWM/D3D pacing.

## Source of truth, mirrors, and conformance tests

The **normative A3A0 protocol contract** is defined by these headers:

- `drivers/aerogpu/protocol/aerogpu_pci.h`
- `drivers/aerogpu/protocol/aerogpu_ring.h`
- `drivers/aerogpu/protocol/aerogpu_cmd.h`

The emulator maintains **Rust + TypeScript mirrors** of the same ABI for host-side parsing and tooling:

- Rust: `emulator/protocol/aerogpu/*.rs` (crate `aero-protocol`)
- TypeScript: `emulator/protocol/aerogpu/*.ts`

When any of the normative headers change, the mirrors **must** be updated in lock-step. CI enforces this via conformance tests that compile and run a small C “ABI dump” helper (`emulator/protocol/tests/aerogpu_abi_dump.c`) and compare:

- constant values (MMIO offsets, flags, enum values),
- struct sizes and field offsets, and
- opcode/packet coverage.

### Running the conformance tests locally

```bash
cargo test -p aero-protocol --locked
npm run test:protocol
```

## ABI variants and PCI IDs

This directory currently contains two PCI/MMIO ABIs:

- **Versioned ABI (current)** – `aerogpu_pci.h` + `aerogpu_ring.h` + `aerogpu_cmd.h`, PCI `A3A0:0001` (`VEN_A3A0&DEV_0001`).
  - Uses the major/minor compatibility model below (major breaking, minor forwards compatible).
  - Emulator device model: `crates/emulator/src/devices/pci/aerogpu.rs`.
- **Legacy bring-up ABI (deprecated)** – `legacy/aerogpu_protocol_legacy.h`, PCI `1AED:0001` (legacy `"ARGP"` device model).
  - Emulator device model: `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (feature `emulator/aerogpu-legacy`).

Both IDs are project-specific (not PCI-SIG assigned). Both identify as a VGA-compatible display controller (base class `0x03`, subclass `0x00`, prog-if `0x00`).

The in-tree Win7 packaging INFs (`drivers/aerogpu/packaging/win7/*.inf`) bind to the versioned `VEN_A3A0&DEV_0001` device by default.
If you are intentionally using the deprecated legacy device model/ABI (legacy `"ARGP"` / PCI `1AED:0001`), use the INFs under
`drivers/aerogpu/packaging/win7/legacy/` instead (and enable the emulator legacy device model via feature
`emulator/aerogpu-legacy`).

For a quick overview of the canonical AeroGPU PCI IDs (new vs legacy) and which emulator device
models implement each ABI, see: `docs/abi/aerogpu-pci-identity.md`.

## Versioning model

The versioned ABI uses a **major.minor** version:

- `ABI_MAJOR` changes are **breaking**. A driver built for major *N* must not drive a device advertising major *N+1*.
- `ABI_MINOR` changes are **forwards compatible** (drivers should accept a higher minor within the same major). Minor bumps may add:
  - new MMIO registers (in currently-reserved space),
  - new command opcodes,
  - new optional features bits,
  - larger structs **only** when an explicit `size_bytes` is present (or via new opcodes).

The device reports `AEROGPU_ABI_VERSION_U32` via MMIO `AEROGPU_MMIO_REG_ABI_VERSION`.

## Endianness, alignment, packing

- The guest is x86/x64 Windows: **little-endian**. All multi-byte fields are little-endian.
- Command buffers and tables are sequences of packed structs. Headers use `#pragma pack(push, 1)` where layout must be exact.
- Any structure that can vary in size uses an explicit `size_bytes` field, and/or a packet header with `size_bytes`.

## BAR / MMIO overview

BAR0 exposes a memory-mapped register block (`AEROGPU_PCI_BAR0_SIZE_BYTES`, currently 64 KiB).

The key MMIO responsibilities are:

1. **Discovery**
   - `MAGIC`, `ABI_VERSION`, and `FEATURES`.
2. **Command transport**
   - Ring GPA/size programming.
   - Doorbell.
   - IRQ status/ack.
3. **Completion**
   - Completed fence value in MMIO and optionally a shared fence page.
4. **Error reporting** (ABI 1.3+)
   - `AEROGPU_IRQ_ERROR` indicates a device-side validation/execution failure.
   - When `AEROGPU_FEATURE_ERROR_INFO` is present, additional read-only MMIO registers expose the
     most recent error code/fence/count (`ERROR_*`) for low-bandwidth diagnostics from the guest.
5. **Display output**
   - Scanout0 configuration (width/height/format/pitch/framebuffer GPA).
   - Optional vblank timing registers + vblank IRQ (required for Win7 DWM pacing when `AEROGPU_FEATURE_VBLANK` is set; see `vblank.md`).
   - Cursor configuration is reserved and feature-gated.

See `aerogpu_pci.h` for exact offsets and bit definitions.

### Device feature bits

The device reports optional capability bits in `FEATURES_LO/HI`.

Notable bits include:

- `AEROGPU_FEATURE_VBLANK`: implements vblank IRQ + timing registers (Win7 pacing).
- `AEROGPU_FEATURE_TRANSFER`: supports transfer/copy commands (e.g. `COPY_BUFFER`,
  `COPY_TEXTURE2D`), including optional **writeback into guest backing memory**
  for GPU→CPU readback. (Introduced in ABI 1.1.)
- `AEROGPU_FEATURE_ERROR_INFO`: exposes additional MMIO registers describing why
  `AEROGPU_IRQ_ERROR` was raised (last error code + fence + count). (Introduced in ABI 1.3.)

## Error reporting (IRQ_ERROR + error-info registers)

`IRQ_ERROR` (`AEROGPU_IRQ_ERROR`) is raised by the device when a submission fails validation or
execution. Since the IRQ bit is only a boolean, the device may optionally expose *error info*
registers when `AEROGPU_FEATURE_ERROR_INFO` is set in `FEATURES_LO/HI`:

- `AEROGPU_MMIO_REG_ERROR_CODE` (`u32`)
- `AEROGPU_MMIO_REG_ERROR_FENCE_LO/HI` (`u64`)
- `AEROGPU_MMIO_REG_ERROR_COUNT` (`u32`, saturating)

### Semantics

- On each error, the device updates:
  - `ERROR_CODE` to a stable `enum aerogpu_error_code` (`AEROGPU_ERROR_*`) value,
  - `ERROR_FENCE` to the associated submission fence (or `0` if not applicable), and
  - `ERROR_COUNT` (monotonic, saturating at `0xffffffff`).
- The values are **sticky**: they persist until overwritten by a subsequent error or until the
  device is reset (e.g. VM/emulator restart). They are not cleared by `IRQ_ACK`.

Guest drivers/tools must only read these registers when `AEROGPU_FEATURE_ERROR_INFO` is present.

## Command submission transport

### Ring setup (KMD)

1. Allocate a contiguous guest memory region for the ring:
   - at least `sizeof(aerogpu_ring_header) + entry_count * entry_stride_bytes`, where
     `entry_stride_bytes >= sizeof(aerogpu_submit_desc)`.
2. Initialize `aerogpu_ring_header` (magic, abi_version, entry_count, etc).
3. Program MMIO:
   - `RING_GPA_LO/HI`
   - `RING_SIZE_BYTES`
   - set `RING_CONTROL_ENABLE`
4. Optionally allocate and program a shared fence page:
   - program `FENCE_GPA_LO/HI` (only if `AEROGPU_FEATURE_FENCE_PAGE` is set).
5. Enable interrupts via `IRQ_ENABLE` (optional; polling is allowed).

### Submitting work (KMD)

For each submission:

1. Write an `aerogpu_submit_desc` into the next ring slot.
2. Update `ring->tail` (monotonic counter).
3. Write to `AEROGPU_MMIO_REG_DOORBELL` to notify the device.

The device consumes entries in order, updating `ring->head`.

Submission descriptor validation rules (from `aerogpu_ring.h`):

- `cmd_gpa` and `cmd_size_bytes` must be both zero (empty submission) or both non-zero.
- When `cmd_gpa/cmd_size_bytes` are non-zero, `cmd_gpa + cmd_size_bytes` must not overflow `u64`.
- `alloc_table_gpa` and `alloc_table_size_bytes` must be both zero (absent) or both non-zero (present).
- When `alloc_table_gpa/alloc_table_size_bytes` are non-zero, `alloc_table_gpa + alloc_table_size_bytes`
  must not overflow `u64`.

### Allocation table (optional)

Each submission may provide an optional **sideband allocation table**:

- The submit descriptor points to `alloc_table_gpa/alloc_table_size_bytes`.
- The table contains an `aerogpu_alloc_table_header` followed by `aerogpu_alloc_entry` items.
- Command packets identify protocol objects via `aerogpu_handle_t` (`resource_handle`, `buffer_handle`, `texture_handle`, etc).
- When a resource uses guest memory as its backing store, packets may refer to backing memory by `backing_alloc_id` (an `alloc_id` from this table).

This enables compact command streams that use small IDs instead of repeating GPAs.

**See [`allocation-table.md`](./allocation-table.md)** for the full end-to-end contract, including:
validation rules, `backing_offset_bytes` / `row_pitch_bytes` interpretation, aliasing semantics, and
`READONLY` writeback behavior.

#### `alloc_id` ownership and stability

`alloc_id` values are owned by the **guest UMD**:

- The UMD chooses a non-zero `alloc_id` for each WDDM allocation it wants the host to
  be able to reference.
- To avoid collisions with KMD-synthesized IDs, UMD-generated IDs must keep the
  high bit clear (`alloc_id <= 0x7fffffff`). The KMD reserves
  `0x80000000..0xffffffff` for internal/standard allocations (see
  `aerogpu_wddm_alloc.h`).
- For **shared allocations** (cross-process `OpenResource`), the UMD must embed
  the chosen `alloc_id` into the preserved WDDM allocation private data blob so
  it can be recovered in another process. See `aerogpu_wddm_alloc.h`.
 - `alloc_id` values for shared allocations should avoid collisions across guest
   processes: DWM may compose many redirected surfaces from different processes
   in a single submission, and the per-submit allocation table is keyed by
   `alloc_id`. `alloc_id` must be non-zero and stay in the UMD-owned range
   (`alloc_id <= 0x7fffffff`). A simple implementation is to allocate it from a
   cross-process monotonic counter (e.g. named shared memory / file mapping +
   atomic increment) and clamp/mask it into range (skipping 0).
 - **Collision policy:** `alloc_id` must uniquely identify a backing allocation
   within a submission. Duplicate `alloc_id` values are only allowed when they
   refer to the **same underlying backing** (same guest physical address), which
   can happen when a shared resource is opened multiple times and yields distinct
   per-process allocation handles.
   - If the KMD observes the same `alloc_id` mapping to **different** GPAs while
     building the per-submit allocation table, it must reject the submission
     deterministically (current Win7 KMD: `STATUS_INVALID_PARAMETER`).
   - If the host observes a malformed/ambiguous allocation table (including
     duplicate `alloc_id` entries), it must treat the submission as failed, but
     must still complete/advance the fence (to avoid deadlock).
 - For D3D9Ex shared surfaces, `share_token` (as used by
   `EXPORT_SHARED_SURFACE`/`IMPORT_SHARED_SURFACE`) must be stable across guest
   processes and must not be derived from process-local handle values.
  Canonical contract: the Win7 KMD generates a stable non-zero `share_token` and
   persists it in the preserved WDDM allocation private driver data blob
   (`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
   dxgkrnl preserves the blob and returns the exact same bytes on cross-process
   `OpenResource`, so both processes observe the same `share_token`.
  - **Collision policy:** `share_token` must be treated as a **globally unique**
    identifier. The host must detect and reject:
    - `EXPORT_SHARED_SURFACE` attempting to bind an already-exported token to a
      different resource, and
    - `EXPORT_SHARED_SURFACE` attempting to reuse a token that was previously
      released (`RELEASE_SHARED_SURFACE`), and
    - `IMPORT_SHARED_SURFACE` using an unknown/released token.
    Current host behavior: mark the submission as failed (error/IRQ), but still
    advance the fence.
 - The KMD treats `alloc_id` as an **input** (UMD→KMD), validates it, and forwards
   the corresponding GPA/size to the host in `aerogpu_alloc_entry`.

See `aerogpu_wddm_alloc.h` for the exact private-data layout used to persist
`alloc_id` across CreateAllocation/OpenAllocation.

#### Guest-memory access rules

- If any command in a submission requires the host to **READ or WRITE guest backing
  memory** for an allocation (e.g., destination writeback for CPU readback), the
  submission MUST provide an allocation table entry for that allocation ID.
- Commands that describe CPU writes to guest memory (e.g. `RESOURCE_DIRTY_RANGE`) are only meaningful for
  guest-backed resources (`backing_alloc_id != 0`). Host-owned resources must upload bytes explicitly via
  `UPLOAD_RESOURCE`.
- The host must reject (validation error) any writeback to allocations marked
  `AEROGPU_ALLOC_FLAG_READONLY`.

## Fence / completion model

Fences are **monotonic 64-bit** values chosen by the guest.

- Each submission provides `signal_fence`.
- The device updates the completed fence to at least that value once the submission is finished.
- If a submission requests any writeback into guest backing memory, the device MUST
  only signal/advance `completed_fence` after all such writebacks are complete and
  visible to the guest.
- Completion is observable via:
  - MMIO `COMPLETED_FENCE_LO/HI` (always available), and
  - optionally `aerogpu_fence_page.completed_fence` if a fence page is configured.

If interrupts are enabled, the device raises `AEROGPU_IRQ_FENCE` when the completed fence advances (unless the submission requested `AEROGPU_SUBMIT_FLAG_NO_IRQ`).

## Command stream (“AeroGPU IR”)

### Structure

Command buffers are byte streams in guest memory:

1. `aerogpu_cmd_stream_header`
2. A sequence of packets, each beginning with `aerogpu_cmd_hdr`.

`aerogpu_cmd_stream_header.size_bytes` is the number of bytes used by the stream (including the
header). It must be `>= sizeof(aerogpu_cmd_stream_header)` and `<= aerogpu_submit_desc.cmd_size_bytes`;
any trailing bytes in the command buffer beyond `size_bytes` are ignored (forward-compatible padding /
capacity).

`aerogpu_cmd_hdr.size_bytes` provides the packet length for skipping unknown opcodes.

### Forward-compat rules

Consumers (the emulator) must:

- Validate that the stream header magic matches and the major ABI is supported.
- Skip unknown opcodes using `size_bytes`.
- Require `size_bytes` to be at least `sizeof(aerogpu_cmd_hdr)` and 4-byte aligned.

Producers (the driver) must:

- Emit correct `size_bytes` for every packet.
- Zero all reserved fields (unless a field is explicitly repurposed by an encoding rule in the
  headers, e.g. `aerogpu_shader_stage_ex` via `reserved0` when `shader_stage==COMPUTE`).
- Only use features/opcodes indicated by the ABI version and feature bits.

### Minimal opcode set

The initial protocol defines an IR sufficient for D3D9-style rendering and can be extended for D3D10/11:

- Resource management: create/destroy buffer/texture, dirty range notifications.
  - `RESOURCE_DIRTY_RANGE` describes CPU writes to **guest-backed** memory (`backing_alloc_id != 0`).
    Host-owned resources must use `UPLOAD_RESOURCE`.
- Shader upload: DXBC blob upload + bind.
- Pipeline state: blend/depth/raster state setting.
- Render state: render targets, viewport, scissor.
- Input assembler: vertex/index buffers.
- Draw, draw indexed.
- Clear.
- Present / PresentEx (D3D9Ex flags).
- Shared surface export/import (cross-process DWM redirected surfaces; MVP: single-allocation only).
- Flush (explicit scheduling point).

See `aerogpu_cmd.h` for the full opcode list and packet layouts.

### Debugging: decoding raw command stream dumps

When debugging Win7 guest driver issues it’s often useful to inspect the **raw AeroGPU command stream** that was most recently submitted
(for example, a dump captured via kernel/UMD instrumentation or any other mechanism that writes the byte stream to disk).

The host-side tool `aero-gpu-trace-replay` includes a small decoder that prints a stable, grep-friendly opcode listing:

```bash
# From the repo root:
cargo run -p aero-gpu-trace-replay -- decode-cmd-stream <cmd-stream.bin>

# Fail on unknown opcodes (default is forward-compatible: prints UNKNOWN and continues):
cargo run -p aero-gpu-trace-replay -- decode-cmd-stream --strict <cmd-stream.bin>
```

The input file must contain the raw `aerogpu_cmd_stream_header` followed by the packet sequence.
Output format is one packet per line:

```
0x00000018 CreateBuffer size_bytes=40 ...
0x00000040 UploadResource size_bytes=36 ...
```

### Shared-surface MVP limitation (single allocation)

The shared-surface ABI (`EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`) currently assumes that a shared surface is backed by a **single** WDDM allocation (one contiguous guest memory range). Many WDDM resources can be split across multiple allocations (mips/arrays/planes).

To keep the contract simple and to match Win7 DWM redirected surfaces, the AeroGPU driver stack rejects shared resources that would require multiple allocations (MVP policy: `mip_levels=1` (reject `MipLevels/Levels=0`, which requests a full mip chain), `array_layers=1`).

## End-to-end flow (Windows → emulator)

1. **UMD encodes** AeroGPU IR packets into a command buffer in guest memory.
2. **KMD submits** the buffer by writing an `aerogpu_submit_desc` to the shared ring and ringing the MMIO doorbell.
3. The **emulator consumes** ring entries, parses command buffers, and translates IR to WebGPU operations.
4. On completion, the emulator **signals the fence** (MMIO + optional fence page) and optionally raises an IRQ.
