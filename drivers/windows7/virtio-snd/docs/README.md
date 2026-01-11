<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Aero virtio-snd (Windows 7) PortCls/WaveRT driver (render + capture)

This directory contains implementation notes for a clean-room **PortCls + WaveRT** Windows 7 audio driver that targets the Aero **virtio-snd** PCI device.

This `docs/README.md` is the **source of truth** for the in-tree Windows 7 `virtio-snd` driver package:

- What the shipped/default build targets
- Which PCI HWIDs the INF binds to
- What (if any) QEMU configuration is supported
- What render/capture endpoints exist today

The shipped driver package produces `aero_virtio_snd.sys` and exposes two fixed-format endpoints:

Render (output, stream 0):

* 48,000 Hz
* Stereo (2 channels)
* 16-bit PCM little-endian (S16_LE)

Capture (input, stream 1):

* 48,000 Hz
* Mono (1 channel)
* 16-bit PCM little-endian (S16_LE)

## What ships / compatibility contract

### Default build: **AERO-W7-VIRTIO v1** (modern-only)

The default/shipped driver package targets the definitive Aero Windows 7 virtio contract:

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md#34-virtio-snd-audio) (**Contract ID:** `AERO-W7-VIRTIO`, **v1.0**)

Contract v1 is **virtio-pci modern only** (PCI vendor-specific capabilities + MMIO). Transitional/legacy virtio-pci I/O-port transport is explicitly out of scope.

Windows tooling (Guest Tools manifests, CI tooling, etc.) must remain consistent with that contract:

- [`docs/windows-device-contract.md`](../../../../docs/windows-device-contract.md)
- [`docs/windows-device-contract.json`](../../../../docs/windows-device-contract.json)

### INF hardware IDs (what Windows 7 will bind to)

The shipped INF is intentionally strict and only matches Aero contract v1 devices:

- **Required (as-shipped):** `PCI\VEN_1AF4&DEV_1059&REV_01`
  - This is the only active HWID match in `inf/aero_virtio_snd.inf`.
