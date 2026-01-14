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

1. **Aero contract v1 (default CI artifact):** strict PCI identity enforcement (`PCI\VEN_1AF4&DEV_1059&REV_01`)
   as encoded by `inf/aero_virtio_snd.inf`.
2. **QEMU compatibility (optional):** opt-in package for stock QEMU defaults as encoded by
    `inf/aero-virtio-snd-legacy.inf`.

The two INFs intentionally have **no overlapping hardware IDs** so they do not compete for the same PCI function.

Both variants use the same **virtio-pci modern** transport path (PCI vendor-specific capabilities + BAR0 MMIO) with
split-ring virtqueues. The optional QEMU package exists so QEMU bring-up can be done without weakening the default
contract-v1 INF; it binds to the transitional virtio-snd PCI HWID (`PCI\VEN_1AF4&DEV_1018`).

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
  - `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
  - `drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c`
  - `drivers/windows7/virtio/common/src/virtio_pci_contract.c`
  - `drivers/windows/virtio/common/virtqueue_split.c`
  - `src/virtiosnd_hw.c` (`VirtIoSndStartHardware`/`VirtIoSndStopHardware`)
  - `src/virtiosnd_queue_split.c`
  - `src/virtiosnd_control.c`, `src/virtiosnd_tx.c`, `src/virtiosnd_rx.c`
  - `src/virtiosnd_intx.c`

### Legacy virtio-pci I/O-port bring-up (not shipped)

The repository also contains an older legacy/transitional virtio-pci I/O-port path (for example
`src/backend_virtio_legacy.c`, `src/aero_virtio_snd_ioport_hw.c`, and `drivers/windows7/virtio/common`). It is
kept for historical bring-up and ad-hoc compatibility testing only.

CI guardrail: PRs must keep `aero_virtio_snd.vcxproj` on the modern-only backend. See `scripts/ci/check-virtio-snd-vcxproj-sources.py`.

## Default build architecture (virtio-pci modern endpoint driver)

- **Transport:** virtio-pci **modern** (BAR0 MMIO + PCI vendor-specific capabilities; negotiates `VIRTIO_F_VERSION_1`).
- **Queues:** contract v1 defines `controlq`/`eventq`/`txq`/`rxq`. The driver initializes all four; PortCls
  uses `controlq` (0) + `txq` (2) for render (stream 0) and `controlq` (0) + `rxq` (3) for capture (stream 1).
  `eventq` is currently unused by the PortCls endpoints.
- **Interrupts:** MSI/MSI-X (message interrupts) preferred when granted by Windows (the shipped INF opts in). When MSI/MSI-X is active, the driver programs virtio MSI-X routing (`common_cfg.msix_config`, `common_cfg.queue_msix_vector`) and verifies read-back. If message interrupts are unavailable/cannot be connected, the driver uses INTx (contract v1 baseline).
  - On Aero contract devices, MSI-X is **exclusive** when enabled: if a virtio MSI-X selector is `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) (or the MSI-X entry is masked/unprogrammed), interrupts for that source are **suppressed** (no MSI-X message and no INTx fallback). Therefore vector-programming failures must be treated as fatal unless the driver can switch to INTx or polling-only mode.
- **Protocol:** PCM control + TX/RX protocol engines for streams 0/1.
- **Pacing:** WaveRT period timer/DPC provides the playback clock; virtqueue used
  entries are treated as resource reclamation rather than timing.

The optional `aero-virtio-snd-legacy.inf` package uses the same virtio-pci modern
transport stack but is packaged to bind to QEMU's transitional virtio-snd PCI ID
(`PCI\VEN_1AF4&DEV_1018`).

## Optional/Compatibility Features

This section describes optional behavior that is **not required by AERO-W7-VIRTIO contract v1**, but is relevant when
running against non-contract virtio-snd implementations (for example, stock QEMU).

### `eventq` robustness

Contract v1 reserves `eventq` for future use (§3.4.2.1) and forbids drivers from depending on event messages.

The driver still initializes `eventq` and posts a small buffer pool so it can safely tolerate:

- devices that unexpectedly complete event buffers, and
- future device models that begin emitting async events (jack connect/disconnect, period elapsed, XRUN, etc.).

Audio streaming MUST remain correct even when `eventq` is silent (most contract-v1 devices) or noisy.

### Multi-format/device capability variance (non-contract)

