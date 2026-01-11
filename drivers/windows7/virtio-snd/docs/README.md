# virtio-snd (Windows 7) driver skeleton

This directory contains an initial, clean-room **WDM kernel-mode function driver** for a PCI virtio-snd device.

The driver currently:

- Loads on Windows 7 SP1 (x86/x64)
- Attaches to the PCI PDO
- Handles basic PnP (START/STOP/REMOVE) and maps BAR resources (best-effort)

It **does not** yet implement virtio-pci transport, virtqueues, or any PortCls miniports, so it will not expose audio endpoints yet.

## Compatibility / Aero contract v1

This driver package targets the **Aero Windows 7 virtio device contract v1**:

- **Transport:** virtio-pci **modern** only (virtio 1.0+; PCI vendor-specific capabilities + MMIO)
- **Interrupts:** **INTx required** (this WDM driver does not opt into MSI/MSI-X)

See: [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md) (virtio-snd §3.4, versioning §4).

## Design notes

- PortCls/WaveRT plan: [`portcls-wavert-design.md`](portcls-wavert-design.md)

## See also (Aero docs / host implementation)

- [`docs/virtio-snd.md`](../../../../docs/virtio-snd.md) — overall device model scope + request/response formats.
- [`docs/windows7-virtio-driver-contract.md` (virtio-snd section)](../../../../docs/windows7-virtio-driver-contract.md#34-virtio-snd-audio) — queue indices, PCI IDs, and required transport features.
- Canonical wire format / behavior reference (emulator side):
  - [`crates/aero-virtio/src/devices/snd.rs`](../../../../crates/aero-virtio/src/devices/snd.rs)
  - [`crates/aero-virtio/tests/virtio_snd.rs`](../../../../crates/aero-virtio/tests/virtio_snd.rs)

## Protocol implemented (Aero subset)

This driver is intended to target the **minimal** virtio-snd subset implemented by the Aero emulator today:

- Exactly **one** PCM playback stream: `stream_id = 0`
- Format is effectively fixed to **stereo**, **48,000 Hz**, **signed 16-bit little-endian** (`S16_LE`)
- Only the **controlq** and **txq** are used for basic playback

All multi-byte fields described below are **little-endian**.

### Controlq (queue 0) commands supported

The controlq request payload begins with a 32-bit `code` and the response always begins with a 32-bit `status`.

Only these request codes are implemented (all for **stream 0**):

- `VIRTIO_SND_R_PCM_INFO (0x0100)`
- `VIRTIO_SND_R_PCM_SET_PARAMS (0x0101)`
- `VIRTIO_SND_R_PCM_PREPARE (0x0102)`
- `VIRTIO_SND_R_PCM_START (0x0104)`
- `VIRTIO_SND_R_PCM_STOP (0x0105)`
- `VIRTIO_SND_R_PCM_RELEASE (0x0103)`

All jack/chmap requests return `VIRTIO_SND_S_NOT_SUPP`.

#### Minimal control state machine (device-side behavior)

The emulator’s PCM stream state is:

`Idle` → (`SET_PARAMS`) → `ParamsSet` → (`PREPARE`) → `Prepared` → (`START`) → `Running`

- `START` while already `Running` returns `OK` (idempotent).
- `PREPARE` while already `Prepared` returns `OK` (idempotent).
- `STOP` transitions `Running` → `Prepared`.
- `RELEASE` resets the stream back to `Idle` and clears the previously-set params.
- TX submissions are only accepted in `Running` (see txq below).

Notes (important for driver behavior):

- `SET_PARAMS` overwrites any previous params and unconditionally transitions to `ParamsSet` (even if called while `Prepared`/`Running`).
- `RELEASE` always returns `OK` and resets the stream back to `Idle` (even if called while `Running`).
- For `PREPARE`/`START`/`STOP` and for TX submissions, a well-formed request in an invalid state returns `VIRTIO_SND_S_IO_ERR`.

#### Control message layouts used by Aero

> Note: these are *not* a full virtio-snd spec description; they describe the concrete subset used by `crates/aero-virtio`.

`PCM_INFO` request (`12` bytes):

```
u32 code      = 0x0100
u32 start_id
u32 count
```

`PCM_INFO` response:

- `u32 status`
- Followed by `count` entries of `virtio_snd_pcm_info` (Aero returns **0 or 1** entry depending on whether the range includes stream 0).

`virtio_snd_pcm_info` layout returned by Aero (`32` bytes):

```
u32 stream_id      = 0
u32 features       = 0
u64 formats        = (1 << VIRTIO_SND_PCM_FMT_S16)
u64 rates          = (1 << VIRTIO_SND_PCM_RATE_48000)
u8  direction      = VIRTIO_SND_D_OUTPUT (0)
u8  channels_min   = 2
u8  channels_max   = 2
u8  reserved[5]    = 0
```

`PCM_SET_PARAMS` request (`24` bytes):

```
u32 code         = 0x0101
u32 stream_id    = 0
u32 buffer_bytes
u32 period_bytes
u32 features     = 0
u8  channels     = 2
u8  format       = VIRTIO_SND_PCM_FMT_S16 (5)
u8  rate         = VIRTIO_SND_PCM_RATE_48000 (7)
u8  padding      = 0
```

`PCM_SET_PARAMS` response:

- `u32 status`

`buffer_bytes` and `period_bytes` are currently stored by the emulator for bookkeeping only; they are not yet used to enforce flow control or sizing on the host side.

For `PCM_PREPARE`, `PCM_START`, `PCM_STOP`, `PCM_RELEASE`, the request and response are:

```
u32 code
u32 stream_id = 0
```

Response:

- `u32 status`

### TX queue (queue 2) buffer format (playback)

Each playback submission is one descriptor chain containing:

1. One or more **device-readable** ("out") descriptors that concatenate to:
   - an **8-byte header**, followed by
   - raw PCM bytes
2. At least one **device-writable** ("in") descriptor with an **8-byte status response**

The TX payload (guest → device) is:

```
u32 stream_id = 0
u32 reserved  = 0
u8  pcm[]     // interleaved S16_LE stereo frames
```

PCM requirements enforced by the current emulator implementation:

- The payload after the 8-byte header must contain a whole number of 16-bit samples (`pcm_len % 2 == 0`).
- The total sample count must be a whole number of **stereo frames** (`samples % 2 == 0`), i.e. `pcm_len % 4 == 0`.

The TX response (device → guest) is always 8 bytes:

```
u32 status
u32 latency_bytes = 0   // currently always 0 in Aero
```

### Status codes + NTSTATUS mapping (guest-side)

Both controlq responses and txq responses use these virtio-snd status codes:

| virtio-snd status | Value | Meaning (Aero) | Recommended NTSTATUS |
|---|---:|---|---|
| `VIRTIO_SND_S_OK` | `0` | Success | `STATUS_SUCCESS` |
| `VIRTIO_SND_S_BAD_MSG` | `1` | Malformed request, invalid `stream_id`, or invalid PCM framing | `STATUS_INVALID_PARAMETER` |
| `VIRTIO_SND_S_NOT_SUPP` | `2` | Well-formed but unsupported command/format | `STATUS_NOT_SUPPORTED` |
| `VIRTIO_SND_S_IO_ERR` | `3` | Valid request but invalid stream state (e.g. TX before `START`) | `STATUS_INVALID_DEVICE_STATE` |

## Streaming model

This is a **push** model: the guest driver is responsible for submitting PCM "periods" into `txq` at (approximately) real-time rate.

Key semantics of the current Aero device model:

- The device **accepts** TX buffers by copying the PCM samples into a host-side ring buffer (see `crates/aero-virtio/src/devices/snd.rs`).
- On **underrun**, the host audio backend outputs **silence** and continues (see the ring buffer behavior in `crates/aero-audio/src/ring.rs`).
- On **overfill/overrun**, the host backend may **drop the oldest audio** to keep latency bounded (also `crates/aero-audio/src/ring.rs`).

### What a TX completion means (important)

A used-ring completion for a TX descriptor chain means:

- the device has **validated** the header/framing and
- has **accepted** the samples into the host ring buffer (or dropped them if overfilled)

It does **not** mean that the audio has actually been *played* by the host yet.

### WaveRT / period scheduling expectation (Windows 7 integration point)

Because TX completions can happen "as fast as the guest can submit" (they are decoupled from real playback time), a PortCls/WaveRT miniport should:

- Use the WaveRT **period timer / service group callback** as the pacing clock.
- Submit exactly one period’s worth of PCM per period tick (or keep a small bounded number of periods queued).
- Treat TX completions primarily as a resource-recycling signal (e.g. to reuse common buffers / ring descriptors), **not** as a playback-position signal.

Practical sizing notes:

- `period_bytes` should be a multiple of the **frame size** (`4` bytes at S16_LE stereo).
- At 48kHz, a 10ms period is `480 frames * 4 bytes/frame = 1920 bytes` (example only).
- `buffer_bytes` should match the WaveRT cyclic buffer size (often `N * period_bytes`). The emulator does not enforce these values today, but future implementations may.

If the miniport submits PCM faster than the period schedule (e.g. "fill the queue until it’s full"), the host ring buffer will eventually overfill and audio will glitch due to dropped samples.

## Building (WDK 7600 / WDK 7.1)

1. Open a WDK build environment:
   - **Windows 7 x86 Free Build Environment** (x86)
   - **Windows 7 x64 Free Build Environment** (x64)
   - Checked builds work as well (DBG output is enabled only in checked builds).

2. Build from the driver root:

```
cd drivers\windows7\virtio-snd
build -cZ
```

The output will be under `objfre_win7_*` (or `objchk_win7_*` for checked builds).

## Installing (development/testing)

1. Copy `virtiosnd.sys` next to `inf\virtio-snd.inf`.
2. Use Device Manager → Update Driver → "Have Disk..." and point to the `inf` directory.

### PCI Hardware IDs

Contract v1 virtio-snd devices enumerate as:

- Vendor ID: `VEN_1AF4`
- Device ID: `DEV_1059` (`0x1040 + VIRTIO_ID_SOUND` where `VIRTIO_ID_SOUND = 25`)
- Revision ID: `REV_01` (contract major version = 1)
- Subsystem ID: `SUBSYS_0003xxxx` (Aero uses `SUBSYS_00031AF4`)

`inf/virtio-snd.inf` matches the **revision-gated** modern HWID:

- `PCI\VEN_1AF4&DEV_1059&REV_01`

For additional safety in environments that expose multiple virtio-snd devices, you can further restrict the match to Aero’s subsystem ID:

- `PCI\VEN_1AF4&DEV_1059&SUBSYS_00031AF4&REV_01`

Notes:

- The **transitional/legacy** virtio-snd PCI device ID (`DEV_1018`) is intentionally **not** matched by this INF (Aero contract v1 is modern-only).
- This WDM driver currently uses **INTx only**. The INF does **not** include the registry settings required to request MSI/MSI-X from Windows.
