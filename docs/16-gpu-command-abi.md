# 16 - Experimental Virtual GPU Device + Command ABI (`aero-gpu-device`)

> Status: **experimental / legacy**.
>
> This document describes the standalone ABI used by `crates/aero-gpu-device` for
> deterministic host-side tests and trace plumbing.
>
> It is **NOT** the canonical Windows 7 WDDM AeroGPU guest↔emulator protocol.

This document defines an **experimental** emulator-side virtual GPU device model and a
guest↔host GPU command ABI.

## Canonical AeroGPU WDDM ABI (source of truth)

Contributors working on the real Windows 7 WDDM AeroGPU path should use the protocol
headers under:

- [`drivers/aerogpu/protocol/README.md`](../drivers/aerogpu/protocol/README.md)
  - `aerogpu_pci.h` (PCI IDs + BAR/MMIO)
  - `aerogpu_ring.h` (submission ring + fences)
  - `aerogpu_cmd.h` (command stream packets)
- [`emulator/protocol`](../emulator/protocol) (Rust/TypeScript mirror of the headers)

The canonical WDDM AeroGPU device models use project-specific (non-PCI-SIG) PCI IDs.
For the full rationale (why there are two) and the mapping to ABIs/device models, see:

- [`docs/abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md)

- `0xA3A0:0x0001` – new, versioned ABI (`drivers/aerogpu/protocol/aerogpu_pci.h`)
- `0x1AED:0x0001` – legacy bring-up ABI (`drivers/aerogpu/protocol/aerogpu_protocol.h`)

## Source of truth (this experimental ABI)

- `crates/aero-gpu-device/src/abi.rs`

---

## 1. Transport: PCI device + MMIO + doorbell

The GPU is exposed to the guest as a PCI device with a single MMIO BAR.

### PCI identification

- Experimental PCI vendor ID: `0xA0E0` *(used only by `aero-gpu-device`, not by the Win7 WDDM driver stack)*
- Device ID: `0x0001`
- Class code: `0x03` (Display controller)
- Subclass: `0x02` (3D controller)
- BAR0: MMIO registers, 4 KiB

These values are defined in `aero_gpu_device::abi::pci`.

### Interrupts

The device raises an interrupt when new completion entries are available.

In the current implementation this is modeled as a single INTx-style interrupt line (MSI/MSI-X can be added later without changing the command stream).

---

## 2. Shared memory regions (guest physical)

The guest allocates shared buffers in guest RAM and programs their **guest physical** base addresses via MMIO:

1. **Command ring** (guest → host): variable-length command records.
2. **Completion ring** (host → guest): variable-length completion records.
3. **Descriptor region** (guest → host): optional blob storage for future large descriptors/shader bytecode.

The current tests use host-side “synthetic guest” code to write into these rings to validate the ABI end-to-end.

---

## 3. MMIO register map (BAR0)

All registers are little-endian `u32` accesses.

| Offset | Name | R/W | Description |
|---:|---|:--:|---|
| `0x000` | `REG_ABI_VERSION` | R | `(ABI_MAJOR<<16) \| ABI_MINOR` |
| `0x100/0x104` | `REG_CMD_RING_BASE_{LO,HI}` | W | Guest physical base address of command ring |
| `0x108` | `REG_CMD_RING_SIZE` | W | Expected ring size (bytes). Host verifies it matches the ring header. |
| `0x110/0x114` | `REG_CPL_RING_BASE_{LO,HI}` | W | Guest physical base address of completion ring |
| `0x118` | `REG_CPL_RING_SIZE` | W | Expected ring size (bytes). |
| `0x120/0x124` | `REG_DESC_BASE_{LO,HI}` | W | Guest physical base address of descriptor region (optional) |
| `0x128` | `REG_DESC_SIZE` | W | Size of descriptor region (bytes) |
| `0x200` | `REG_DOORBELL` | W | Write any value to trigger command processing (batched) |
| `0x300` | `REG_INT_STATUS` | R | Pending interrupt bits |
| `0x304` | `REG_INT_MASK` | R/W | Interrupt enable mask |
| `0x308` | `REG_INT_ACK` | W | Write-1-to-clear interrupt bits |
| `0x310/0x314` | `REG_LAST_COMPLETED_SEQ_{LO,HI}` | R | Last completed command sequence number |
| `0x318/0x31C` | `REG_LAST_FAULT_SEQ_{LO,HI}` | R | Last faulting command sequence number |

Interrupt bits:

- `INT_STATUS_CPL_AVAIL` (bit 0): completion entries are available
- `INT_STATUS_FAULT` (bit 1): at least one command completed with a non-OK status

---

## 4. Ring buffer layout

Both command and completion rings use the same byte-ring structure:

```
base_paddr:
  +0x00  GpuRingHeader (64 bytes)
  +0x40  data[ring_size_bytes]
```

### `GpuRingHeader` (64 bytes)

Fields:

- `magic`: `"AGRN"`
- `abi_major`, `abi_minor`: ABI version for the ring
- `ring_size_bytes`: size of the data region (must be ≥ 256 and 8-byte aligned)
- `head`, `tail`: byte offsets within the data region

`head` is written by the consumer, `tail` by the producer.

### Ring records

The data region contains a sequence of variable-length **records**.

Every record begins with:

```
u32 magic
u32 size_bytes   // total record size including this header
```

Records must be 8-byte aligned (`size_bytes % 8 == 0`).

To wrap to the beginning of the ring without allowing a record to span the end, the producer writes a pad record:

- `magic = "AGPD"`
- `size_bytes = remaining_bytes_to_end`

The consumer treats `"AGPD"` as an internal marker and resets `head` to 0.

---

## 5. Command records (guest → host)

Command record magic: `"AGPC"`

Header layout (`GpuCmdHeader`, 24 bytes total):

| Offset | Type | Field |
|---:|---|---|
| 0x00 | `u32` | magic (`"AGPC"`) |
| 0x04 | `u32` | size_bytes |
| 0x08 | `u16` | opcode |
| 0x0A | `u16` | flags (reserved) |
| 0x0C | `u16` | abi_major |
| 0x0E | `u16` | abi_minor |
| 0x10 | `u64` | seq |
| 0x18 | bytes | payload |

Unknown opcodes are completed with status `UNSUPPORTED` and the device continues processing subsequent commands.

---

## 6. Completion records (host → guest)

Completion record magic: `"AGCP"`

Header layout (`GpuCompletion`, 24 bytes total):

| Offset | Type | Field |
|---:|---|---|
| 0x00 | `u32` | magic (`"AGCP"`) |
| 0x04 | `u32` | size_bytes |
| 0x08 | `u64` | seq |
| 0x10 | `u16` | opcode |
| 0x12 | `u16` | reserved |
| 0x14 | `u32` | status |

Status codes:

- `0`: OK
- `1`: INVALID_COMMAND
- `2`: INVALID_RESOURCE
- `3`: OUT_OF_BOUNDS
- `4`: UNSUPPORTED

---

## 7. Supported opcodes (v1.0)

Early milestones are focused on “draw a triangle and present”.

### Resource management

- `CREATE_BUFFER`
- `DESTROY_BUFFER`
- `WRITE_BUFFER` / `READ_BUFFER`
- `CREATE_TEXTURE2D`
- `DESTROY_TEXTURE`
- `WRITE_TEXTURE2D` / `READ_TEXTURE2D`

#### Usage bit expectations (enforced by the WebGPU backend)

To keep the ABI explicit and make backends validate intent:

- `WRITE_BUFFER` requires the buffer to have `TRANSFER_DST`.
- `READ_BUFFER` requires the buffer to have `TRANSFER_SRC`.
- `WRITE_TEXTURE2D` requires the texture to have `TRANSFER_DST`.
- `READ_TEXTURE2D` and `PRESENT` require the texture to have `TRANSFER_SRC`.

### Rendering / state

- `SET_RENDER_TARGET`
- `CLEAR`
- `SET_VIEWPORT`
- `SET_PIPELINE` *(minimal: built-in pipeline selection)*
- `SET_VERTEX_BUFFER`
- `DRAW` *(triangle list)*
- `PRESENT`

### Synchronization

- `FENCE_SIGNAL` *(placeholder in v1.0; real shared fence table can be added later)*

---

## 8. Memory ownership + lifetime rules

1. Resource IDs are owned by the guest; the host treats them as opaque handles.
2. Buffers/textures remain valid until explicitly destroyed.
3. For `WRITE_*` commands, the guest memory referenced by `src_paddr` must remain valid and unchanged until the corresponding completion is observed.
4. For `READ_*` commands, the host writes the destination guest memory before posting the completion entry.

---

## 9. Host-side validation harness

The crate contains a deterministic software backend and a “synthetic guest” producer to validate:

- Ring mechanics (wrap markers, head/tail updates)
- Command parsing and bounds checks
- Completion generation and interrupt signaling

See:

- `aero_gpu_device::guest::SyntheticGuest`
- `crates/aero-gpu-device/tests/golden_triangle.rs`
