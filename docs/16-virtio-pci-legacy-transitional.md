# 16 - Virtio PCI: Legacy (0.9) + Transitional Devices

## Goal

Windows 7-era virtio drivers (notably older **virtio-win** builds) often expect the **virtio 0.9 “legacy” PCI transport** (I/O port BAR registers) or a **transitional device** that exposes *both* the legacy interface and the virtio 1.0+ PCI capability-based interface.

This document describes an **optional compatibility mode** for targeting upstream virtio-win driver bundles. It is **not** the default Aero Windows 7 virtio transport: [`AERO-W7-VIRTIO` v1](./windows7-virtio-driver-contract.md) is modern-only (PCI capabilities + BAR0 MMIO) and explicitly does not require legacy/transitional I/O port transport.

To maximize compatibility with upstream driver bundles (especially older Windows 7-era virtio-win builds), Aero’s virtio devices may support:

- **Legacy virtio PCI transport** (virtio 0.9 register layout via an I/O port BAR), and/or
- **Transitional virtio PCI devices** that expose **both**:
  - legacy I/O port BAR registers, and
  - modern virtio 1.0+ PCI capabilities (common cfg/notify/isr/device cfg).

This document specifies the register layout and the behavioral rules needed to implement legacy and transitional virtio PCI devices in a way that keeps feature negotiation, queue setup, and interrupts consistent across the modern and legacy paths.

---

## Terminology

- **Legacy**: virtio 0.9 PCI transport using an **I/O port BAR** and **PFN-based** queue setup.
- **Modern**: virtio 1.0+ PCI transport using **vendor-specific PCI capabilities** and MMIO config structures.
- **Transitional**: a single PCI function that exposes **both legacy + modern** transports. The guest chooses which one to use based on what it probes and negotiates.

---

## Web runtime selection (web runtime)

The Aero web runtime defaults to **modern-only** virtio devices (Aero contract v1). To enable compatibility modes for upstream virtio-win bundles, select the transport explicitly per device:

### virtio-net

- **Settings UI:** "Virtio-net mode"
- **Config:** `virtioNetMode: "modern" | "transitional" | "legacy"`
- **URL query override:** `?virtioNetMode=modern|transitional|legacy`

### virtio-input (keyboard/mouse)

- **Settings UI:** "Virtio-input mode"
- **Config:** `virtioInputMode: "modern" | "transitional" | "legacy"`
- **URL query override:** `?virtioInputMode=modern|transitional|legacy`

### virtio-snd (audio)

- **Settings UI:** "Virtio-snd mode"
- **Config:** `virtioSndMode: "modern" | "transitional" | "legacy"`
- **URL query override:** `?virtioSndMode=modern|transitional|legacy`

Notes:

- Changing any of these `virtio*Mode` values changes the guest-visible PCI device ID / BAR layout and requires a VM restart to take effect.
- `"legacy"` disables modern virtio-pci capabilities and exposes only the legacy I/O port register block.

---

## PCI Identification (Transitional vs Modern-only)

Virtio PCI functions are typically identified by:

- **PCI Vendor ID**: `0x1AF4` (virtio / Red Hat)
- **PCI Device ID**:
  - **Transitional IDs** (legacy-compatible): `0x1000..0x103F`
  - **Modern-only IDs**: `0x1040..0x107F`

> Exact device IDs depend on the virtio device type mapping used by the implementation. A common convention is:
> - transitional: `0x1000 + (device_type - 1)`
> - modern: `0x1040 + device_type`

For compatibility with **upstream virtio-win** driver packages (especially older Win7-era builds), presenting **transitional**
IDs is often the most compatible choice (because many older drivers probe/bind via the legacy I/O-port transport and the `0x1000..` transitional PCI ID range).

However, Aero’s own Windows 7 virtio contract v1 uses the **modern** virtio-pci ID space by default
(see `docs/windows7-virtio-driver-contract.md` and `docs/windows-device-contract.md`). Aero may still
expose transitional IDs/legacy I/O BARs as an optional compatibility mode, but drivers must not rely
on it unless explicitly stated by the contract.

