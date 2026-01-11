<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# `virtio-snd` (Windows 7) — design notes

This document is a **clean-room** design note for the Aero Windows 7 virtio-snd
driver. It summarizes the intended architecture and how the pieces fit together.
See `../SOURCES.md` for the authoritative list of references.

The directory contains a clean-room Windows 7 WDM audio adapter driver that
integrates PortCls **WaveRT** + **Topology** miniports so Windows 7 can enumerate
an audio endpoint.

This driver targets the **Aero Windows 7 virtio device contract v1** (`AERO-W7-VIRTIO`).
The authoritative interoperability contract is `docs/windows7-virtio-driver-contract.md`; if this
design note ever disagrees with the contract, the contract wins.

There are currently two **build/packaging variants** supported:

1. **Aero contract v1 (default CI artifact):** strict PCI identity enforcement (`DEV_1059` + `REV_01`)
   as encoded by `inf/aero-virtio-snd.inf`.
2. **QEMU compatibility (optional):** opt-in package for stock QEMU defaults as encoded by
   `inf/aero-virtio-snd-legacy.inf`.

The two INFs intentionally have **no overlapping hardware IDs** so they do not compete for the same PCI function.

Both variants use the same **virtio-pci modern** transport path (PCI vendor-specific capabilities + BAR0 MMIO) with
split-ring virtqueues. The optional QEMU package only relaxes the contract identity checks so the driver can start
on QEMU defaults.

Note: the repository also contains older legacy/transitional virtio-pci **I/O-port** bring-up code (for example
`src/backend_virtio_legacy.c`) for historical bring-up and ad-hoc compatibility testing only. It is not
built/shipped and is not part of the `AERO-W7-VIRTIO` contract. This path only negotiates the low 32 bits of virtio
feature flags and is not suitable for contract v1 devices (`VIRTIO_F_VERSION_1` is bit 32).

## Code organization

- PortCls adapter + miniports: `src/adapter.c`, `src/adapter_context.c`, `src/wavert.c`, `src/topology.c`
- WaveRT backend interface: `include/backend.h`
- Virtio backend implementation: `src/backend_virtio.c`
- Virtio-pci modern bring-up + split virtqueues + protocol engines:
  - `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c`
  - `drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c`
  - `src/virtiosnd_hw.c` (`VirtIoSndStartHardware`/`VirtIoSndStopHardware`)
  - `src/virtiosnd_queue_split.c`
  - `src/virtiosnd_control.c`, `src/virtiosnd_tx.c`, `src/virtiosnd_rx.c`
  - `src/virtiosnd_intx.c`
- Shared virtio support code linked in from:
  - `drivers/windows/virtio/common/virtqueue_split.c`
  - `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
  - `drivers/windows7/virtio/common/src/virtio_pci_contract.c`

### Legacy virtio-pci I/O-port bring-up (not shipped)

The repository also contains an older legacy/transitional virtio-pci I/O-port path (for example
`src/backend_virtio_legacy.c`, `src/aeroviosnd_hw.c`, and `drivers/windows7/virtio/common`). It is
kept for historical bring-up and ad-hoc compatibility testing only.

CI guardrail: PRs must keep `virtio-snd.vcxproj` on the modern-only backend. See
`scripts/ci/check-virtio-snd-vcxproj-sources.py`.

## Default build architecture (virtio-pci modern endpoint driver)

- **Transport:** virtio-pci **modern** (BAR0 MMIO + PCI vendor-specific capabilities; negotiates `VIRTIO_F_VERSION_1`).
- **Queues:** contract v1 defines `controlq`/`eventq`/`txq`/`rxq`. The driver initializes all four; PortCls
  uses `controlq` (0) + `txq` (2) for render (stream 0) and `controlq` (0) + `rxq` (3) for capture (stream 1).
  `eventq` is currently unused by the PortCls endpoints.
- **Interrupts:** INTx only.
- **Protocol:** PCM control + TX/RX protocol engines for streams 0/1.
- **Pacing:** WaveRT period timer/DPC provides the playback clock; virtqueue used
  entries are treated as resource reclamation rather than timing.

The optional `aero-virtio-snd-legacy.inf` package uses the same virtio-pci modern
transport stack but relaxes PCI identity/version gating so it can be used with
stock QEMU defaults (transitional virtio-snd PCI ID, typically `REV_00`).

## High-level architecture

The implementation is intended to be layered so that most logic is reusable across
other Windows 7 virtio drivers (blk/net/input).

### 1) virtio-pci modern transport layer

This section describes the modern virtio-pci architecture used by the driver.

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
      completions via the RX engine.

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

- Exposes fixed-format endpoints:
  - Render (stream 0): 48kHz, stereo, 16-bit PCM LE.
  - Capture (stream 1): 48kHz, mono, 16-bit PCM LE.
- Uses a periodic timer/DPC “software DMA” model to:
  - for render: advance play position, signal the WaveRT notification event, and
    submit one period of PCM into a backend callback.
  - for capture: submit one RX period buffer; RX completion advances the WaveRT
    write cursor and signals the notification event.
- Uses a backend abstraction (`include/backend.h`) so the WaveRT period timer path
  can remain decoupled from virtio transport details.
- The shipped driver package uses the virtio-pci modern backend (`backend_virtio.c`) for render (stream 0), pushing PCM via
  `controlq`/`txq`.
- Capture (stream 1) uses the RX protocol engine (`virtiosnd_rx.c`) and submits MDL-backed buffers to `rxq`.
- A legacy I/O-port backend exists for compatibility testing with transitional devices, but it is not shipped.

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
- `rxq = 64` (initialized; used by the WaveRT capture endpoint)

Driver stance (strict contract-v1):

- Negotiate only the features required for correctness. For contract v1 this
  means enabling `VIRTIO_F_VERSION_1` and `VIRTIO_F_RING_INDIRECT_DESC` and
  leaving `VIRTIO_F_RING_EVENT_IDX` disabled even if a device offers it (for
  example due to a buggy/emerging device model).
- Always size ring allocations from the device-reported `common_cfg.queue_size` after selecting
  each queue. Treat unexpected values (for example smaller than required by the contract, or
  inconsistent `queue_notify_off`) as an incompatibility during bring-up.

Implementation note:

- `AERO-W7-VIRTIO` v1 requires virtio-pci **modern** feature negotiation
  (`VIRTIO_F_VERSION_1` is bit 32). The shipped driver package and INFs assume a
  modern-only device (`DEV_1059` + `REV_01`).
- The adapter validates the contract PCI identity (requires `DEV_1059` + `REV_01`)
  at `START_DEVICE` via `AeroVirtioPciValidateContractV1Pdo`.
- The legacy/transitional I/O-port bring-up path only negotiates the low 32 bits
  of virtio feature flags (so it cannot negotiate `VIRTIO_F_VERSION_1`) and is
  not part of the supported binding contract.

## References

- Aero device contract: `docs/windows7-virtio-driver-contract.md`
- Windows device contract (non-normative for virtio; `AERO-W7-VIRTIO` is definitive): `docs/windows-device-contract.md`
- Virtqueue guide (in-repo): `docs/virtio/virtqueue-split-ring-win7.md`
- WDM virtio-pci modern bring-up notes: `docs/windows/virtio-pci-modern-wdm.md`
- Interrupt guide (in-repo): `docs/windows/virtio-pci-modern-interrupts.md`
