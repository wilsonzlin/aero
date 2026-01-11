# Virtio-snd (Paravirtual Audio) Device

This repository includes a minimal **virtio-snd** device model intended to be used as a high-performance alternative to full Intel HDA emulation once guest drivers exist.

See also:

- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide for Windows 7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.

Scope:

- **2 PCM streams**
  - Stream `0`: playback/output, **stereo (2ch)**, 48kHz, signed 16-bit little-endian (S16_LE)
  - Stream `1`: capture/input, **mono (1ch)**, 48kHz, signed 16-bit little-endian (S16_LE)
- **Split virtqueues** (virtio 1.2 subset)
- **Control queue** for stream discovery/configuration
- **TX queue** for PCM frame submission (playback)
- **RX queue** for PCM capture (recording)

## Device Configuration

The device reports two PCM streams:

- `streams = 2`
- `jacks = 0`
- `chmaps = 0`

## Supported Control Commands

The control virtqueue accepts requests prefixed by a 32-bit little-endian `code`. The response always starts with a 32-bit little-endian **status**.

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

### AudioWorklet ring buffer layout

The AudioWorklet ring buffer used by the AU-WORKLET path uses **frame indices** (not sample indices).
See `web/src/platform/audio.ts` and `web/src/platform/audio-worklet-processor.js` for the canonical
layout and wrap-around behavior.

## Audio Output Path

TX PCM samples are written into the same **AudioWorklet ring buffer** abstraction used by the AU-WORKLET layer (`web/src/platform/audio.ts`). The current implementation converts interleaved S16_LE samples into interleaved `f32` samples and writes them into the Float32 ring buffer consumed by `web/src/platform/audio-worklet-processor.js`.

## Windows 7 Driver Strategy

Windows 7 does not ship a virtio-snd driver. Expected options:

### Option A: Custom test-signed WDM audio miniport

- Implement a WDM audio miniport (PortCls / WaveRT) that:
  - Enumerates as a standard Windows audio endpoint.
  - Uses the virtio PCI transport and split virtqueues to:
    - Query stream capabilities (`PCM_INFO`)
    - Negotiate fixed params (`PCM_SET_PARAMS`)
    - Start/stop the stream and submit PCM buffers via the TX queue.
- Distribute as **test-signed**:
  - Enable test mode in the guest (`bcdedit /set testsigning on`).
  - Install the test certificate into the guest's trusted store.

This is the most controlled path and avoids licensing ambiguity.

### Option B: Reuse open-source virtio-win (if license-compatible)

If an existing virtio-win `viosnd` driver supports Windows 7 and the project license is compatible, it could be reused to avoid writing a custom audio miniport.

This option requires a licensing review (see `docs/13-legal-considerations.md`) and validation that the driver supports the subset implemented here (single playback stream, S16_LE @ 48kHz).

## Browser Smoke Test (AudioWorklet)

A standalone smoke test is provided under `web/` to validate AudioWorklet playback of interleaved S16_LE stereo data at 48kHz.

Because it uses `SharedArrayBuffer`, it must be served with cross-origin isolation headers (COOP/COEP). A minimal local server is included:

```bash
node web/serve-smoke-test.mjs
```

Then open:

```
http://localhost:8000/
```