- **Optional (tighter, commented out in the INF):** `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

The INF does **not** match:

- Any legacy/transitional virtio-pci device IDs.
- Any “short form” without the revision gate (for example `PCI\VEN_1AF4&DEV_1059`), even though those appear in the Windows device contract manifest for tooling convenience.

The source tree also contains a legacy filename alias INF checked in as `inf/virtio-snd.inf.disabled` for compatibility
with tooling/workflows that still reference `virtio-snd.inf`. When enabled (rename to `inf/virtio-snd.inf`), it
installs the same driver/service as `aero_virtio_snd.inf` and matches the same Aero contract v1 HWIDs.

CI packaging stages only `inf/aero_virtio_snd.inf` (see `ci-package.json`) to avoid shipping multiple INFs that match
the same hardware IDs. The default CI/Guest Tools driver bundle therefore includes only the strict Aero contract v1
package (the opt-in QEMU compatibility package is not staged by default).

See also: [`pci-hwids.md`](pci-hwids.md) and `inf/aero_virtio_snd.inf`.

### Expected virtio features and queues (contract v1 §3.4)

Virtio feature bits:

- MUST negotiate `VIRTIO_F_VERSION_1` (bit `32`)
- MUST negotiate `VIRTIO_F_RING_INDIRECT_DESC` (bit `28`)
- MUST NOT rely on `VIRTIO_F_RING_EVENT_IDX` / packed rings in contract v1

Virtqueues (indices and sizes):

| Queue index | Name | Queue size |
|---:|---|---:|
| 0 | `controlq` | 64 |
| 1 | `eventq` | 64 |
| 2 | `txq` | 256 |
| 3 | `rxq` | 64 |

### Render vs capture status

The contract defines **two** fixed-format PCM streams:

- Stream 0 (playback/output): 48,000 Hz, stereo (2ch), signed 16-bit little-endian (`S16_LE`)
- Stream 1 (capture/input): 48,000 Hz, mono (1ch), signed 16-bit little-endian (`S16_LE`)

The PortCls miniports expose both streams as Windows endpoints:

- Stream 0 (playback/output): Windows render endpoint
- Stream 1 (capture/input): Windows capture endpoint

### QEMU support (manual testing)

Because the INF requires `PCI\VEN_1AF4&DEV_1059&REV_01`, stock QEMU defaults often will **not** bind without additional configuration.

To test the strict Aero contract v1 identity under QEMU (only if your QEMU build exposes these properties):

```bash
-device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
```

If your QEMU build cannot set the PCI revision to `0x01`, the stock INF will not bind. In that case, use the
opt-in transitional package instead:

- `inf/aero-virtio-snd-legacy.inf`
- `virtiosnd_legacy.sys` (MSBuild `Configuration=Legacy`)

See the manual QEMU test plan for details: [`../tests/qemu/README.md`](../tests/qemu/README.md).

## Build variants (Aero contract vs QEMU)

The driver sources support two *packaging* variants so that QEMU testing can be enabled without
weakening the default Aero contract-v1 INF:

| Variant | MSBuild config | SYS | INF | Binds to |
| --- | --- | --- | --- | --- |
| **Aero contract v1 (default)** | `Release` | `aero_virtio_snd.sys` | `inf/aero_virtio_snd.inf` | `PCI\VEN_1AF4&DEV_1059&REV_01` |
| **QEMU compatibility (optional)** | `Legacy` | `virtiosnd_legacy.sys` | `inf/aero-virtio-snd-legacy.inf` | `PCI\VEN_1AF4&DEV_1018` (transitional; no revision gate) |

The two packaging INFs intentionally have **no overlapping hardware IDs**, so they do not compete
for the same device.

Note: only the **Aero contract v1** variant is CI-packaged by default; the QEMU compatibility variant is a manual,
opt-in package.

Additionally, the repo keeps an older legacy virtio-pci **I/O-port** transport build for bring-up:

- MSBuild project: `virtio-snd-ioport-legacy.vcxproj`
- SYS: `virtiosnd_ioport.sys`
- INF: `inf/aero-virtio-snd-ioport.inf` (matches `PCI\VEN_1AF4&DEV_1018&REV_00`)

This I/O-port variant is not part of `AERO-W7-VIRTIO` v1 and is not staged by CI/Guest Tools.

## Architecture (what’s built by default)

PortCls miniports:

- Registers `Wave` (PortWaveRT + IMiniportWaveRT) and `Topology` (PortTopology + IMiniportTopology) subdevices.
- Physically bridges them via `PcRegisterPhysicalConnection`.
- Exposes fixed-format endpoints:
  - Render (stream 0): 48,000 Hz, stereo (2ch), 16-bit PCM LE (S16_LE)
  - Capture (stream 1): 48,000 Hz, mono (1ch), 16-bit PCM LE (S16_LE)
- Uses a QPC-based clock (`KeQueryPerformanceCounter`) for position reporting (virtqueue completions are not a reliable clock in Aero).
- Uses a periodic timer/DPC “software DMA” model that:
  - for render: submits one period of PCM to a backend callback and advances play position (backpressure-aware)
  - for capture: submits one RX period buffer; RX completion advances write position and signals the WaveRT notification event

* `src/adapter.c` — PortCls adapter driver (`PcInitializeAdapterDriver` / `PcAddAdapterDevice`)
* `src/topology.c` — topology miniport (speaker + microphone jacks + channel config properties)
* `src/wavert.c` — WaveRT miniport + stream (fixed-format render + capture)

Backend layer (WaveRT ↔ virtio-snd):

- The WaveRT miniport uses the backend interface in `include/backend.h`.
- Default backend: `src/backend_virtio.c` (submits one period of PCM to `txq` each tick).
- Fallback backend: `src/backend_null.c` (silent; used for debugging and when virtio bring-up fails).
- To force the Null backend even when virtio is available, set `ForceNullBackend` (`REG_DWORD`) = `1` under the device's **Device Parameters** key.
  - When `ForceNullBackend=1`, the adapter will also tolerate virtio transport start failures (no Code 10) so PortCls/WaveRT behavior can be tested even if the virtio-snd device/emulator is unavailable.
  - With the Null backend, both render and capture endpoints remain functional from the Windows audio stack’s perspective, but record/play silence.

Virtio transport + protocol engines (AERO-W7-VIRTIO v1 modern transport):

* `include/backend.h` — WaveRT “backend” interface used by `wavert.c`
* `src/backend_virtio.c` — virtio-snd backend implementation (PortCls ↔ virtio glue)
* `src/backend_null.c` — silent backend implementation (fallback / debugging)
* `src/virtiosnd_hw.c` — virtio-snd WDM bring-up + queue ownership
  - `VirtIoSndStartHardware` / `VirtIoSndStopHardware` handle BAR0 MMIO discovery/mapping, PCI vendor capability parsing, feature negotiation, queue setup, INTx wiring, and reset/teardown
* Canonical virtio-pci modern transport (BAR0 MMIO + PCI vendor capability parsing):
  - `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c`
  - `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
