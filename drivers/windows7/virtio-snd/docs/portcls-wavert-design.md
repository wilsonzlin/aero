<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd (Windows 7): PortCls + WaveRT design

This document captures Aero’s intended **PortCls/WaveRT** architecture for a
Windows 7 `virtio-snd` audio driver, and how the Windows **KS/WaveRT** stream
state machine maps onto the `virtio-snd` **PCM control** state machine.

It is written as a driver-local design note so future implementers can build and
review the driver without relying on untracked samples or “tribal knowledge”.

## Scope and goals

This design targets the simplest useful audio path for Aero:

- Windows 7 SP1 (x86/x64).
- One **render** endpoint (speaker/headphones style) backed by `virtio-snd` stream id `0` (TX).
- One **capture** endpoint (microphone style) backed by `virtio-snd` stream id `1` (RX).
- Contract-v1 baseline formats are required (see [Assumptions and limits](#assumptions-and-limits)).
- Optional multi-format negotiation (non-contract): when a virtio-snd device advertises additional capabilities via
  `PCM_INFO`, the driver may expose/negotiates additional formats/rates to Windows (while still preferring the
  contract-v1 baseline when present).

Advanced DSP/offload and anything beyond the driver’s negotiated format/rate set are out of scope for this driver.

## Why PortCls + WaveRT (Windows 7+)

Windows exposes audio endpoints through the **Kernel Streaming (KS)** model. In
practice, Windows 7’s shared-mode audio engine expects modern audio drivers to
use **PortCls** (portcls.sys) and to expose a **WaveRT** miniport for low
latency, event-driven streaming.

WaveRT is a better fit than older “packet” or WaveCyclic styles because:

- The audio engine can write directly into a **memory-mapped cyclic buffer**.
- The miniport can provide accurate “hardware-like” position and timestamps.
- Periodic notifications are explicit (event-based) rather than implicit polling.

In a virtual device, “hardware DMA into a buffer” does not exist. WaveRT still
works well as a contract: the driver owns a cyclic buffer that the audio engine
writes into, and the driver is responsible for moving audio from that buffer to
the host device.

## Why we need a QPC-based clock in Aero

On physical audio hardware, the driver can usually derive playback progress from
hardware state:

- a DMA position register, or
- interrupts/completions that occur when the device consumes a period.

In Aero’s current virtualization model, `virtio-snd` **TX completions are
effectively immediate**: once the guest submits a buffer to the `virtio-snd` TX
queue, the device reports it “used” right away. That is a *transport*
acknowledgement (“the host accepted the bytes”), not a playback timing signal
("the bytes were played").

Consequences:

- We **cannot** use virtqueue used entries as the audio clock.
- We **cannot** generate WaveRT period notifications from virtio TX completion.

Therefore the driver must synthesize a stable playback clock using
`KeQueryPerformanceCounter` (QPC):

- QPC is monotonic and high-resolution.
- The driver can convert elapsed time to “frames played” using the stream sample
  rate.
- Position reporting and notifications become deterministic and do not depend on
  host-side completion behavior.

From the Windows audio stack’s perspective, this makes the driver behave like a
hardware device with a reliable timebase, even if the underlying transport is
“write-only”.

## High-level architecture

### Windows side (PortCls / KS)

The intended structure is a classic PortCls “adapter + miniports” design:

- **Adapter / FDO**: owns the virtio transport (virtio-pci + virtqueues) and
  provides shared device services.
- **Topology miniport**: exposes the endpoint topology (speaker + microphone connectors).
- **WaveRT miniport**: exposes one render pin and one capture pin that support WaveRT.
- **WaveRT stream object**: created per open stream (render or capture); owns the cyclic buffer mapping,
  clock state, and notification timer/period scheduling.

The KS pin state changes drive stream start/stop, and PortCls calls into the
WaveRT stream’s `SetState` to request those transitions.

### virtio-snd side

The `virtio-snd` device provides:

- This design assumes the Aero Windows 7 virtio device contract v1 (`AERO-W7-VIRTIO`); if anything in this document
  disagrees with the contract, the contract wins:
  - `docs/windows7-virtio-driver-contract.md` (§3.4 virtio-snd)
- Contract v1 summary (virtio-snd):
  - Transport: virtio-pci modern-only (PCI vendor-specific caps + BAR0 MMIO) with PCI Revision ID `0x01` (`REV_01`)
  - Features: `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` only
  - Interrupts: INTx baseline (read-to-ack ISR). MSI/MSI-X may be used when Windows grants message interrupts (INF opt-in); in that case the driver programs virtio MSI-X routing (`msix_config`, `queue_msix_vector`) and verifies read-back.
    - On Aero contract devices, MSI-X is **exclusive** when enabled: if a virtio MSI-X selector is `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) (or the MSI-X entry is masked/unprogrammed), interrupts for that source are **suppressed** (no MSI-X message and no INTx fallback). Therefore vector-programming failures must be treated as fatal unless the driver can switch to INTx or polling-only mode.
  - Queues: `controlq=64`, `eventq=64`, `txq=256`, `rxq=64`
  - Streams: stream 0 output (stereo) and stream 1 input (mono); contract v1 requires an S16_LE/48 kHz baseline, but devices may advertise additional formats/rates via `PCM_INFO`

- A **control queue** for PCM control commands (`PCM_SET_PARAMS`, `PREPARE`,
  `START`, `STOP`, `RELEASE`, ...).
- One or more **PCM data queues**. For this design, we assume:
  - a render TX queue feeding stream id `0`
  - a capture RX queue feeding stream id `1`

The driver’s render loop:

1. Waits for a period boundary (software timer).
2. Copies the next period of audio from the WaveRT cyclic buffer.
3. Submits it to the virtio-snd TX queue as a PCM transfer for stream id 0.
4. Signals the WaveRT notification event for the audio engine.

Because TX completions are not used for timing, virtqueue “used” processing is
treated as backpressure/resource reclamation only.

The driver’s capture loop uses the same WaveRT cyclic-buffer contract as render, but the dataflow is reversed:
the virtio-snd device writes PCM into the buffer via `rxq`. On each period boundary, the driver submits the next
period-sized buffer region for stream id 1, advances the WaveRT **write cursor** when RX completes, and signals
the notification event for the audio engine.

Capture completions may be polled from the period/timer path to avoid interrupt storms on device models that
complete RX buffers immediately.

## Buffer strategy (cyclic buffer + periods)

### WaveRT cyclic buffer

WaveRT exposes a **cyclic** (ring) buffer to the audio engine:

- Render: the audio engine writes samples into the buffer at its own pace, and the driver reads
  (consumes) samples from the buffer at the device pace.
- Capture: the driver writes captured samples into the buffer at the device pace, and the audio
  engine reads samples out at its own pace.

For virtio-snd, the buffer lives in system memory (non-paged) and acts as the
handoff between the Windows audio engine and the virtio-snd transport.

### Period size and notification interval

WaveRT uses a *period* model:

- The buffer is divided into **periods** of `PeriodBytes`.
- The audio engine expects a notification event once per period so it can keep
  the buffer filled.

Driver constraints:

- `BufferBytes` **must** be a multiple of `PeriodBytes`.
- `BufferBytes` should be at least `2 * PeriodBytes` (double-buffering) to allow
  jitter without underruns.

Virtio-snd transfer strategy:

- The driver submits **one virtio-snd transfer per period**.
- Each transfer copies `PeriodBytes` from the cyclic buffer starting at the
  current play cursor.

This keeps the virtqueue descriptor footprint bounded and aligns device writes
with WaveRT notifications.

### Wrap-around handling

The play cursor advances modulo `BufferBytes`. When a period crosses the end of
the ring buffer:

- Copy `TailBytes = BufferBytes - Cursor` from `[Cursor, BufferBytes)`.
- Copy `HeadBytes = PeriodBytes - TailBytes` from `[0, HeadBytes)`.

The virtio-snd submission is built from the concatenation of those regions (for
example into a temporary contiguous buffer, or as two SG segments if the
transport layer supports it).

In Aero, WaveRT buffers are represented as an MDL whose pages may be physically
non-contiguous. To submit the cyclic buffer to the virtio-snd TX queue without
copying, use the scatter/gather helper in this driver:

- `VirtIoSndTxSgBuildFromMdlRegion()` (`include/virtiosnd_sg_tx.h`)

It converts the logical circular region into a compact SG list by splitting on
page boundaries and coalescing physically contiguous PFNs, and it handles wrap
around the end of the ring.

## KS/WaveRT state machine ↔ virtio-snd PCM control mapping

Windows pins use the KS state machine:

- `KSSTATE_STOP`
- `KSSTATE_ACQUIRE`
- `KSSTATE_PAUSE`
- `KSSTATE_RUN`

For WaveRT render, PortCls typically drives transitions in the order:
`STOP → ACQUIRE → PAUSE → RUN` and back down as the stream closes.

The `virtio-snd` PCM control commands are used to reflect the same lifecycle on
the device.

### Mapping table

The table below describes the intended behavior. “SetState” refers to the WaveRT
stream object’s state-change entry point (e.g. `IMiniportWaveRTStream::SetState`).

| KS transition | virtio-snd control ops | Driver-local actions |
|---|---|---|
| `STOP → ACQUIRE` | `PCM_SET_PARAMS`, then `PCM_PREPARE` | Validate format; allocate/commit WaveRT buffer; reset software clock state; arm notification infrastructure but do **not** start it yet. |
| `ACQUIRE → PAUSE` | *(none)* | Stream is prepared but idle; do not advance the play cursor. |
| `PAUSE → RUN` | `PCM_START` | Record QPC start time; start periodic “period boundary” timer/DPC; begin submitting period transfers to the TX queue; signal notifications each period. |
| `RUN → PAUSE` | `PCM_STOP` | Stop the period timer; freeze position reporting at the last computed cursor; stop submitting audio. |
| `PAUSE → ACQUIRE` | *(none)* | Keep parameters/buffer; remain prepared. |
| `ACQUIRE → STOP` | `PCM_RELEASE` | Cancel timers; drop notification event; release buffers and per-stream resources. |

Notes:

- If Windows requests a direct `RUN → STOP`, perform `PCM_STOP` first (if
  running), then `PCM_RELEASE`.
- The driver may re-issue `PCM_SET_PARAMS` on `STOP → ACQUIRE` when the stream
  format/rate changes (for example when Windows opens a stream with a different
  WAVEFORMAT). For the contract-v1 baseline case, it is always S16_LE/48 kHz.

## Position reporting

WaveRT has two related but distinct notions of “position”:

1. **Position register (cyclic)**: a cursor within `[0, BufferBytes)`, wrapping
   at `BufferBytes`. This is the “hardware DMA pointer” view.
2. **Presentation position (linear + timestamp)**: a monotonically increasing
   frame counter plus a QPC timestamp describing when that position was/will be
   presented.

For Aero, both are derived from the same software clock.

### Position register semantics

For render:

- The position register reports how far the device has progressed through the
  cyclic buffer since `RUN` started.
- It **must not move backwards** while the stream is running.
- It freezes while in `PAUSE/ACQUIRE/STOP`.

Implementation strategy:

- Maintain `StartQpc` and `StartLinearFrames` when entering `RUN`.
- On query, compute:
  - `elapsedFrames = floor((NowQpc - StartQpc) * SampleRate / QpcFrequency)`
  - `linearFrames = StartLinearFrames + elapsedFrames`
  - `positionBytes = (linearFrames * BlockAlign) mod BufferBytes`

The modulo output is what Windows treats as the register position.

### GetPresentationPosition semantics

`GetPresentationPosition` should return:

- `PositionInFrames` (64-bit, monotonic) and
- a QPC timestamp corresponding to that position.

In this design:

- `PositionInFrames` is the same `linearFrames` used above (without modulo).
- The QPC time is the `NowQpc` used for the calculation (or, if implementing a
  “snap to period boundary” model, the QPC time at the last boundary).

The key invariant is that the returned `(PositionInFrames, Qpc)` pair is
internally consistent with the reported sample rate and does not jump
backwards.

## Notification strategy (event-driven, period-based)

WaveRT notifications are driven by an event handle supplied by the audio engine
(via the WaveRT notification property).

Because virtio-snd TX completions are not a timing signal in Aero, the driver
generates notifications itself:

- Create a periodic kernel timer + DPC (or equivalent) per active stream.
- The timer period corresponds to the chosen `PeriodFrames`:
  - `PeriodFrames = PeriodBytes / BlockAlign`
  - `PeriodDurationSeconds = PeriodFrames / SampleRate`
- On each firing:
  1. Advance the internal play cursor by exactly one period (linear + modulo).
  2. Submit one period of audio to the virtio-snd TX queue.
  3. Signal the notification event.

This keeps the audio engine’s “write cursor” cadence aligned with the driver’s
consumption cadence.

## Assumptions and limits

The virtio-snd WaveRT implementation intentionally starts narrow:

- **Contract-v1 baseline formats required**:
  - Stream `0` (render): 48 kHz, **stereo**, 16-bit PCM (S16_LE)
  - Stream `1` (capture): 48 kHz, **mono**, 16-bit PCM (S16_LE)
- **Optional multi-format** (non-contract): if the device advertises extra formats/rates in `PCM_INFO`,
  the driver may expose additional Windows formats via dynamic WaveRT pin data ranges (while keeping the contract-v1
  baseline format preferred/first).
- Channels are still fixed per direction (stream 0 stereo, stream 1 mono).
- **Single render + single capture endpoint**:
  - One render pin and one capture pin.
  - No loopback.
- **Fixed stream mapping**:
  - Map the Windows render stream to `virtio-snd` **stream id 0** (TX).
  - Map the Windows capture stream to `virtio-snd` **stream id 1** (RX).
  - Do not expose multiple endpoints or multiple concurrent hardware streams per direction.

If the device/host later supports a real playback clock or delayed completions,
the QPC-based clock can be replaced or corrected, but the WaveRT contract should
remain the same.
