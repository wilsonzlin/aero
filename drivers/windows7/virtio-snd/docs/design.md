<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# `virtio-snd` (Windows 7) — design notes

This document is a **clean-room** design note for the Aero Windows 7 virtio-snd
driver. It summarizes the intended architecture and how the pieces fit together.
See `../SOURCES.md` for the authoritative list of references.

The directory contains a clean-room Windows 7 WDM audio adapter driver that
integrates PortCls **WaveRT** + **Topology** miniports so Windows 7 can enumerate
an audio endpoint.

There are currently **two virtio transport implementations** in-tree:

1. **Shipped/default driver package:** `AERO-W7-VIRTIO` v1 **virtio-pci modern**
   transport (PCI vendor-specific capabilities + MMIO) with split-ring
   virtqueues.
2. **Legacy/transitional bring-up code (not shipped):** legacy virtio-pci
   **I/O-port** register layout for transitional devices. This exists for
   historical bring-up, but it is **not** part of the `AERO-W7-VIRTIO` contract.
   This path only negotiates the low 32 bits of virtio feature flags and is not
   suitable for contract v1 devices (`VIRTIO_F_VERSION_1` is bit 32). The shipped
   INFs do not bind to transitional IDs.

## Code organization

### Default build (AERO-W7-VIRTIO v1, virtio-pci modern)

- PortCls adapter + miniports: `src/adapter.c`, `src/wavert.c`, `src/topology.c`
- WaveRT backend interface: `include/backend.h`
- Virtio backend implementation: `src/backend_virtio.c`
- Virtio-pci modern bring-up + split virtqueues + protocol engines:
  - `src/virtio_pci_modern_wdm.c`
  - `src/virtiosnd_hw.c`
  - `src/virtiosnd_queue_split.c`
  - `src/virtiosnd_control.c`, `src/virtiosnd_tx.c`, `src/virtiosnd_rx.c`
  - `src/virtiosnd_intx.c`

### Legacy virtio-pci I/O-port bring-up (not shipped)

The repository also contains an older legacy/transitional virtio-pci I/O-port
path (for example `src/backend_virtio_legacy.c`, `src/aeroviosnd_hw.c`, and
`drivers/windows7/virtio/common`). It is kept for historical bring-up only.

## Default build architecture (virtio-pci modern endpoint driver)

- **Transport:** virtio-pci **modern** (MMIO + PCI vendor-specific capabilities).
- **Queues:** contract v1 defines `controlq`/`eventq`/`txq`/`rxq`. The current
  PortCls integration is render-only and primarily uses `controlq` (0) + `txq`
  (2); capture endpoint plumbing is still pending.
- **Interrupts:** INTx only.
- **Protocol:** minimal PCM control flow for stream 0 (playback) and TX submissions.
- **Pacing:** WaveRT period timer/DPC provides the playback clock; virtqueue used
  entries are treated as resource reclamation rather than timing.

## High-level architecture

The implementation is intended to be layered so that most logic is reusable across
other Windows 7 virtio drivers (blk/net/input).

### 1) virtio-pci modern transport layer

This section describes the virtio-pci modern architecture used by the driver
package.

Responsibilities:

- Identify the device by PCI IDs and reject unsupported contract versions (PCI
  Revision ID).
- Parse the PCI capability list to locate virtio vendor-specific capabilities:
  `COMMON_CFG`, `NOTIFY_CFG`, `ISR_CFG`, and `DEVICE_CFG`.
- Map BAR0 and expose typed accessors for the MMIO register blocks.
- Implement the virtio device bring-up sequence:
  reset → ACKNOWLEDGE → DRIVER → negotiate features → FEATURES_OK → program queues
  → DRIVER_OK.
- Serialize access to selector registers in `common_cfg` (feature selectors and
  `queue_select`) so DPC/power paths cannot interleave multi-step sequences.

### 2) virtqueue split-ring layer

Responsibilities:

- Provide a per-queue object that owns:
  - ring memory (descriptor table + avail ring + used ring) in DMA-accessible,
    nonpaged memory,
  - descriptor allocation/free list + per-head “cookie” context,
  - producer/consumer index tracking (`avail_idx`, `last_used_idx`),
  - queue-level spinlock(s) for DISPATCH_LEVEL safety.
- Support the contract-required features:
  - `VIRTIO_F_RING_INDIRECT_DESC` (indirect descriptors).
- Contract v1 does **not** negotiate `VIRTIO_F_RING_EVENT_IDX`; the queue layer must
  function correctly without EVENT_IDX. If a future contract version adds
  event-index support, treat it as optional/negotiated rather than a hard
  requirement.

### 3) virtio-snd protocol engine

Responsibilities:

- Implement the virtio-snd control message flow on `controlq`:
  - query PCM stream capabilities (`PCM_INFO`)
  - configure params (`PCM_SET_PARAMS`)
  - prepare/start/stop/release for playback and capture (streams 0 and 1)
- Handle `eventq` for asynchronous device events (if used by the device model).
- Implement the PCM data path:
   - playback: submit PCM buffers on `txq`
   - capture: submit capture buffers on `rxq` (stream id `1`) and process
     completions via the RX engine. The current PortCls integration is still
     render-only (no capture endpoint wired up yet), but the protocol layer is
     contract-complete.