* INTx integration:
  - `drivers/windows7/virtio/common/src/virtio_pci_intx_wdm.c`
  - `src/virtiosnd_intx.c`
* `src/virtiosnd_queue_split.c` — split-ring virtqueue wrapper used by the virtio-snd engines
* `src/virtiosnd_control.c` / `src/virtiosnd_tx.c` / `src/virtiosnd_rx.c` — control/TX/RX protocol engines
* `drivers/windows7/virtio/common/src/virtio_pci_contract.c` — `AERO-W7-VIRTIO` v1 contract identity validation used at `START_DEVICE` (requires `PCI\VEN_1AF4&DEV_1059&REV_01`)
* Shared split-ring virtqueue implementation:
  - `drivers/windows/virtio/common/virtqueue_split.c`

Scatter/gather helpers (WaveRT cyclic buffer → virtio descriptors):

* `include/virtiosnd_sg.h` / `src/virtiosnd_sg.c` — DISPATCH_LEVEL-safe helper that maps an MDL-backed circular PCM buffer region into a compact `(phys,len)` list (page-split + physical coalescing + wrap support).
* `include/virtiosnd_sg_tx.h` / `src/virtiosnd_sg_tx.c` — convenience wrapper that emits `VIRTIOSND_SG` entries (the SG type used by the `virtiosnd_queue.h` abstraction).

Notes:

* **Queues:** `controlq`/`eventq`/`txq`/`rxq` are initialized per contract v1; render uses `controlq` (0) + `txq` (2), capture uses `controlq` (0) + `rxq` (3).
  - `eventq` is initialized for contract conformance but is not currently used by the PortCls endpoints.
* **Interrupts:** **INTx** only (MSI/MSI-X not currently used by this driver package).
* **Feature negotiation:** contract v1 requires 64-bit feature negotiation (`VIRTIO_F_VERSION_1` is bit 32) and `VIRTIO_F_RING_INDIRECT_DESC` (bit 28).

## Legacy / transitional virtio-pci paths (opt-in bring-up)

The repository also contains an older **legacy/transitional virtio-pci I/O-port** bring-up path (for example
`src/backend_virtio_legacy.c`, `src/aero_virtio_snd_ioport_hw.c`, and `drivers/windows7/virtio/common/src/virtio_pci_legacy.c`).
That code is kept for historical bring-up, but it is **not part of the `AERO-W7-VIRTIO` v1 contract**: it only
negotiates the low 32 bits of virtio feature flags (so it cannot negotiate `VIRTIO_F_VERSION_1`), and the default
contract INF (`inf/aero_virtio_snd.inf`) does not bind to transitional IDs (use `inf/aero-virtio-snd-legacy.inf` for
stock QEMU defaults).

