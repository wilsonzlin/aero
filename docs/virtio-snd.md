# Virtio-snd (Paravirtual Audio) Device

This repository includes a minimal **virtio-snd** device model (`crates/aero-virtio`, `aero_virtio::devices::snd`) intended to be used as a high-performance alternative to full Intel HDA emulation once guest drivers exist.

Note: the **browser worker runtime** (IO worker) wires up **HDA** as the default *active* guest audio device, but **virtio-snd is
also wired into the IO-worker PCI stack**:

- WASM bridge export: `crates/aero-wasm/src/virtio_snd_pci_bridge.rs` (`VirtioSndPciBridge`)
- TS PCI device wrapper: `web/src/io/devices/virtio_snd.ts` (`VirtioSndPciDevice`)
- IO worker init/wiring: `web/src/workers/io_virtio_snd_init.ts` + `web/src/workers/io.worker.ts`

See also:

- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide for Windows 7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`windows-device-contract.md`](./windows-device-contract.md) — PCI ID + driver service naming contract (Aero).
- [`drivers/protocol/virtio/`](../drivers/protocol/virtio/) — canonical `#[repr(C)]` message layouts (with Rust unit tests) shared between the guest driver and device model.

## Browser runtime status

Virtio-snd is instantiated in the browser by the **IO worker**:

1. `web/src/workers/io.worker.ts` calls `tryInitVirtioSndDevice` (`web/src/workers/io_virtio_snd_init.ts`) when the WASM export
   `VirtioSndPciBridge` is available.
2. `tryInitVirtioSndDevice` registers `VirtioSndPciDevice` (`web/src/io/devices/virtio_snd.ts`) on the IO worker PCI bus.

### Making virtio-snd the active (ring-attached) audio device

The host AudioWorklet rings are **SPSC**, so the IO worker attaches them to only one guest audio device at a time:

- Prefer **HDA** when `HdaControllerBridge` is available.
- Fall back to **virtio-snd** only when HDA is unavailable (e.g. a WASM build that omits HDA exports).

If both devices are registered, virtio-snd may still enumerate to the guest, but it will not produce/consume host audio unless an
explicit device-selection mechanism is added (detach HDA rings and attach virtio-snd rings).

Limitations (current):

- VM snapshot/restore is supported in the browser runtime under kind `"audio.virtio_snd"` (`DeviceId::VIRTIO_SND = 22`).
- Snapshots preserve guest-visible virtio-pci + stream state and AudioWorklet ring indices, but do not serialize host audio
  contents (rings are cleared to silence on restore).

Scope:

- **2 PCM streams**
  - Stream `0`: playback/output, **stereo (2ch)**, 48kHz, signed 16-bit little-endian (S16_LE)
  - Stream `1`: capture/input, **mono (1ch)**, 48kHz, signed 16-bit little-endian (S16_LE)
- **Split virtqueues** (virtio 1.2 subset)
- **Control queue** for stream discovery/configuration
- **TX queue** for PCM frame submission (playback)
- **RX queue** for PCM capture (recording)

## PCI identification (Aero)

Aero exposes virtio-snd as a virtio-pci device using the standard virtio vendor ID:

- Vendor ID: `0x1AF4`
- Device ID (canonical / default): `0x1059` (modern ID space: `0x1040 + VIRTIO_ID_SND (25)`)
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0019` (`VIRTIO_ID_SND`)
- Revision ID: `0x01` (Aero Windows 7 virtio contract v1; see `docs/windows7-virtio-driver-contract.md`)

Windows driver binding (Aero):

- The shipped Aero Win7 virtio-snd INF (`drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf`) is intentionally strict and matches only:
  - `PCI\VEN_1AF4&DEV_1059&REV_01`
  - (Optional) `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01` (commented out in the INF by default)

### Contract v1 summary (AERO-W7-VIRTIO: virtio-snd)

Treat [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) as authoritative.
This section is a convenience summary that MUST remain consistent with that contract.
The clean-room Win7 INF (`aero_virtio_snd.inf`) matches only `DEV_1059` (typically revision-gated as `&REV_01`), so if
Windows shows `DEV_1018` you must configure the hypervisor to expose a modern-only device (for example QEMU
`disable-legacy=on`).

- **Transport:** virtio-pci **modern-only** (PCI vendor-specific capabilities + **BAR0 MMIO**). No legacy I/O-port BARs.
- **Contract major version:** encoded in PCI Revision ID (`REV_01`).
- **Feature bits:** `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` only.
- **Virtqueue sizes:** `controlq=64`, `eventq=64`, `txq=256`, `rxq=64`.
- **Streams (fixed-format):**
  - Stream `0`: render/playback, stereo (2ch), 48kHz, S16_LE
  - Stream `1`: capture/input, mono (1ch), 48kHz, S16_LE
- *(Optional/non-contract, Win7 driver behavior)*: if a virtio-snd implementation advertises additional formats/rates/channel
  counts in `PCM_INFO`, the in-tree Windows 7 driver can optionally expose additional formats to the Windows audio stack
  via dynamic WaveRT pin data ranges (while still requiring and preferring the contract-v1 baseline).

### Event queue (eventq)

The virtio-snd specification defines `eventq` (queue index `1`) for asynchronous device → driver notifications.

Contract v1 does **not** define any required event messages (see `docs/windows7-virtio-driver-contract.md` §3.4.2.1).

By default, Aero’s virtio-snd device model does not emit any `eventq` messages unless the host explicitly queues them (for example
via `VirtioSnd::queue_event(...)` / `VirtioSnd::queue_jack_event(...)`). The browser/WASM runtime uses this to emit jack
connect/disconnect events when host audio backends are attached/detached:

- Speaker jack (`jack_id = 0`): AudioWorklet output ring attach/detach (`VirtioSndPciBridge::set_audio_ring_buffer`)
- Microphone jack (`jack_id = 1`): mic capture ring attach/detach (`VirtioSndPciBridge::set_mic_ring_buffer`)

Implementation notes (device model):

- The device model keeps a **bounded** FIFO of pending event messages (currently capped at 256) to avoid unbounded host memory
  growth if a guest never posts/consumes event buffers.
- `queue_jack_event(...)` deduplicates redundant JACK state transitions within the pending FIFO (it will not enqueue repeated
  identical connected/disconnected events for the same jack ID).

Even when no events are emitted:

- The Windows 7 virtio-snd driver posts a small bounded set of writable buffers and keeps `eventq` running.
- If the device completes an event buffer anyway (future extensions, or a buggy device model), the driver parses the standard
  8-byte virtio-snd event header (`type: u32` + `data: u32`, little-endian), dispatches known events best-effort, and ignores
  unknown/malformed events without crashing (buffers are always reposted).

The authoritative Windows driver-binding values are tracked in [`docs/windows-device-contract.md`](./windows-device-contract.md)
and [`docs/windows-device-contract.json`](./windows-device-contract.json).

If the device does not report the contract-v1 HWID `PCI\VEN_1AF4&DEV_1059&REV_01`, the shipped INF will not bind. For QEMU, pass:

```text
-device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
```

For QEMU bring-up/regression (where the virtio-snd device may enumerate as transitional by default), the repo also
contains an opt-in compatibility driver package:

- `drivers/windows7/virtio-snd/inf/aero-virtio-snd-legacy.inf` (binds the transitional virtio-snd PCI ID `PCI\VEN_1AF4&DEV_1018`; installs service `aeroviosnd_legacy`)
- `virtiosnd_legacy.sys` (build with MSBuild `Configuration=Legacy`)

For older bring-up scenarios where a legacy **I/O-port** virtio-pci transport is required (not part of the Aero virtio
contract and not packaged by default), the repo also contains an opt-in I/O-port package:

- `drivers/windows7/virtio-snd/inf/aero-virtio-snd-ioport.inf` (binds `PCI\VEN_1AF4&DEV_1018&REV_00`; installs service `aeroviosnd_ioport`)
- `virtiosnd_ioport.sys` (build with MSBuild `virtio-snd-ioport-legacy.vcxproj`)

## Device Configuration

The device reports two PCM streams:

- `streams = 2`
- `jacks = 0` (**preferred**) or `jacks = 2` (**tolerated**, matches the driver’s fixed two-jack topology and enables optional JACK eventq notifications)
- `chmaps = 0`

## Supported Control Commands

The control virtqueue accepts requests prefixed by a 32-bit little-endian `code`. The response always starts with a 32-bit little-endian **status**.

The canonical packed layouts for all virtio-snd structs referenced below are defined in `drivers/protocol/virtio` (e.g. `VirtioSndPcmInfo`, `VirtioSndPcmSetParamsReq`, `VirtioSndPcmXferHdr`).

### Status codes

- `VIRTIO_SND_S_OK = 0`
- `VIRTIO_SND_S_BAD_MSG = 1` (malformed request / invalid stream id)
- `VIRTIO_SND_S_NOT_SUPP = 2` (well-formed but unsupported operation/format)
- `VIRTIO_SND_S_IO_ERR = 3` (invalid state for the requested operation)

### Implemented request codes

- `VIRTIO_SND_R_PCM_INFO (0x0100)`
  - Returns `virtio_snd_pcm_info` entries for streams `0` and/or `1` depending on the requested range.
- `VIRTIO_SND_R_PCM_SET_PARAMS (0x0101)`
  - Validates and stores parameters for stream id `0` or `1`.
  - Only accepts:
    - Stream `0` (playback): `{ channels = 2, format = S16_LE, rate = 48000 }`
    - Stream `1` (capture): `{ channels = 1, format = S16_LE, rate = 48000 }`
- `VIRTIO_SND_R_PCM_PREPARE (0x0102)`
  - Requires parameters to have been set.
- `VIRTIO_SND_R_PCM_START (0x0104)`
  - Requires the stream to have been prepared.
- `VIRTIO_SND_R_PCM_STOP (0x0105)`
  - Requires the stream to be running.
- `VIRTIO_SND_R_PCM_RELEASE (0x0103)`
  - Resets stream state back to `Idle`.

All jack/chmap requests return `VIRTIO_SND_S_NOT_SUPP`.

## TX Queue (Playback)

The TX virtqueue is used to submit PCM frames to the host.

The device expects a descriptor chain with:

1. One or more **out** descriptors containing:
   - An 8-byte header: `stream_id: u32` + `reserved: u32`
   - Followed by raw PCM bytes (interleaved S16_LE stereo)
2. At least one **in** descriptor containing an 8-byte response:
   - `status: u32`
   - `latency_bytes: u32` (currently `0`)

PCM writes are accepted only when the stream has been started; otherwise `VIRTIO_SND_S_IO_ERR` is returned.

## RX Queue (Capture)

The RX virtqueue is used by the guest to fetch captured PCM frames from the host.

The device expects a descriptor chain with:

1. One or more **out** descriptors containing:
   - An 8-byte header: `stream_id: u32` + `reserved: u32`
2. One or more **in** descriptors for PCM payload bytes (device writes captured PCM here)
   - Raw PCM bytes (S16_LE mono, 48kHz)
3. A final **in** descriptor containing an 8-byte response:
   - `status: u32`
   - `latency_bytes: u32` (currently `0`)

Captured reads are serviced only when the capture stream has been started; otherwise `VIRTIO_SND_S_IO_ERR` is returned.

If the host capture backend cannot provide enough samples to fill the payload buffers, the device writes silence for the missing samples and increments host-side underrun telemetry counters.

### Capture sample source

In browser builds, captured samples are expected to come from the Web mic capture
ring buffer (`SharedArrayBuffer`) via `aero_platform::audio::mic_bridge::MicBridge` (re-exported as
`aero_audio::mic_bridge::MicBridge`).

### Capture sample-rate conversion

The browser microphone capture graph runs at the owning `AudioContext.sampleRate` (which browsers may ignore; Safari/iOS often uses 44.1kHz). The virtio-snd guest-facing ABI is fixed at 48kHz S16_LE, so the RX/capture path resamples from the host capture rate to **48kHz** before encoding PCM payload bytes.

In `aero_virtio::devices::snd::VirtioSnd`, the host capture rate is tracked by `capture_sample_rate_hz` (defaults to `host_sample_rate_hz`).

### AudioWorklet ring buffer layout

The AudioWorklet ring buffer used by the AU-WORKLET path uses **frame indices** (not sample indices).
The canonical layout is defined in:

- Rust: `crates/platform/src/audio/worklet_bridge.rs` (`aero_platform::audio::worklet_bridge`, re-exported by `aero_audio::worklet_bridge`)
- JS:
  - `web/src/audio/audio_worklet_ring.ts` + `web/src/platform/audio_worklet_ring_layout.js` (layout constants + helper math)
  - `web/src/platform/audio.ts` (main-thread ring producer helpers + AudioWorkletNode wiring)
  - `web/src/platform/audio-worklet-processor.js` (AudioWorklet consumer)

Layout (little-endian):

- u32 `readFrameIndex` (bytes 0..4)
- u32 `writeFrameIndex` (bytes 4..8)
- u32 `underrunCount` (bytes 8..12): total missing output frames rendered as silence due to underruns (wraps at 2^32)
- u32 `overrunCount` (bytes 12..16): frames dropped by the producer due to buffer full (wraps at 2^32)
- f32 `samples[]` (bytes 16..), interleaved by channel: `L0, R0, L1, R1, ...`

## Host sample rate and resampling

In the canonical Rust device model (`aero_virtio::devices::snd::VirtioSnd`), the guest contract is fixed at **48kHz** PCM, but the host Web Audio graph may run at a different sample rate. The device therefore performs sample-rate conversion in both directions:

### TX / playback

TX PCM samples are:

1. decoded from interleaved S16_LE to interleaved `f32`
2. resampled from the guest contract rate (**48kHz**) to the host/output sample rate (typically `AudioContext.sampleRate`)
3. pushed into an `aero_audio::sink::AudioSink` (usually an AudioWorklet ring buffer producer).

### RX / capture

RX PCM samples are:

1. read as mono `f32` from the capture backend (typically `MicBridge`) at the host/input sample rate
2. resampled from the host/input sample rate (`capture_sample_rate_hz`, defaulting to `host_sample_rate_hz`) to the guest contract rate (**48kHz**)
3. encoded as S16_LE and written into the guest RX payload buffers.

`host_sample_rate_hz` defaults to 48kHz, but can be configured via:

- `VirtioSnd::new_with_host_sample_rate(...)`
- `VirtioSnd::new_with_capture_and_host_sample_rate(...)`
- `VirtioSnd::set_host_sample_rate_hz(...)`

If the capture input rate differs from the playback/output rate, override it via:

- `VirtioSnd::set_capture_sample_rate_hz(...)`

## Windows 7 Driver Strategy

Windows 7 does not ship a virtio-snd driver. Expected options:

### Option A: Custom test-signed WDM audio miniport

- Implement a WDM audio miniport (PortCls / WaveRT) that:
  - Enumerates as a standard Windows audio endpoint.
  - Uses the Aero **virtio-pci modern** transport (PCI vendor capabilities + BAR0 MMIO) and split virtqueues to:
    - Query stream capabilities (`PCM_INFO`)
    - Negotiate params (`PCM_SET_PARAMS`)
    - Start/stop streams and submit PCM buffers via the TX queue (playback) and RX queue (capture).
  - Works with PCI **INTx** (contract v1 baseline requires INTx; ISR status is read-to-ack to deassert the line).
  - Supports **MSI/MSI-X** (message-signaled interrupts). The in-tree Windows 7 driver package opts in via INF
    (`MSISupported=1`), prefers message interrupts when Windows grants them, programs virtio MSI-X routing
    (`msix_config`, `queue_msix_vector`), and verifies read-back.
    - On Aero contract devices, MSI-X is **exclusive** when enabled: if a virtio MSI-X selector is
      `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) (or the MSI-X entry is masked/unprogrammed), interrupts for that source are
      **suppressed** (no MSI-X message and no INTx fallback). Drivers must not rely on “INTx fallback” unless MSI-X is
      actually disabled and INTx resources are in use.
  - Optional bring-up toggles (per-device registry, intended for early device-model bring-up):
    - `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly` (`REG_DWORD`)
      - `1`: allow polling-only mode if no usable interrupt resource can be connected (neither MSI/MSI-X nor INTx)
    - `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend` (`REG_DWORD`)
      - `1`: force the silent null backend and allow `START_DEVICE` to succeed even when virtio transport bring-up fails
    - Find `<DeviceInstancePath>` via **Device Manager → device → Details → “Device instance path”**.
    - The shipped INFs seed these values with `FLG_ADDREG_NOCLOBBER` so explicit user overrides persist across reinstall/upgrade.
    - Backwards compatibility: older installs may store these values under the device/driver software key; the driver checks the per-device `Device Parameters` key first and falls back.
