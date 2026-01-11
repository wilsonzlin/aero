# `virtio-snd` (Windows 7) — design notes

This document is a **clean-room** design note for the Aero Windows 7 virtio-snd
driver. It summarizes the intended architecture and how the pieces fit together.
See `../SOURCES.md` for the authoritative list of references.

The driver in this directory is a clean-room WDM function driver targeting the
Aero virtio device contract. It includes a virtio-pci modern transport core and
split-ring virtqueue plumbing, but it does not yet integrate PortCls/WaveRT
miniports, so it does not expose audio endpoints to Windows yet.

## High-level architecture

The implementation is intended to be layered so that most logic is reusable across
other Windows 7 virtio drivers (blk/net/input).

### 1) virtio-pci modern transport layer

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
- Treat optional ring features (notably `VIRTIO_F_RING_EVENT_IDX`) as negotiated
  capabilities rather than hard requirements.

### 3) virtio-snd protocol engine

Responsibilities:

- Implement the virtio-snd control message flow on `controlq`:
  - query PCM stream capabilities (`PCM_INFO`)
  - configure params (`PCM_SET_PARAMS`)
  - prepare/start/stop/release for playback
- Handle `eventq` for asynchronous device events (if used by the device model).
- Implement the PCM data path:
  - playback: submit PCM buffers on `txq`
  - capture: receive buffers on `rxq` (stream id `1`; defined by the contract, but not implemented by this driver yet)

The protocol engine should be written to:

- validate message sizes and reserved fields,
- return `NOT_SUPP` for unsupported requests,
- keep stream state transitions explicit (Idle → ParamsSet → Prepared → Running).

### 4) PortCls / WaveRT miniport integration

Target model:

- Expose a render (playback) endpoint through PortCls using a WaveRT-style miniport.
- The audio miniport is responsible for:
  - presenting the supported formats/rates/channels to the OS,
  - managing stream creation and teardown,
  - translating WaveRT buffer progress into virtio-snd buffer submissions.

Integration approach (high level):

- During device start, initialize PortCls and register the miniport pair(s).
- When the OS starts a stream, allocate a virtio-snd stream instance and begin
  feeding PCM buffers on `txq`.
- Keep the PortCls-facing part decoupled from the virtio transport so protocol and
  virtqueue logic remains testable outside of PortCls.

### 5) Interrupt + timer pacing model

Baseline requirements:

- Work correctly with **PCI INTx** + the virtio ISR status register (contract v1).
- Optionally support MSI-X when available and enabled by the INF.

Planned behavior:

- ISR does minimal work:
  - acknowledge/deassert INTx by reading the ISR status register,
  - queue a DPC to do the real processing.
- DPC drains used rings for any queues with pending work and completes requests.
- Playback pacing should be stable under load:
  - Use virtqueue interrupt suppression (and optionally EVENT_IDX when negotiated)
    to avoid interrupt storms.
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

## Contract conformance note (contract v1 vs current emulator)

The virtio-snd device model in `crates/aero-virtio/src/devices/snd.rs` is expected to match
the definitive contract in `docs/windows7-virtio-driver-contract.md`, including:

- Contract v1 does **not** negotiate `VIRTIO_F_RING_EVENT_IDX`.
- Queue sizes: `controlq/eventq = 64`, `txq/rxq = 256`.

Driver stance:

- Negotiate only the features the driver actually requires for correctness
  (at minimum `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` for contract v1).
- Validate device-reported `common_cfg.queue_size` / `queue_notify_off` against the
  contract for the queues the driver wires up, and fail bring-up on mismatch.
- Treat `VIRTIO_F_RING_EVENT_IDX` as optional: only negotiate it if the driver fully
  implements the event-index algorithms; otherwise operate in always-notify mode.

## References

- Aero device contract: `docs/windows7-virtio-driver-contract.md`
- Virtqueue guide (in-repo): `docs/virtio/virtqueue-split-ring-win7.md`
- Interrupt guide (in-repo): `docs/windows/virtio-pci-modern-interrupts.md`