CI guardrail: PRs must keep `aero_virtio_snd.vcxproj` on the modern-only backend. See `scripts/ci/check-virtio-snd-vcxproj-sources.py`.

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
The contract (and emulator) define two fixed-format PCM streams. The shipped driver
package exposes both streams via PortCls/WaveRT:

- Stream 0 (playback/output): 48,000 Hz, stereo (2ch), signed 16-bit little-endian (`S16_LE`)
- Stream 1 (capture/input): 48,000 Hz, mono (1ch), signed 16-bit little-endian (`S16_LE`)

Basic playback uses `controlq` + `txq`. Capture uses `controlq` + `rxq`; the emulator fills silence
when host input is unavailable.

All multi-byte fields described below are **little-endian**.

### Controlq (queue 0) commands supported

The controlq request payload begins with a 32-bit `code` and the response always begins with a 32-bit `status`.

Contract v1 defines these request codes (for both streams 0 and 1). The current
PortCls miniports use them for **stream 0** (render) and **stream 1** (capture):

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

* `drivers/windows7/virtio-snd/aero_virtio_snd.vcxproj`

Configuration notes:

- The MSBuild project ships only `Release` and `Legacy` configurations, and both define `DBG=0`
  (free build; debug logging compiled out).
- To enable `VIRTIOSND_TRACE*` `DbgPrintEx` logging, use a WinDDK 7600 checked build or create a local
  MSBuild configuration that sets `DBG=1`.

Build outputs are staged under:

- `Release` (default):
  - `out/drivers/windows7/virtio-snd/x86/aero_virtio_snd.sys`
  - `out/drivers/windows7/virtio-snd/x64/aero_virtio_snd.sys`
- `Legacy` (optional QEMU/transitional package):
  - `out/drivers/windows7/virtio-snd/x86/virtiosnd_legacy.sys`
  - `out/drivers/windows7/virtio-snd/x64/virtiosnd_legacy.sys`

### Legacy: WinDDK 7600 `build.exe` (deprecated)

The original `makefile`/`sources` files are kept for reference, but the supported build path is now MSBuild (WDK10).

If you still need to build with WinDDK 7600:

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

3. Copy the built `aero_virtio_snd.sys` into the driver package staging directory:

```text
drivers/windows7/virtio-snd/inf/aero_virtio_snd.sys
```

> `Inf2Cat` hashes every file referenced by the INF, so `aero_virtio_snd.sys` must exist in `inf/` before generating the catalog.

Instead of copying manually, you can use:

```powershell
# Stage the x86 or amd64 build output into inf\aero_virtio_snd.sys
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64
```

For the optional transitional/QEMU package:

```powershell
# Stage into inf\virtiosnd_legacy.sys
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64 -Variant legacy
```

To build a signed `release/` package in one step (stages SYS → Inf2Cat → sign → package):

```powershell
# Contract v1 (default):
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -InputDir <build-output-root>

# Transitional/QEMU:
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -Variant legacy -InputDir <build-output-root>
```

Add `-Zip` to also create deterministic `release/out/*.zip` bundles.

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

This generates catalogs for the **contract v1** package.

To generate the optional transitional/QEMU catalog, run:

```cmd
.\scripts\make-cat.cmd legacy
```

Expected output (`make-cat.cmd`):

```text
inf\aero_virtio_snd.cat
```
### 3) Sign the SYS + CAT

From `drivers/windows7/virtio-snd/`:

```cmd
.\scripts\sign-driver.cmd
```

This signs (contract v1):

- `inf\aero_virtio_snd.sys`
- `inf\aero_virtio_snd.cat`

To sign the transitional/QEMU package, run:

```cmd
.\scripts\sign-driver.cmd legacy
```

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
# Auto-detect arch from inf\aero_virtio_snd.sys and stage into release\<arch>\virtio-snd\
powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1
```

For the transitional/QEMU package:

```powershell
# Auto-detect arch from inf\virtiosnd_legacy.sys and stage into release\<arch>\virtio-snd-legacy\
powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Variant legacy
```

To force a specific architecture label (and validate the SYS matches it):

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Arch amd64
```