- Distribute as **test-signed**:
  - Enable test mode in the guest (`bcdedit /set testsigning on`).
  - Install the test certificate into the guest's trusted store.

This is the most controlled path and avoids licensing ambiguity.

In this repo, the in-tree implementation of this approach is:

- `drivers/windows7/virtio-snd/` (WDM PortCls + WaveRT audio driver)

See also:

- [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) (definitive device/driver contract; transport + required features)
- [`docs/windows/virtio-pci-modern-wdm.md`](./windows/virtio-pci-modern-wdm.md) (WDM modern transport + interrupts bring-up guide)

### Option B: Reuse open-source virtio-win (if license-compatible)

If an existing virtio-win `viosnd` driver supports Windows 7 and the project license is compatible, it could be reused to avoid writing a custom audio miniport.

This option requires a licensing review (see `docs/13-legal-considerations.md`) and validation that the driver supports at
minimum the subset implemented here (fixed-format playback + capture streams, S16_LE @ 48kHz).

## Browser Smoke Test (AudioWorklet)

A standalone smoke test is provided under `web/` to validate AudioWorklet playback and sample-rate handling.

It generates a 48kHz stereo tone and (if needed) linearly resamples it to the *actual* `AudioContext.sampleRate` before writing into the shared ring buffer. This prevents pitch-shift on browsers that ignore the requested sample rate (Safari/iOS commonly uses 44.1kHz).

Because it uses `SharedArrayBuffer`, it must be served with cross-origin isolation headers (COOP/COEP). A minimal local server is included:

```bash
node web/serve-smoke-test.mjs
```

Then open:

```
http://localhost:8000/
```
