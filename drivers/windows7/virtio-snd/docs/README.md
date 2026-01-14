<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Aero virtio-snd (Windows 7) PortCls/WaveRT driver (render + capture)

This directory contains implementation notes for a clean-room **PortCls + WaveRT** Windows 7 audio driver that targets the Aero **virtio-snd** PCI device.

This `docs/README.md` is the **source of truth** for the in-tree Windows 7 `virtio-snd` driver package:

- What the shipped/default build targets
- Which PCI HWIDs the INF binds to
- What (if any) QEMU configuration is supported
- What render/capture endpoints exist today

The shipped driver package produces `aero_virtio_snd.sys` and exposes two endpoints (render + capture).

The **contract-v1 baseline** format is always supported and is used as a safe default.

For **contract v1** devices (which advertise only the minimal PCM capability in `PCM_INFO`), the endpoints are
fixed-format:

Render (output, stream 0):

* 48,000 Hz
* Stereo (2 channels)
* 16-bit PCM little-endian (S16_LE)

Capture (input, stream 1):

* 48,000 Hz
* Mono (1 channel)
* 16-bit PCM little-endian (S16_LE)

When a virtio-snd implementation advertises a **superset** of capabilities in `PCM_INFO`, the driver can expose
additional formats/rates to PortCls/WaveRT via dynamically generated pin data ranges (while still requiring and
preferring the contract-v1 baseline when present).

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

### Interrupts (INTx + optional MSI/MSI-X)

`AERO-W7-VIRTIO` v1 requires the device/driver pair to work correctly with **PCI INTx** + the virtio ISR status register (read-to-ack).
The contract also permits **MSI-X** as an optional enhancement; Windows reports both MSI and MSI-X as “message-signaled interrupts”.

The in-tree virtio-snd driver supports both interrupt delivery modes:

- The driver supports both **message-signaled interrupts** (MSI/MSI-X) and legacy **INTx**.
- The canonical `inf/aero_virtio_snd.inf` opts into MSI/MSI-X on Windows 7 (see the `Interrupt Management\\MessageSignaledInterruptProperties` keys below).
- When message interrupts are present, the driver prefers MSI/MSI-X and programs virtio MSI-X vector routing:
  - if Windows grants enough messages: **vector 0 = config**, **vectors 1..4 = queues 0..3** (`controlq`/`eventq`/`txq`/`rxq`)
  - otherwise: **all sources on vector 0**
- If MSI/MSI-X is unavailable or cannot be connected, the driver falls back to INTx and (best-effort) disables virtio MSI-X routing
  (`VIRTIO_PCI_MSI_NO_VECTOR` / `0xFFFF`). In INTx mode (MSI-X disabled at the PCI layer), the device delivers interrupts via INTx + ISR semantics.
  When MSI-X is enabled, `0xFFFF` suppresses interrupts for that source (no INTx fallback), so the driver treats MSI-X vector programming failures as fatal
  unless it can continue with INTx or polling-only mode.

If neither MSI/MSI-X nor INTx resources are available, the driver will fail `START_DEVICE` by default. If `AllowPollingOnly=1` is set under:

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly`
  - Find `<DeviceInstancePath>` via Device Manager → device → Details → “Device instance path”.

the driver may start in polling-only mode (reduced interrupt-driven behavior).

#### INF registry keys (Windows 7 MSI opt-in)

On Windows 7, MSI/MSI-X is enabled via INF `HKR` settings under:

`Interrupt Management\\MessageSignaledInterruptProperties`

As shipped in `inf/aero_virtio_snd.inf`:

```inf
[AeroVirtioSnd_Install.NT.HW]
AddReg = AeroVirtioSnd_InterruptManagement_AddReg, AeroVirtioSnd_Parameters_AddReg