Contract v1 fixes the PCM formats/rates and stream topology (stream 0 render + stream 1 capture, both 48kHz S16_LE).

For compatibility, the bring-up path is written to be defensive when a device advertises a superset:

- It validates that the required contract format/rate/channel combinations are present in `PCM_INFO`.
- When `PCM_INFO` capabilities are available, it can dynamically generate WaveRT format tables and expose
  additional formats/rates/channels to the Windows audio stack.
- It preserves the contract-v1 baseline format as the **first** enumerated format (so Windows keeps the
  expected default mix format).
- When `PCM_INFO` is unavailable (null backend / some legacy builds), it falls back to fixed contract-v1 formats.

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
     completions via the RX engine. The PortCls WaveRT miniport wires this up as
     a Windows capture endpoint.

The protocol engine should be written to:

- validate message sizes and reserved fields,
- return `NOT_SUPP` for unsupported requests,
- keep stream state transitions explicit (Idle → ParamsSet → Prepared → Running).

### 4) PortCls / WaveRT adapter + miniports

Implementation summary:

- Adapter / PortCls registration: `src/adapter.c`
- WaveRT miniport (render + capture): `src/wavert.c`
- Topology miniport: `src/topology.c`

The driver registers `Wave` (PortWaveRT + IMiniportWaveRT) and `Topology`
(PortTopology + IMiniportTopology) subdevices, and physically connects them via
`PcRegisterPhysicalConnection`.

Current behavior:

- Exposes endpoints with a contract-v1 baseline format by default:
  - Render (stream 0): 48kHz, stereo, 16-bit PCM LE.
  - Capture (stream 1): 48kHz, mono, 16-bit PCM LE.
- When virtio-snd `PCM_INFO` capabilities are available, it can additionally expose extra formats/rates
  via dynamically generated WaveRT data ranges (while still requiring and preferring the contract-v1 baseline when present).
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
- Prefer **MSI/MSI-X** when Windows assigns message interrupts (INF `Interrupt Management\\MessageSignaledInterruptProperties` opt-in) and virtio MSI-X vector programming succeeds (`common_cfg.msix_config`, `common_cfg.queue_msix_vector`), and use INTx when message interrupts are unavailable/cannot be connected.
  - If MSI-X is enabled but vector programming fails (read-back `VIRTIO_PCI_MSI_NO_VECTOR`), interrupts are suppressed on Aero contract devices; the driver must not rely on implicit INTx fallback.
- If no usable interrupt resource can be connected (neither MSI/MSI-X nor INTx), fail `START_DEVICE` by default.
  - Optional bring-up: `AllowPollingOnly=1` allows starting in polling-only mode and relying on the WaveRT period timer DPC to poll/drain used rings (intended for early device-model bring-up and debugging).

Behavior:

- ISR does minimal work and queues a DPC to do the real processing:
  - **INTx:** acknowledge/deassert by reading the virtio ISR status register.
  - **MSI/MSI-X:** treat interrupts as message-based/non-shared; do not read the ISR status byte in the ISR.
- DPC drains used rings and completes requests:
  - **INTx:** drain all queues (INTx does not identify which queue fired).
  - **MSI/MSI-X:** dispatch based on the message ID when enough vectors are granted (vector 0 = config, vector 1..4 = queues 0..3); otherwise drain all sources from vector 0.
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
  contract-v1 device (`PCI\VEN_1AF4&DEV_1059&REV_01`).
- The adapter validates the contract PCI identity (requires `PCI\VEN_1AF4&DEV_1059&REV_01`)
  at `START_DEVICE` by reading PCI config space (`src/adapter.c`). The transport
  layer also enforces the contract PCI identity and BAR0 layout during
  `VirtioPciModernTransportInit`.
- The legacy/transitional I/O-port bring-up path only negotiates the low 32 bits
  of virtio feature flags (so it cannot negotiate `VIRTIO_F_VERSION_1`) and is
  not part of the supported binding contract.

## References

- Aero device contract: `docs/windows7-virtio-driver-contract.md`
- Windows device contract (non-normative for virtio; `AERO-W7-VIRTIO` is definitive): `docs/windows-device-contract.md`
- Virtqueue guide (in-repo): `docs/virtio/virtqueue-split-ring-win7.md`
- WDM virtio-pci modern bring-up notes: `docs/windows/virtio-pci-modern-wdm.md`
- Interrupt guide (in-repo): `docs/windows/virtio-pci-modern-interrupts.md`
