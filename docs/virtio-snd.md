# Virtio-snd (Paravirtual Audio) Device

This repository includes a minimal **virtio-snd** device model (`crates/aero-virtio`, `aero_virtio::devices::snd`) intended to be used as a high-performance alternative to full Intel HDA emulation once guest drivers exist.

The legacy virtio-snd implementation under `crates/emulator/src/io/virtio/devices/snd.rs` is retained behind the `emulator/legacy-audio` feature for reference.

See also:

- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide for Windows 7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`windows-device-contract.md`](./windows-device-contract.md) — PCI ID + driver service naming contract (Aero).
- [`drivers/protocol/virtio/`](../drivers/protocol/virtio/) — canonical `#[repr(C)]` message layouts (with Rust unit tests) shared between the guest driver and device model.

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
- `jacks = 0`
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
    - Negotiate fixed params (`PCM_SET_PARAMS`)
    - Start/stop streams and submit PCM buffers via the TX queue (playback) and RX queue (capture).
  - Works with PCI **INTx** (contract v1 requires INTx; ISR status is read-to-ack to deassert the line).
- Distribute as **test-signed**:
  - Enable test mode in the guest (`bcdedit /set testsigning on`).
  - Install the test certificate into the guest's trusted store.

This is the most controlled path and avoids licensing ambiguity.

In this repo, the in-tree implementation of this approach is:

- `drivers/windows7/virtio-snd/` (WDM PortCls + WaveRT driver skeleton)

See also:

- [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) (definitive device/driver contract; transport + required features)
- [`docs/windows/virtio-pci-modern-wdm.md`](./windows/virtio-pci-modern-wdm.md) (WDM modern transport + INTx bring-up guide)

### Option B: Reuse open-source virtio-win (if license-compatible)

If an existing virtio-win `viosnd` driver supports Windows 7 and the project license is compatible, it could be reused to avoid writing a custom audio miniport.

This option requires a licensing review (see `docs/13-legal-considerations.md`) and validation that the driver supports the subset implemented here (fixed-format playback + capture streams, S16_LE @ 48kHz).

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
