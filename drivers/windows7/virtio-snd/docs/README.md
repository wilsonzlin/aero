<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Aero virtio-snd (Windows 7) PortCls/WaveRT driver (render-only)

This directory contains implementation notes for a clean-room **PortCls + WaveRT** Windows 7 audio driver that targets the Aero **virtio-snd** PCI device.

The supported, default build produces `virtiosnd.sys` and exposes a single **render** endpoint:

* 48,000 Hz
* Stereo (2 channels)
* 16-bit PCM little-endian (S16_LE)

## Architecture (what’s built by default)

PortCls miniports:

* `src/adapter.c` — PortCls adapter driver (`PcInitializeAdapterDriver` / `PcAddAdapterDevice`)
* `src/topology.c` — minimal topology miniport (speaker jack + channel config properties)
* `src/wavert.c` — WaveRT miniport + stream (periodic timer/DPC advances the ring position and pushes PCM)

Virtio backend (legacy virtio-pci I/O-port):

* `include/backend.h` — WaveRT “backend” interface used by `wavert.c`
* `src/backend_virtio_legacy.c` — backend implementation that forwards to `aeroviosnd_hw.c`
* `src/aeroviosnd_hw.c` — **legacy virtio-pci I/O-port** bring-up + controlq/txq protocol
* `drivers/windows7/virtio/common` — reusable legacy virtio-pci + split virtqueue implementation

Notes:

* **Queues:** controlq (0) + txq (2) are used for basic playback.
* **Interrupts:** **INTx** only (MSI/MSI-X not implemented yet).
* Because this driver uses the legacy I/O-port interface, it only negotiates the **low 32 bits** of virtio feature flags.

## Additional code in this directory (not built by default)

The repository also contains bring-up code for a modern virtio-pci (MMIO/capability-based) WDM driver (for example `src/virtiosnd_hw.c` and related `virtiosnd_*` modules). That path is still under development and is not required for the PortCls endpoint driver.

Modern virtio transport (bring-up notes):

- Sets up split-ring virtqueues (control/event/tx/rx) using the reusable backend in `virtiosnd_queue_split.c`
  - Note: rxq (capture) is initialized for transport bring-up but capture buffers are not submitted yet.
- Connects **INTx** and routes used-ring completions to the control/TX/RX protocol engines in a DPC
- Includes control/TX/RX protocol engines (`virtiosnd_control.c` / `virtiosnd_tx.c` / `virtiosnd_rx.c`) and thin wrappers (`VirtIoSndHwSendControl` / `VirtIoSndHwSubmitTx`) used by the WaveRT virtio backend (render). Capture endpoint plumbing is not implemented yet.

## Design notes

- Driver architecture overview: [`design.md`](design.md)
- PortCls/WaveRT plan: [`portcls-wavert-design.md`](portcls-wavert-design.md)
- QEMU manual test plan: [`../tests/qemu/README.md`](../tests/qemu/README.md)
- Source tracking (clean-room policy): [`../SOURCES.md`](../SOURCES.md)

## See also

