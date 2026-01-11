<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# `virtio-snd` (Windows 7) — design notes

This document is a **clean-room** design note for the Aero Windows 7 virtio-snd
driver. It summarizes the intended architecture and how the pieces fit together.
See `../SOURCES.md` for the authoritative list of references.

The directory contains a clean-room Windows 7 WDM audio adapter driver that
integrates PortCls **WaveRT** + **Topology** miniports so Windows 7 can enumerate
an audio endpoint.

There are currently **two virtio transport paths** in-tree:

1. **Default build (PortCls endpoint driver):** uses the **legacy virtio-pci
   I/O-port** register layout via `drivers/windows7/virtio/common` for maximum
   compatibility with transitional virtio devices (for example stock QEMU). This
   path only negotiates the **low 32 bits** of virtio feature flags and therefore
   does **not** negotiate `VIRTIO_F_VERSION_1`.
2. **Modern virtio-pci bring-up (under development):** capability/MMIO-based
   virtio-pci modern + split-ring virtqueues + protocol engines. This path is
   intended to align with the definitive Aero contract (`AERO-W7-VIRTIO` v1),
   which is modern-only.

## Code organization

### Default build (legacy virtio-pci I/O-port)

- PortCls adapter + miniports: `src/adapter.c`, `src/wavert.c`, `src/topology.c`
- WaveRT backend interface: `include/backend.h`
- Legacy virtio backend implementation: `src/backend_virtio_legacy.c`
- Legacy virtio-pci bring-up + minimal `controlq`/`txq` protocol: `include/aeroviosnd.h`, `src/aeroviosnd_hw.c`
- Shared legacy virtio transport/queue code linked in from:
  - `drivers/windows7/virtio/common/src/virtio_pci_legacy.c`
  - `drivers/windows7/virtio/common/src/virtio_queue.c`

### Modern virtio-pci bring-up (not built by default)

The directory also contains a modern virtio-pci WDM bring-up path (for example
`src/virtiosnd_hw.c` and related `virtiosnd_*` modules). That code is not part of
the default PortCls build today.

## Default build architecture (legacy virtio-pci endpoint driver)

- **Transport:** legacy virtio-pci I/O-port register layout (transitional devices).
- **Queues:** `controlq` (0) + `txq` (2) are used for render playback. `eventq`/`rxq`
  are not wired up by the default driver.
- **Interrupts:** INTx only.
- **Protocol:** minimal PCM control flow for stream 0 (playback) and TX submissions.
- **Pacing:** WaveRT period timer/DPC provides the playback clock; virtqueue used
  entries are treated as resource reclamation rather than timing.

## High-level architecture

The implementation is intended to be layered so that most logic is reusable across
other Windows 7 virtio drivers (blk/net/input).

### 1) virtio-pci modern transport layer (not built by default)

This section describes the modern virtio-pci architecture used by the
non-default WDM bring-up path.

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

### 2) virtqueue split-ring layer (modern path; not built by default)

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

### 3) virtio-snd protocol engine (modern path; not built by default)

Responsibilities:

- Implement the virtio-snd control message flow on `controlq`:
  - query PCM stream capabilities (`PCM_INFO`)
  - configure params (`PCM_SET_PARAMS`)
  - prepare/start/stop/release for playback
- Handle `eventq` for asynchronous device events (if used by the device model).
- Implement the PCM data path:
   - playback: submit PCM buffers on `txq`
   - capture: `rxq` (stream id `1`) is initialized for contract conformance and an
     RX engine exists, but the current PortCls integration is render-only (no
     capture endpoint wired up yet)

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
- The default build uses a legacy virtio-pci backend. A modern backend is future
  work.

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

### 6) PnP / power / reset lifecycle (modern path; not built by default)

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
  - `rxq = 64` (initialized; RX engine exists but capture endpoint integration is TBD)

Driver stance (for eventual strict contract-v1 support):

- The modern virtio-pci path is expected to negotiate only the features required
  for correctness. For contract v1 this means enabling `VIRTIO_F_VERSION_1` and
  `VIRTIO_F_RING_INDIRECT_DESC` and leaving `VIRTIO_F_RING_EVENT_IDX` disabled
  even if a device offers it (for example due to a buggy/emerging device model).
- Always size ring allocations from the device-reported `common_cfg.queue_size` after selecting
  each queue. Treat unexpected values (for example smaller than required by the contract, or
  inconsistent `queue_notify_off`) as an incompatibility during bring-up.

Implementation note:

- The default PortCls endpoint driver currently uses the **legacy** virtio-pci
  I/O-port transport (and therefore cannot negotiate `VIRTIO_F_VERSION_1` at bit
  32). This is a compatibility step and is documented at a high level in
  `docs/windows-device-contract.md`. The modern virtio-pci bring-up path in this
  directory is the intended direction for full `AERO-W7-VIRTIO` v1 conformance.

## References

- Aero device contract: `docs/windows7-virtio-driver-contract.md`
- Windows device contract (non-normative for virtio; `AERO-W7-VIRTIO` is definitive): `docs/windows-device-contract.md`
- Virtqueue guide (in-repo): `docs/virtio/virtqueue-split-ring-win7.md`
- WDM virtio-pci modern bring-up notes: `docs/windows/virtio-pci-modern-wdm.md`
- Interrupt guide (in-repo): `docs/windows/virtio-pci-modern-interrupts.md`