The protocol engine should be written to:

- validate message sizes and reserved fields,
- return `NOT_SUPP` for unsupported requests,
- keep stream state transitions explicit (Idle → ParamsSet → Prepared → Running).

### 4) PortCls / WaveRT adapter + miniports

Implementation summary:

- Adapter / PortCls registration: `src/adapter.c`
- WaveRT miniport (render): `src/wavert.c`
- Topology miniport: `src/topology.c`

The driver registers `Wave` (PortWaveRT + IMiniportWaveRT) and `Topology`
(PortTopology + IMiniportTopology) subdevices, and physically connects them via
`PcRegisterPhysicalConnection`.

Current behavior:

- Exposes a single render endpoint with a fixed format (48kHz, stereo, 16-bit
  PCM LE).
- Uses a periodic timer/DPC “software DMA” model to:
  - advance play position and signal the WaveRT notification event, and
  - submit one period of PCM into a backend callback.
- Uses a backend abstraction (`include/backend.h`) so the WaveRT period timer path
  can remain decoupled from virtio transport details.
- The shipped driver package uses the virtio-pci modern backend (`backend_virtio.c`).
  Capture is not yet exposed as a Windows endpoint.

### 5) Interrupt + timer pacing model

Baseline requirements:

- Work correctly with **PCI INTx** + the virtio ISR status register (contract v1).
- The current driver package uses **INTx only** (it does not opt into MSI/MSI-X).

Behavior:

- ISR does minimal work:
  - acknowledge/deassert INTx by reading the ISR status register,
  - queue a DPC to do the real processing.
- DPC drains used rings for any queues with pending work and completes requests.
- Playback pacing should be stable under load:
  - Use virtqueue interrupt suppression to avoid interrupt storms (contract v1 does
    not use EVENT_IDX).
  - Use a periodic timer to ensure the transmit pipeline stays filled and to
    re-check for progress if interrupts are lost or suppressed.

### 6) PnP / power / reset lifecycle

PnP:

- `START_DEVICE`: map resources, parse capabilities, reset + negotiate, create and
  enable virtqueues, enable interrupts, register miniports.
- `STOP_DEVICE`: quiesce streams, disable interrupts, reset the device, free queue
  memory, unmap BARs.
- `SURPRISE_REMOVAL`/`REMOVE_DEVICE`: stop everything, release resources, detach.

Power:

- Ensure D-state transitions stop audio playback/capture cleanly.
- Treat any virtio device reset (including `device_status = 0`) as requiring full
  reinitialization of queue state and interrupt routing.

## Contract conformance note (contract v1)

The definitive contract for this driver is `docs/windows7-virtio-driver-contract.md` (§3.4).
The in-tree emulator model (`crates/aero-virtio/src/devices/snd.rs`) includes unit tests that
assert contract v1 feature bits and queue sizes.

Contract v1 requirements include:

- Feature bits: MUST offer `VIRTIO_F_VERSION_1` and `VIRTIO_F_RING_INDIRECT_DESC`, and MUST NOT
  offer `VIRTIO_F_RING_EVENT_IDX` / packed rings.
- Queue indices/max sizes (see `docs/windows7-virtio-driver-contract.md` and
  `include/virtiosnd_queue.h`):
  - `controlq = 64`
  - `eventq = 64` (initialized; currently unused unless the device model emits events)
  - `txq = 256`
  - `rxq = 64` (initialized; RX engine exists, capture endpoint integration is TBD)

Driver stance (for strict contract-v1 support):

- The modern virtio-pci path is expected to negotiate only the features required
  for correctness. For contract v1 this means enabling `VIRTIO_F_VERSION_1` and
  `VIRTIO_F_RING_INDIRECT_DESC` and leaving `VIRTIO_F_RING_EVENT_IDX` disabled
  even if a device offers it (for example due to a buggy/emerging device model).
- Always size ring allocations from the device-reported `common_cfg.queue_size` after selecting
  each queue. Treat unexpected values (for example smaller than required by the contract, or
  inconsistent `queue_notify_off`) as an incompatibility during bring-up.

Implementation note:

- `AERO-W7-VIRTIO` v1 requires virtio-pci **modern** feature negotiation
  (`VIRTIO_F_VERSION_1` is bit 32). The shipped driver package and INFs assume a
  modern-only device (`DEV_1059` + `REV_01`).
- The legacy/transitional I/O-port bring-up path does not implement contract v1
  feature negotiation and is not part of the supported binding contract.

## References

- Aero device contract: `docs/windows7-virtio-driver-contract.md`
- Windows device contract (non-normative for virtio; `AERO-W7-VIRTIO` is definitive): `docs/windows-device-contract.md`
- Virtqueue guide (in-repo): `docs/virtio/virtqueue-split-ring-win7.md`
- WDM virtio-pci modern bring-up notes: `docs/windows/virtio-pci-modern-wdm.md`
- Interrupt guide (in-repo): `docs/windows/virtio-pci-modern-interrupts.md`