- [`docs/virtio-snd.md`](../../../../docs/virtio-snd.md) — device model scope + request/response layouts
- [`docs/windows-device-contract.md`](../../../../docs/windows-device-contract.md) — PCI IDs + Windows service/INF naming contract
- [`docs/windows7-virtio-driver-contract.md` (virtio-snd section)](../../../../docs/windows7-virtio-driver-contract.md#34-virtio-snd-audio) — queue indices, PCI IDs, and required transport features
- [`docs/windows/virtio-pci-modern-wdm.md`](../../../../docs/windows/virtio-pci-modern-wdm.md) — WDM virtio-pci modern transport bring-up notes (PCI caps, BAR0 mapping, feature negotiation, queues, INTx ISR/DPC)
- Canonical wire format / behavior reference (emulator side):
  - [`crates/aero-virtio/src/devices/snd.rs`](../../../../crates/aero-virtio/src/devices/snd.rs)
  - [`crates/aero-virtio/tests/virtio_snd.rs`](../../../../crates/aero-virtio/tests/virtio_snd.rs)

## Protocol implemented (Aero subset)

This driver targets the **Aero Windows 7 virtio device contract v1** (virtio-snd §3.4).
The contract (and emulator) define two fixed-format PCM streams. The driver currently
only exposes **stream 0** (playback/output) as a Windows render endpoint; capture is not
yet exposed via PortCls.

- Stream 0 (playback/output): 48,000 Hz, stereo (2ch), signed 16-bit little-endian (`S16_LE`)
- Stream 1 (capture/input): 48,000 Hz, mono (1ch), signed 16-bit little-endian (`S16_LE`) (capture endpoint TBD)

Basic playback uses `controlq` + `txq`. The device model also defines `rxq` for capture,
but capture is not implemented by the current PortCls miniport.

All multi-byte fields described below are **little-endian**.

### Controlq (queue 0) commands supported

The controlq request payload begins with a 32-bit `code` and the response always begins with a 32-bit `status`.

Contract v1 requires these request codes (for both streams 0 and 1). The driver’s
control engine implements the v1 subset for both streams, but the current PortCls
integration only drives **stream 0** (playback):

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
- Followed by zero or more entries of `virtio_snd_pcm_info` (Aero returns **0–2** entries depending on whether the requested `start_id..start_id+count` range includes stream 0 and/or 1).

`virtio_snd_pcm_info` layout returned by Aero (`32` bytes):

```
u32 stream_id
u32 features
u64 formats
u64 rates
u8  direction
u8  channels_min
u8  channels_max
u8  reserved[5]
```

Fixed fields used by Aero:

- `features = 0`
- `formats = (1 << VIRTIO_SND_PCM_FMT_S16)`
- `rates = (1 << VIRTIO_SND_PCM_RATE_48000)`
- Stream 0: `direction = VIRTIO_SND_D_OUTPUT (0)`, `channels_min = channels_max = 2`
- Stream 1: `direction = VIRTIO_SND_D_INPUT (1)`, `channels_min = channels_max = 1`

`PCM_SET_PARAMS` request (`24` bytes):

```
u32 code         = 0x0101
u32 stream_id
u32 buffer_bytes
u32 period_bytes
u32 features     = 0
u8  channels
u8  format       = VIRTIO_SND_PCM_FMT_S16 (5)
u8  rate         = VIRTIO_SND_PCM_RATE_48000 (7)
u8  padding      = 0
```

`channels` must match the stream direction:

- Stream 0 (output): `channels = 2`
- Stream 1 (input): `channels = 1`

`PCM_SET_PARAMS` response:

- `u32 status`

`buffer_bytes` and `period_bytes` are currently stored by the emulator for bookkeeping only; they are not yet used to enforce flow control or sizing on the host side.

For `PCM_PREPARE`, `PCM_START`, `PCM_STOP`, `PCM_RELEASE`, the request and response are:

```
u32 code
u32 stream_id
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

## Building

### Supported: WDK10 / MSBuild

CI builds the driver using:

* `drivers/windows7/virtio-snd/virtio-snd.vcxproj`

Configuration notes:

- `Release` builds define `DBG=0` (free build; debug logging compiled out).
- `Debug` builds define `DBG=1` (enables `VIRTIOSND_TRACE*` `DbgPrintEx` logging).

Build outputs are staged under:

- `out/drivers/windows7/virtio-snd/x86/virtiosnd.sys`
- `out/drivers/windows7/virtio-snd/x64/virtiosnd.sys`

### Legacy: WDK 7600 / WDK 7.1 `build.exe` (deprecated)

The original `makefile`/`sources` files are kept for reference, but the supported build path is now MSBuild (WDK10).

If you still need to build with WDK 7.1:

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

3. Copy the built `virtiosnd.sys` into the driver package staging directory:

```text
drivers/windows7/virtio-snd/inf/virtiosnd.sys
```

> `Inf2Cat` hashes every file referenced by the INF, so `virtiosnd.sys` must exist in `inf/` before generating the catalog.

## Prerequisites (host build/sign machine)

Run the signing tooling from a WDK Developer Command Prompt (so the tools are in `PATH`):

- `Inf2Cat.exe`
- `signtool.exe`
- `certutil.exe` (built into Windows)

## Test-signing workflow (CAT + cert + signature)

### 1) Generate a test certificate (on the signing machine)

From `drivers/windows7/virtio-snd/`:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1
```

Expected outputs:

```text
cert\aero-virtio-snd-test.cer
cert\aero-virtio-snd-test.pfx
```

`make-cert.ps1` defaults to generating a **SHA-1-signed** certificate for maximum compatibility with stock Windows 7 SP1.
If you cannot create SHA-1 certificates in your environment, you can opt into SHA-2 by rerunning with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\make-cert.ps1 -AllowSha2CertFallback
```

> A SHA-2-signed certificate may require Windows 7 SHA-2 updates (KB3033929 / KB4474419) on the test machine.

### 2) Generate the catalog (CAT)

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\make-cat.cmd
```

Expected output:

```text
inf\aero-virtio-snd.cat
inf\virtio-snd.cat
```

`virtio-snd.cat` is only generated if `inf\virtio-snd.inf` is present.

### 3) Sign the SYS + CAT

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\sign-driver.cmd
```

This signs:

- `inf\virtiosnd.sys`
- `inf\aero-virtio-snd.cat`
- `inf\virtio-snd.cat` (if `inf\virtio-snd.inf` is present)

## Windows 7 guest setup

### Enable test-signing mode

On the Windows 7 guest (elevated cmd):

```cmd
bcdedit /set testsigning on
shutdown /r /t 0
```

### Install the test certificate (guest)

Copy `cert\aero-virtio-snd-test.cer` to the guest, then run (elevated PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-test-cert.ps1 -CertPath .\cert\aero-virtio-snd-test.cer
```

This installs the cert into:

- LocalMachine **Trusted Root Certification Authorities**
- LocalMachine **Trusted Publishers**

## Packaging for Guest Tools / ISO integration

After building + signing, stage a per-arch driver folder under `release/`:

```powershell
# Auto-detect arch from inf\virtiosnd.sys and stage into release\<arch>\virtio-snd\
powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1
```

To force a specific architecture label (and validate the SYS matches it):

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Arch amd64
```

The staged output can be copied into:

```text
guest-tools\drivers\<arch>\virtio-snd\
```

## Installing (development/testing)

1. Ensure the package directory contains:
   - `aero-virtio-snd.inf`
   - `virtiosnd.sys`
   - `aero-virtio-snd.cat` (signed)
   - (Optional) `virtio-snd.inf` + `virtio-snd.cat` (signed)
2. Use Device Manager → Update Driver → "Have Disk..." and point to `inf\` (or `release\<arch>\virtio-snd\` once packaged). Pick the desired INF when prompted.
2. Use Device Manager → Update Driver → "Have Disk..." and point to `inf\` (or `release\<arch>\virtio-snd\` once packaged). Pick the desired INF when prompted.

For offline/slipstream installation into Windows 7 images (WIM or offline OS), see:

- `../tests/offline-install/README.md`

For a repeatable end-to-end validation flow under QEMU (device enumeration → driver binding → endpoint presence → playback), see:

- `../tests/qemu/README.md`