[AeroVirtioSnd_InterruptManagement_AddReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
; virtio-snd needs config + 4 queues = 5 vectors; request a little extra for future growth:
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8

; Per-device bring-up toggles (defaults):
[AeroVirtioSnd_Parameters_AddReg]
HKR,Parameters,,0x00000010
HKR,Parameters,ForceNullBackend,0x00010003,0
HKR,Parameters,AllowPollingOnly,0x00010003,0
```

Notes:

- `0x00010001` = `REG_DWORD`
- `0x00010003` = `REG_DWORD` + `FLG_ADDREG_NOCLOBBER` (do not overwrite an existing value)
- `MessageNumberLimit` is a request; Windows may grant fewer messages.

#### Expected vector mapping

When MSI/MSI-X is active and Windows grants enough messages (at least `1 + numQueues`), the expected mapping is:

- **Vector/message 0:** virtio **config** interrupt (`common_cfg.msix_config`)
- **Vector/message 1..4:** queues 0..3 (`controlq`, `eventq`, `txq`, `rxq`)

If Windows grants fewer than `1 + numQueues` messages, or if the device rejects per-queue vector assignment (vector readback mismatch), the driver falls back to:

- **All sources on vector/message 0** (config + all queues)

If MSI/MSI-X connection fails, the driver falls back to INTx (if available).
If MSI-X is enabled but virtio vector programming fails (read-back `VIRTIO_PCI_MSI_NO_VECTOR`), interrupts are suppressed on Aero contract devices (no INTx fallback), so the driver treats this as fatal unless it can switch to INTx or polling-only mode.

#### Troubleshooting / verifying MSI is active

In **Device Manager** (`devmgmt.msc`) → the virtio-snd PCI device → **Properties** → **Resources**:

- **INTx** typically shows a single small IRQ number (often shared).
- **MSI/MSI-X** typically shows one or more interrupt entries with larger values (often shown in hex) and they are usually not shared.

The driver also prints an always-on `START_DEVICE` diagnostic line indicating which interrupt mode it actually selected:

- `virtiosnd: interrupt mode: MSI/MSI-X ...`
- `virtiosnd: interrupt mode: INTx`
- `virtiosnd: interrupt mode: polling-only`

You can view this output with a kernel debugger or Sysinternals **DebugView** (Capture Kernel).

You can also use `aero-virtio-selftest.exe`:

- The selftest logs to `C:\\aero-virtio-selftest.log` and emits `AERO_VIRTIO_SELFTEST|TEST|virtio-snd|...` markers on stdout/COM1.
- The selftest also emits a `virtio-snd-irq|INFO|...` line indicating which interrupt mode is active:
  - `virtio-snd-irq|INFO|mode=intx`
  - `virtio-snd-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|...` (when the driver exposes the optional `\\.\aero_virtio_snd_diag` interface)
  - `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
  - `virtio-snd-irq|INFO|mode=msi|messages=<n>` (fallback: message-signaled interrupts; does not distinguish MSI vs MSI-X)
- See `../../tests/guest-selftest/README.md`.

See also: [`docs/windows/virtio-pci-modern-interrupt-debugging.md`](../../../../docs/windows/virtio-pci-modern-interrupt-debugging.md).

### Render vs capture status

The contract defines **two** PCM streams. Contract v1 requires the following **baseline** format for each stream, but
devices may advertise additional formats/rates via `VIRTIO_SND_R_PCM_INFO`:

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
- Exposes endpoints with a contract-v1 baseline format (and optional additional formats/rates when advertised by the device):
  - Render (stream 0): 48,000 Hz, stereo (2ch), 16-bit PCM LE (S16_LE)
  - Capture (stream 1): 48,000 Hz, mono (1ch), 16-bit PCM LE (S16_LE)
- When virtio-snd `PCM_INFO` advertises additional formats/rates/channel counts, the WaveRT miniport can dynamically
  generate pin data ranges and expose extra Windows formats (while still requiring and preferring the contract-v1
  baseline).
- Uses a QPC-based clock (`KeQueryPerformanceCounter`) for position reporting (virtqueue completions are not a reliable clock in Aero).
- Uses a periodic timer/DPC “software DMA” model that:
  - for render: submits one period of PCM to a backend callback and advances play position (backpressure-aware)
  - for capture: submits one RX period buffer; RX completion advances write position and signals the WaveRT notification event

* `src/adapter.c` — PortCls adapter driver (`PcInitializeAdapterDriver` / `PcAddAdapterDevice`)
* `src/topology.c` — topology miniport (speaker + microphone jacks + channel config properties)
* `src/wavert.c` — WaveRT miniport + stream (render + capture; dynamic format table from `PCM_INFO` when available; fixed fallback)

Backend layer (WaveRT ↔ virtio-snd):

- The WaveRT miniport uses the backend interface in `include/backend.h`.
- Default backend: `src/backend_virtio.c` (submits one period of PCM to `txq` each tick).
- Fallback backend: `src/backend_null.c` (silent; used for debugging and when virtio bring-up fails).
- To force the Null backend even when virtio is available, set `ForceNullBackend` (`REG_DWORD`) = `1` under the device instance’s **Device Parameters\\Parameters** key:
  - `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend`
    - You can find `<DeviceInstancePath>` via **Device Manager → device → Details → “Device instance path”**.
    - The shipped INFs create `ForceNullBackend` with a default value of `0` (normal virtio backend).
  - When `ForceNullBackend=1`, the adapter will also tolerate virtio transport start failures (no Code 10) so PortCls/WaveRT behavior can be tested even if the virtio-snd device/emulator is unavailable.
  - With the Null backend, both render and capture endpoints remain functional from the Windows audio stack’s perspective, but record/play silence.
- Optional bring-up flag: `AllowPollingOnly` (`REG_DWORD`) under the same per-device **Device Parameters\\Parameters** key:
  - `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly`
  - Default: `0` (created by the INF)
  - When `AllowPollingOnly=1`, the driver may start even if no usable interrupt resource can be discovered/connected (neither MSI/MSI-X nor INTx).
  - In that case it relies on polling used rings (driven by the WaveRT period timer DPC) instead of ISR/DPC delivery, and disables per-queue virtqueue interrupts best-effort to reduce interrupt storm risk.
  - This is intended for early emulator/device-model bring-up and debugging; the default behavior remains interrupt-driven (prefers MSI/MSI-X when available and falls back to INTx).
  - With `AllowPollingOnly=0` (default), `START_DEVICE` normally fails if no usable interrupt resource can be connected.
  - Applies to the modern virtio-pci transport packages (`aero_virtio_snd.sys` and `virtiosnd_legacy.sys`); the legacy I/O-port bring-up package does not use this toggle.
- After changing any bring-up toggle value, reboot the guest or disable/enable the device so Windows re-runs `START_DEVICE`.

Backwards compatibility note: older installs may have these values under the per-device driver key
(`IoOpenDeviceRegistryKey(..., PLUGPLAY_REGKEY_DRIVER, ...)`) rather than under `Device Parameters`. The driver checks the device key first and
falls back to the driver key.

Virtio transport + protocol engines (AERO-W7-VIRTIO v1 modern transport):

* `include/backend.h` — WaveRT “backend” interface used by `wavert.c`
* `src/backend_virtio.c` — virtio-snd backend implementation (PortCls ↔ virtio glue)
* `src/backend_null.c` — silent backend implementation (fallback / debugging)
* `src/virtiosnd_hw.c` — virtio-snd WDM bring-up + queue ownership
  - `VirtIoSndStartHardware` / `VirtIoSndStopHardware` handle BAR0 MMIO discovery/mapping, PCI vendor capability parsing, feature negotiation, queue setup, interrupt wiring (MSI/MSI-X preferred with INTx fallback), and reset/teardown
* Canonical virtio-pci modern transport (BAR0 MMIO + PCI vendor capability parsing):
  - `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c`
  - `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
* Interrupt integration (INTx + MSI/MSI-X):
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
  - `eventq` is initialized for contract conformance and is drained best-effort. While the Aero contract v1 does not define
    any required event messages, the driver parses standard virtio-snd **jack** events (when present) and exposes the
    resulting plug/unplug state via topology jack properties (`KSPROPERTY_JACK_DESCRIPTION*`) and generates a
    `KSEVENTSETID_Jack` / `KSEVENT_JACK_INFO_CHANGE` notification so user-mode can refresh jack state without polling.
    Audio streaming remains correct even if eventq is silent or absent.
  - For diagnostics/observability, the driver maintains `Dx->EventqStats` counters and exposes them via:
    - A structured teardown log marker (only when non-zero): `AERO_VIRTIO_SND_EVENTQ|completions=...|pcm_period=...|xrun=...|...`.
    - A custom topology KS property (`IOCTL_KS_PROPERTY`) used by `aero-virtio-selftest.exe`:
      `AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=...|pcm_period=...|xrun=...|...`.
* **Interrupts:** **INTx** is required by contract v1. The driver supports MSI/MSI-X as an optional enhancement and prefers it when Windows grants message interrupts (the shipped INF opts in), with fallback to INTx when message interrupts are unavailable or MSI-X vector programming is rejected.
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
- Host/device-model checklist (what the Win7 driver enforces at runtime): [`host-integration-checklist.md`](host-integration-checklist.md)
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
The contract defines two PCM streams with a required **baseline** format. Devices may advertise
additional formats/rates via `PCM_INFO`; the driver will negotiate a supported subset when present.

The baseline streams exposed via PortCls/WaveRT are:

- Stream 0 (playback/output): 48,000 Hz, stereo (2ch), signed 16-bit little-endian (`S16_LE`)
- Stream 1 (capture/input): 48,000 Hz, mono (1ch), signed 16-bit little-endian (`S16_LE`)

When a virtio-snd implementation advertises additional `PCM_INFO` capabilities, the driver can optionally expose extra
formats/rates/channels via dynamic WaveRT format tables (non-contract).

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

Baseline fields required by the Aero contract v1 device model:

- `features = 0`
- `formats` includes `(1 << VIRTIO_SND_PCM_FMT_S16)` (device may advertise additional format bits)
- `rates` includes `(1 << VIRTIO_SND_PCM_RATE_48000)` (device may advertise additional rate bits)
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
u8  format       = selected format (baseline: `VIRTIO_SND_PCM_FMT_S16 (5)`)
u8  rate         = selected rate (baseline: `VIRTIO_SND_PCM_RATE_48000 (7)`)
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

- `period_bytes` should be a multiple of the **frame size** (`channels * bytes_per_sample`).
  - Baseline example: S16_LE stereo is `2ch * 2 bytes = 4 bytes/frame`.
- At 48kHz, a 10ms period is `480 frames * frame_bytes` (e.g. baseline: `480 * 4 = 1920 bytes`).
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

For the DebugLogs (DBG=1 / `VIRTIOSND_TRACE*` enabled) build output (`aero_virtio_snd_dbg.sys`), stage it as the canonical INF
name (`aero_virtio_snd.sys`) with:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\stage-built-sys.ps1 -Arch amd64 -Variant debuglogs
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

# DebugLogs (DBG=1 / `VIRTIOSND_TRACE*` enabled):
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -Variant debuglogs -InputDir <build-output-root>

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

For host-buildable unit tests (protocol engines, descriptor/SG building, framing, and status handling), see:

- `../tests/README.md`

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
- This driver package opts into **MSI/MSI-X** in `inf/aero_virtio_snd.inf` and prefers message interrupts when granted, but still supports **INTx** when message interrupts are unavailable/cannot be connected (contract v1 baseline). Windows may grant fewer messages than requested; the driver will fall back to “all sources on vector 0” mapping when needed.
  - If MSI-X is enabled but virtio vector programming fails (read-back `VIRTIO_PCI_MSI_NO_VECTOR`), interrupts are suppressed (no INTx fallback), so this is treated as fatal unless the driver can switch to INTx or polling-only mode.

If this README or the INF disagrees with `AERO-W7-VIRTIO`, treat the contract as authoritative.