The staged output can be copied into:

```text
guest-tools\drivers\<arch>\virtio-snd\
```

Note: the default Guest Tools tree does not include the QEMU compatibility variant.

## Installing (development/testing)

1. Ensure the package directory contains the files for the variant you want to install:
    
    - Contract v1 (default):
      - `aero_virtio_snd.inf`
      - `aero_virtio_snd.sys`
      - `aero_virtio_snd.cat` (signed)
    - Transitional/QEMU (optional):
      - `aero-virtio-snd-legacy.inf`
      - `virtiosnd_legacy.sys`
      - `aero-virtio-snd-legacy.cat` (signed)
    - Legacy I/O-port transport (optional bring-up; not part of `AERO-W7-VIRTIO` v1):
      - `aero-virtio-snd-ioport.inf`
      - `virtiosnd_ioport.sys`
      - `aero-virtio-snd-ioport.cat` (signed)
    - Optional legacy filename alias:
      - `virtio-snd.inf` (rename `virtio-snd.inf.disabled` to enable)
2. Use Device Manager → Update Driver → "Have Disk..." and point to `inf\\` (or `release\\<arch>\\virtio-snd\\` once packaged). Pick the desired INF when prompted.

INF selection note:

- `aero_virtio_snd.inf` is the **canonical** Aero contract v1 package (matches `PCI\VEN_1AF4&DEV_1059&REV_01` and installs service `aero_virtio_snd`).
- `aero-virtio-snd-legacy.inf` is an opt-in QEMU compatibility package (binds the transitional virtio-snd PCI ID `PCI\VEN_1AF4&DEV_1018` with no revision gate and installs service `aeroviosnd_legacy`).
- `aero-virtio-snd-ioport.inf` is an opt-in legacy I/O-port transport package (binds `PCI\VEN_1AF4&DEV_1018&REV_00` and installs service `aeroviosnd_ioport`).
- `virtio-snd.inf` is a legacy filename alias kept for compatibility with older tooling/workflows.
  It installs the same driver/service and matches the same Aero contract v1 HWIDs as `aero_virtio_snd.inf`,
  but is disabled by default to avoid accidentally installing **two** INFs that match the same HWIDs.

  To avoid accidentally installing **two** INFs that match the same HWIDs, the alias INF is checked in as
  `virtio-snd.inf.disabled`; rename it back to `virtio-snd.inf` if you need the legacy filename.

For offline/slipstream installation into Windows 7 images (WIM or offline OS), see:

- `../tests/offline-install/README.md`

For a repeatable end-to-end validation flow under QEMU (device enumeration → driver binding → endpoint presence → playback), see:

- `../tests/qemu/README.md`

### Contract v1 hardware IDs (reference)

Contract v1 virtio-snd devices enumerate as:

- Vendor ID: `VEN_1AF4`
- Device ID: `DEV_1059` (`0x1040 + VIRTIO_ID_SOUND` where `VIRTIO_ID_SOUND = 25`)
- Revision ID: `REV_01` (contract major version = 1)
- Subsystem ID: `SUBSYS_0019xxxx` (Aero uses `SUBSYS_00191AF4`)

`inf/aero_virtio_snd.inf` matches the **revision-gated** modern HWID:

- `PCI\VEN_1AF4&DEV_1059&REV_01`

For additional safety in environments that expose multiple virtio-snd devices, you can further restrict the match to Aero’s subsystem ID:

- `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

Notes:

- The **transitional/legacy** virtio-snd PCI device ID (`DEV_1018`) is intentionally **not** matched by this INF (Aero contract v1 is modern-only).
- This WDM driver currently uses **INTx only**. The INF does **not** include the registry settings required to request MSI/MSI-X from Windows.

If this README or the INF disagrees with `AERO-W7-VIRTIO`, treat the contract as authoritative.