---

## Legacy virtio PCI register block (virtio 0.9)

### Placement

The legacy transport is presented via a **PCI I/O BAR**. The BAR maps a register block whose first 20 bytes are transport-defined registers; the remainder is **device-specific config**.

### Register layout

All registers are **little-endian**.

| Offset | Size | Name | Access | Description |
|--------|------|------|--------|-------------|
| `0x00` | 32 | `HOST_FEATURES` | R | Device feature bits (legacy exposes **only bits 0..31**) |
| `0x04` | 32 | `GUEST_FEATURES` | W | Guest-selected features (legacy **only bits 0..31**) |
| `0x08` | 32 | `QUEUE_PFN` | R/W | PFN of the virtqueue for selected queue (`PFN * 4096 = guest phys addr`) |
| `0x0C` | 16 | `QUEUE_NUM` | R | Queue size (max entries) for selected queue. `0` means queue not available. |
| `0x0E` | 16 | `QUEUE_SEL` | W | Select virtqueue index |
| `0x10` | 16 | `QUEUE_NOTIFY` | W | Notify queue index (kicks the device) |
| `0x12` | 8  | `STATUS` | R/W | Device status state machine |
| `0x13` | 8  | `ISR_STATUS` | R (clear) | Interrupt status. Read returns bits and **clears** them. |
| `0x14` | …  | `DEVICE_CONFIG` | R/W | Device-specific config region |

#### Notes on access widths

Guests may perform 8/16/32-bit I/O to these offsets depending on driver and CPU type. The emulation should:

- accept naturally-aligned accesses, and
- be tolerant of sub-word accesses (e.g. 8-bit reads of `STATUS`).

---

## Legacy feature negotiation

Legacy exposes only **32 bits** of features via `HOST_FEATURES`/`GUEST_FEATURES`.

Implications for transitional devices:

- Modern-only feature bits (e.g. `VIRTIO_F_VERSION_1`, typically bit 32) are **not visible** to legacy drivers.
- Therefore, when the guest uses the legacy path, the device must behave as a **legacy virtio device** (0.9 semantics) and must not require `VIRTIO_F_VERSION_1`.

Recommended internal representation:

```rust
/// Device-supported features (full 64-bit set, even if legacy only exposes low 32).
device_features: u64,

/// Guest-accepted features for the *active* transport.
/// - Legacy path: upper 32 bits are forced to 0.
/// - Modern path: full 64-bit negotiation is allowed.
driver_features: u64,
```

---

## Legacy queue programming (PFN)

Legacy queue setup uses `QUEUE_PFN` rather than explicit descriptor/avail/used addresses.

Typical driver flow:

1. Write `QUEUE_SEL = qidx`
2. Read `QUEUE_NUM` (queue size); if `0`, queue doesn’t exist
3. Allocate a virtqueue ring in guest memory aligned to **4096**
4. Write `QUEUE_PFN = ring_guest_phys_addr / 4096`
5. Start operation; notify via `QUEUE_NOTIFY = qidx`

### Deriving ring addresses from PFN

On the device side, `QUEUE_PFN` resolves to a single base address:

```text
ring_base = (QUEUE_PFN as u64) << 12
```

From `ring_base` and `QUEUE_NUM`, compute descriptor/avail/used addresses using the standard **vring** layout with **alignment = 4096** (often referred to as `VIRTIO_PCI_VRING_ALIGN` in OS code).

See also:

- [`docs/virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue layout/alignment rules and the driver-side algorithms for publishing/consuming entries (Win7 KMDF focus).

This allows the core virtqueue implementation to operate on the same internal representation regardless of transport:

```rust
struct QueueState {
    /// Max size exposed by device (`QUEUE_NUM` for legacy, `queue_size_max` for modern).
    max_size: u16,

    /// Size actually selected/enabled (legacy: always `max_size` once PFN != 0).
    size: u16,

    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,

    enabled: bool,
}
```

---

## Legacy device status state machine

The legacy `STATUS` register is shared conceptually with virtio 1.0+ status bits:

| Bit | Name | Meaning |
|-----|------|---------|
| `0x01` | `ACKNOWLEDGE` | Guest found the device |
| `0x02` | `DRIVER` | Guest knows how to drive the device |
| `0x04` | `DRIVER_OK` | Driver is fully set up; device may start processing queues |
| `0x08` | `FEATURES_OK` | (Virtio 1.0+) Feature negotiation complete |
| `0x40` | `DEVICE_NEEDS_RESET` | Device experienced an error and requires reset |
| `0x80` | `FAILED` | Guest gave up; device should stop |

Compatibility notes:

- Older legacy drivers may **not** use `FEATURES_OK`. Treat it as optional on the legacy path; don’t deadlock waiting for it.
- Writing `0` to `STATUS` is a full device reset: clear negotiated features, queue state, and pending interrupts.
- Aside from full reset (`0`), drivers typically only **set** bits. If a guest clears bits without resetting, the simplest compatible behavior is to ignore the clear.

---

## Legacy ISR status register (read clears)

`ISR_STATUS` is a one-byte register with “read-to-clear” semantics:

| Bit | Meaning |
|-----|---------|
| `0x01` | Queue interrupt (used ring update) |
| `0x02` | Device config changed |

Rules:

- Device sets bits when it needs to notify the guest.
- A guest read returns the current bitmask and then clears it.
- For **INTx**, IRQ deassertion should be tied to `ISR_STATUS` becoming 0 (and no other pending condition).

---

## Modern virtio PCI capabilities (virtio 1.0+ overview)

Modern virtio PCI uses vendor-specific PCI capabilities that point to MMIO structures:

- **Common configuration** (features, queue config, status)
- **ISR configuration** (same “read clears” semantics as legacy ISR)
- **Notify configuration** (per-queue notify address)
- **Device-specific configuration** (same content as legacy `DEVICE_CONFIG`)

Modern feature negotiation uses a **64-bit** feature set accessed via a select/data pair:

```text
device_feature_select (u32)  // 0 or 1
device_feature (u32)         // returns (device_features >> (select*32)) & 0xffff_ffff

driver_feature_select (u32)  // 0 or 1
driver_feature (u32)         // guest writes selected 32-bit word
```

For transitional devices:

- A modern driver must negotiate `VIRTIO_F_VERSION_1` (bit 32) and set `FEATURES_OK`.
- Legacy drivers cannot see bit 32 and will never set it; they should remain on legacy semantics.

---

## Transitional device behavior (legacy + modern at once)

### High-level rule

A transitional virtio PCI function exposes both transports simultaneously. The guest can bind via:

- legacy I/O port registers, or
- modern capabilities.

The device must work correctly in either case.

### Recommended “transport mode” gating

To avoid contradictory configuration (e.g., guest sets modern queue addresses while also programming legacy PFNs), treat the device as operating in one of these modes per reset cycle:

```rust
enum TransportMode {
    Unknown,
    Legacy,
    Modern,
}
```

Recommended selection rule:

- After a reset (`STATUS=0`), mode = `Unknown`.
- The first *write* that meaningfully configures the transport locks the mode:
  - writes to legacy `GUEST_FEATURES`, `QUEUE_PFN`, or legacy `STATUS` progression → `Legacy`
  - writes to modern common-cfg feature fields or modern queue address fields → `Modern`
- Once locked, ignore or reject configuration writes coming from the other transport until next reset.

This matches how real guests behave (they pick one driver stack).

### Forcing legacy for testing

Provide a “disable modern” toggle (analogous to QEMU’s `disable-modern=on`) that:

- omits modern virtio PCI capabilities, and/or
- uses transitional/legacy device IDs only,

so that modern OSes are forced to bind via the legacy path. This is extremely useful for validating Windows 7 compatibility.

---

## Interrupts

### Minimum: legacy INTx

To support legacy drivers, always support **PCI INTx** interrupts:

- Set `ISR_STATUS` bits on used-ring updates and config changes.
- Assert the PCI interrupt line while `ISR_STATUS != 0`.
- Deassert after the guest reads and clears `ISR_STATUS` (and no other conditions remain).

### Recommended: MSI-X for modern virtio-net performance

MSI-X is optional for correctness but strongly recommended for performance, especially for virtio-net with multiple queues.

If MSI-X is implemented:

- allow a per-queue MSI-X vector (modern common config `queue_msix_vector`)
- allow a config-change MSI-X vector (modern common config `msix_config`)
- legacy drivers will ignore MSI-X and continue using INTx/ISR.

---

## Suggested implementation layout (VIO-CORE)

Suggested module layout:

```
src/io/virtio/
  pci_legacy.rs         // I/O port BAR, PFN queues, legacy ISR/status
  pci_modern.rs         // virtio 1.0+ PCI capabilities + common/notify/isr cfg
  pci_transitional.rs   // wraps both; mode gating; shared device core
  core.rs               // common virtio device/queue logic
```

Recommended abstraction boundary:

- `core.rs` owns:
  - negotiated features
  - queue state (desc/avail/used addresses, size, enable)
  - device-specific config blob
  - notification and interrupt generation hooks
- each PCI transport (`pci_legacy`, `pci_modern`) is a “front-end” that:
  - decodes guest register accesses, and
  - calls into the shared core to apply configuration / kick queues.

---

## Verification plan

### Unit tests: legacy guest driver flow (transport-level)

Emulate a “pure legacy” driver interaction against the I/O-port BAR:

1. Read `HOST_FEATURES`
2. Write `GUEST_FEATURES`
3. Set `STATUS = ACKNOWLEDGE | DRIVER`
4. Configure queue 0:
   - `QUEUE_SEL = 0`
   - read `QUEUE_NUM`
   - write `QUEUE_PFN`
5. Set `STATUS = ACKNOWLEDGE | DRIVER | DRIVER_OK`
6. Place one descriptor chain in the ring and `QUEUE_NOTIFY = 0`
7. Device consumes descriptors, writes one used element, raises INTx, sets `ISR_STATUS |= 0x1`
8. Guest reads `ISR_STATUS` and verifies it clears

Pseudo-test (shape only):

```rust
#[test]
fn virtio_pci_legacy_queue_pfn_and_isr() {
    let mut dev = TestVirtioPciTransitional::new()
        .with_modern_disabled(true);

    let io = dev.legacy_io_base();

    // 1-2: feature negotiation
    let host_features = dev.io_read32(io + 0x00);
    dev.io_write32(io + 0x04, host_features & 0xffff_ffff);

    // 3: status progression
    dev.io_write8(io + 0x12, 0x01 | 0x02); // ACKNOWLEDGE | DRIVER

    // 4: queue setup via PFN
    dev.io_write16(io + 0x0E, 0); // QUEUE_SEL
    let qsz = dev.io_read16(io + 0x0C);
    assert!(qsz > 0);

    let ring_addr = dev.alloc_vring_legacy(qsz, /*align=*/4096);
    dev.io_write32(io + 0x08, (ring_addr >> 12) as u32); // QUEUE_PFN

    // 5: driver ok
    dev.io_write8(io + 0x12, 0x01 | 0x02 | 0x04);

    // 6: submit one request and notify
    dev.place_one_descriptor_chain_legacy(ring_addr, qsz);
    dev.io_write16(io + 0x10, 0); // QUEUE_NOTIFY

    // 7: device raises interrupt
    assert!(dev.intx_asserted());

    // 8: ISR read clears
    let isr = dev.io_read8(io + 0x13);
    assert_eq!(isr & 0x01, 0x01);
    assert!(!dev.intx_asserted());
}
```

### Smoke tests (in-guest)

If/when a bootable guest harness exists:

- **Linux legacy bind test**:
  - present transitional virtio-blk and virtio-net
  - boot a kernel/config that will bind via legacy (or force legacy by disabling modern caps)
  - verify one block read/write and one net TX
- **Windows 7 virtio-win bind test**:
  - attach virtio-win ISO
  - verify that virtio-net/virtio-blk devices are detected and the driver binds successfully
