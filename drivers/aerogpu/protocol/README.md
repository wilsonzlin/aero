# AeroGPU Guest↔Emulator Protocol (PCI/MMIO + Rings + Command Stream)

This directory defines the **stable, versioned ABI contract** between:

- the Windows 7 AeroGPU WDDM driver stack (KMD + UMD), and
- the Aero emulator’s virtual GPU device model.

The contract is expressed as C/C++ headers suitable for **WDK** builds and for host-side parsing.

> Note: This directory contains both the **new versioned ABI** (the long-term contract) and a
> **legacy bring-up ABI** (`aerogpu_protocol.h`). The legacy header is kept for compatibility with
> the current Win7 KMD and the emulator’s legacy AeroGPU device model (`crates/emulator/src/devices/pci/aerogpu_legacy.rs`);
> it is **not** the source of truth for the versioned ABI described below (implemented by
> `crates/emulator/src/devices/pci/aerogpu.rs`).
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
- `aerogpu_wddm_alloc.h` – WDDM allocation private-data contract for stable `alloc_id` / `share_token` exchange across CreateAllocation/OpenAllocation.
- `aerogpu_dbgctl_escape.h` – driver-private `DxgkDdiEscape` packets used by bring-up tooling (`drivers/aerogpu/tools/win7_dbgctl`). (Currently layered on top of the legacy `aerogpu_protocol.h` Escape header.)
- `aerogpu_protocol.h` – **legacy bring-up ABI** (monolithic header; PCI `1AED:0001`).
- `vblank.md` – vblank IRQ + timing registers required for Win7 DWM/D3D pacing.

## ABI variants and PCI IDs

This directory currently contains two PCI/MMIO ABIs:

- **Versioned ABI (current)** – `aerogpu_pci.h` + `aerogpu_ring.h` + `aerogpu_cmd.h`, PCI `A3A0:0001` (`VEN_A3A0&DEV_0001`).
  - Uses the major/minor compatibility model below (major breaking, minor forwards compatible).
- **Legacy bring-up ABI** – `aerogpu_protocol.h`, PCI `1AED:0001` (`VEN_1AED&DEV_0001`).

Both IDs are project-specific (not PCI-SIG assigned). Both identify as a display controller (`0x03`), VGA-compatible subclass (`0x00`).

Both IDs may appear in Windows driver INFs during the migration period.

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
4. **Display output**
   - Scanout0 configuration (width/height/format/pitch/framebuffer GPA).
   - Optional vblank timing registers + vblank IRQ (required for Win7 DWM pacing when `AEROGPU_FEATURE_VBLANK` is set; see `vblank.md`).
   - Cursor configuration is reserved and feature-gated.

See `aerogpu_pci.h` for exact offsets and bit definitions.

## Command submission transport

### Ring setup (KMD)

1. Allocate a contiguous guest memory region for the ring:
   - at least `sizeof(aerogpu_ring_header) + entry_count * sizeof(aerogpu_submit_desc)`.
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

### Allocation table (optional)

Each submission may provide an optional **sideband allocation table**:

- The submit descriptor points to `alloc_table_gpa/alloc_table_size_bytes`.
- The table contains an `aerogpu_alloc_table_header` followed by `aerogpu_alloc_entry` items.
- Command packets identify protocol objects via `aerogpu_handle_t` (`resource_handle`, `buffer_handle`, `texture_handle`, etc).
- When a resource uses guest memory as its backing store, packets may refer to backing memory by `backing_alloc_id` (an `alloc_id` from this table).

This enables compact command streams that use small IDs instead of repeating GPAs.

#### Stable `alloc_id` and shared-surface `share_token`

`alloc_id` is a **stable identifier assigned by the Windows KMD** to a WDDM allocation (not a per-submit index).
The KMD must persist this value in **WDDM allocation private driver data** so that the UMD can retrieve it on:

- allocation create (`DxgkDdiCreateAllocation`), and
- allocation open (`DxgkDdiOpenAllocation`) when a shared handle is imported by another guest process.

**Requirement:** the same underlying shared allocation must always report the same `alloc_id` across opens.

For cross-process shared surfaces (`AEROGPU_CMD_EXPORT_SHARED_SURFACE` / `AEROGPU_CMD_IMPORT_SHARED_SURFACE`),
the recommended scheme is:

- `share_token = (uint64_t)alloc_id`

See `aerogpu_wddm_alloc.h` for the exact private-data layout used to persist `alloc_id`/`share_token` across CreateAllocation/OpenAllocation.

## Fence / completion model

Fences are **monotonic 64-bit** values chosen by the guest.

- Each submission provides `signal_fence`.
- The device updates the completed fence to at least that value once the submission is finished.
- Completion is observable via:
  - MMIO `COMPLETED_FENCE_LO/HI` (always available), and
  - optionally `aerogpu_fence_page.completed_fence` if a fence page is configured.

If interrupts are enabled, the device raises `AEROGPU_IRQ_FENCE` when the completed fence advances (unless the submission requested `AEROGPU_SUBMIT_FLAG_NO_IRQ`).

## Command stream (“AeroGPU IR”)

### Structure

Command buffers are byte streams in guest memory:

1. `aerogpu_cmd_stream_header`
2. A sequence of packets, each beginning with `aerogpu_cmd_hdr`.

`aerogpu_cmd_hdr.size_bytes` provides the packet length for skipping unknown opcodes.

### Forward-compat rules

Consumers (the emulator) must:

- Validate that the stream header magic matches and the major ABI is supported.
- Skip unknown opcodes using `size_bytes`.
- Require `size_bytes` to be at least `sizeof(aerogpu_cmd_hdr)` and 4-byte aligned.

Producers (the driver) must:

- Emit correct `size_bytes` for every packet.
- Zero all reserved fields.
- Only use features/opcodes indicated by the ABI version and feature bits.

### Minimal opcode set

The initial protocol defines an IR sufficient for D3D9-style rendering and can be extended for D3D10/11:

- Resource management: create/destroy buffer/texture, dirty range notifications.
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

### Shared-surface MVP limitation (single allocation)

The shared-surface ABI (`EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`) currently assumes that a shared surface is backed by a **single** WDDM allocation (one contiguous guest memory range). Many WDDM resources can be split across multiple allocations (mips/arrays/planes).

To keep the contract simple and to match Win7 DWM redirected surfaces, the AeroGPU driver stack rejects shared resources that would require multiple allocations (MVP policy: `mip_levels=1`, `array_layers=1`).

## End-to-end flow (Windows → emulator)

1. **UMD encodes** AeroGPU IR packets into a command buffer in guest memory.
2. **KMD submits** the buffer by writing an `aerogpu_submit_desc` to the shared ring and ringing the MMIO doorbell.
3. The **emulator consumes** ring entries, parses command buffers, and translates IR to WebGPU operations.
4. On completion, the emulator **signals the fence** (MMIO + optional fence page) and optionally raises an IRQ.
