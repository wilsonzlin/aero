<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Host integration checklist — Aero virtio-snd contract v1 (Windows 7)

This is a **host/device-model** checklist for the in-tree Windows 7 `aero_virtio_snd.sys` driver.
It summarizes the driver’s **runtime-enforced** expectations for `AERO-W7-VIRTIO` **v1**.

If any required item is missing/mismatched, the driver will typically fail `START_DEVICE` (Device Manager **Code 10**) or reject audio streaming (for example TX/RX `BAD_MSG` handling or `PCM_INFO` sanity-check failure).

## PCI identity (contract gate)

- [ ] **Vendor ID:** `VEN_1AF4` (`0x1AF4`)
- [ ] **Device ID:** `DEV_1059` (`0x1059`)
- [ ] **Revision ID:** `REV_01` (`0x01`)
- [ ] **Subsystem vendor ID:** `0x1AF4` (virtio) *(required by the driver’s modern transport in strict/contract mode)*
- [ ] *(Optional INF tightening)* **Subsystem ID:** `SUBSYS_00191AF4`

## BAR0 + virtio-pci modern capability layout

- [ ] **BAR0** is a **64-bit MMIO** BAR (memory BAR, not I/O port).
- [ ] **BAR0 size** is at least `0x4000` bytes (contract/strict mode).
- [ ] (Contract/strict mode) BAR0 base address in PCI config space matches the BAR0 MMIO resource Windows assigns/maps (no “BAR0 address mismatch”).
- [ ] PCI config space exposes a valid **PCI capability list** (Status bit 4 set, aligned pointers, no loops).
- [ ] The capability list includes virtio **vendor-specific** caps (PCI cap ID `0x09`) for:
  - `COMMON_CFG` (`cfg_type=1`, `cap_len>=16`)
  - `NOTIFY_CFG` (`cfg_type=2`, `cap_len>=20`)
  - `ISR_CFG` (`cfg_type=3`, `cap_len>=16`)
  - `DEVICE_CFG` (`cfg_type=4`, `cap_len>=16`)
- [ ] All four capabilities reference **BAR0** (`bar=0`) and have **minimum lengths**:
  - [ ] `COMMON_CFG.length >= 0x0100`
  - [ ] `NOTIFY_CFG.length >= 0x0100`
  - [ ] `ISR_CFG.length >= 0x0020`
  - [ ] `DEVICE_CFG.length >= 0x0100`
- [ ] All four capability `(offset + length)` ranges fit within BAR0 (driver rejects regions that run past BAR0).
- [ ] (Contract/strict mode) The capabilities use the fixed BAR0 offsets:
  - [ ] `COMMON_CFG.offset == 0x0000`
  - [ ] `NOTIFY_CFG.offset == 0x1000`
  - [ ] `ISR_CFG.offset == 0x2000`
  - [ ] `DEVICE_CFG.offset == 0x3000`
- [ ] `NOTIFY_CFG.notify_off_multiplier == 4` (driver rejects other values).

## Virtio feature bits (negotiation)

- [ ] Device offers `VIRTIO_F_VERSION_1` (**bit 32**).
- [ ] Device offers `VIRTIO_F_RING_INDIRECT_DESC` (**bit 28**).
- [ ] Device implements **64-bit** feature negotiation via `*_feature_select` (`0` = low 32 bits, `1` = high 32 bits).
- [ ] Feature negotiation status handshake works:
  - After the driver writes `device_status |= FEATURES_OK`, the device must keep `FEATURES_OK` set (otherwise the driver fails initialization).
- [ ] Device does **not** require `VIRTIO_F_RING_EVENT_IDX` / packed rings.
  - The Win7 driver will **not negotiate** `EVENT_IDX` or `PACKED`.
  - The device must operate correctly without them (the driver never enables them).
  - `EVENT_IDX` is **bit 29**; `PACKED` is **bit 34**.

## Virtqueues (must exist and match exact sizes)

- [ ] Virtqueues **0..3** exist.
- [ ] `COMMON_CFG.num_queues >= 4`.
- [ ] Each queue’s `queue_size` is a power-of-two (driver validates this).
- [ ] Queue sizes are **exactly**:
  - [ ] `0 controlq`: `64`
  - [ ] `1 eventq`: `64`
  - [ ] `2 txq`: `256`
  - [ ] `3 rxq`: `64`
- [ ] `queue_notify_off` is valid for each queue (maps to a doorbell address within the NOTIFY region).
- [ ] (Contract/strict mode) `queue_notify_off(q) == q` for queues `0..3` (driver treats mismatches as unsupported).
- [ ] `queue_notify_off` is stable (driver reads it before and after enabling the queue and expects the same value).
- [ ] `queue_enable` readback works: after the driver programs a queue and writes `queue_enable=1`, reading `queue_enable` returns `1`.
- [ ] Notify “doorbell” accepts a **16-bit** write of the queue index (this driver uses 16-bit MMIO writes).
- [ ] Device correctly supports **indirect descriptors** (`VRING_DESC_F_INDIRECT`) on all queues (the Win7 driver uses indirect descriptors for submissions).

## Interrupt delivery

The driver needs **at least one usable interrupt mechanism** (unless you explicitly opt into bring-up mode with `AllowPollingOnly=1` under the device instance registry key):

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly`
  - Find `<DeviceInstancePath>` via Device Manager → Details → “Device instance path”.
`AERO-W7-VIRTIO` v1 still requires **INTx** compatibility as a baseline, but the driver also supports (and prefers) **MSI/MSI-X** when Windows provides message interrupts.

If you want to exercise PortCls/WaveRT behavior even when virtio transport bring-up is failing (debug/bring-up only), you can force the silent null backend with:

- `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend`
  - Find `<DeviceInstancePath>` via Device Manager → Details → “Device instance path”.

- [ ] At least one of the following is present and functional:
  - [ ] **MSI/MSI-X** message interrupts (`CM_RESOURCE_INTERRUPT_MESSAGE`)
  - [ ] **INTx** line-based interrupts

To quickly confirm what Windows (and the driver) selected at runtime:

- The driver prints an always-on `START_DEVICE` diagnostic line indicating its chosen interrupt mode:
  - `virtiosnd: interrupt mode: MSI/MSI-X ...`
  - `virtiosnd: interrupt mode: INTx`
  - `virtiosnd: interrupt mode: polling-only`
- `aero-virtio-selftest.exe` emits a `virtio-snd-irq|INFO|...` line indicating the observed interrupt mode:
  - `virtio-snd-irq|INFO|mode=intx`
  - `virtio-snd-irq|INFO|mode=msix|messages=<n>|msix_config_vector=0x....|...` (when the driver exposes the optional `\\.\aero_virtio_snd_diag` interface)
    - Includes per-queue MSI-X routing (`msix_queue0_vector..msix_queue3_vector`) and diagnostic counters
      (`interrupt_count`, `dpc_count`, `drain0..drain3`).
  - `virtio-snd-irq|INFO|mode=none|...` (polling-only; no interrupt objects are connected)
  - `virtio-snd-irq|INFO|mode=msi|messages=<n>` (fallback: message interrupts; does not distinguish MSI vs MSI-X)

### MSI/MSI-X (message-signaled interrupts)

- [ ] Device exposes PCI MSI/MSI-X so Windows can allocate message interrupts (the canonical INF sets `Interrupt Management\\MessageSignaledInterruptProperties\\MSISupported=1`).
- [ ] When message interrupts are used, virtio MSI-X vector routing works correctly:
  - [ ] `common_cfg.msix_config` (config vector) is writable and reads back the value the driver programs.
  - [ ] `common_cfg.queue_msix_vector` (queue vectors) is writable and reads back the value the driver programs.
  - [ ] The device delivers interrupts on the programmed vector indices.
- [ ] Expected vector programming from the driver:
  - [ ] `msix_config = 0` (config on message 0)
  - [ ] If Windows grants at least `1 + 4` messages, queues 0..3 are programmed as `queue_msix_vector = 1..4`.
  - [ ] Otherwise (or if per-queue assignment is rejected), the driver falls back to `queue_msix_vector = 0` for all queues (all sources on message 0).

### INTx (line interrupt, contract v1 baseline)

- [ ] PCI **Interrupt Pin** register is `1` (INTA#).
- [ ] ISR status byte is **read-to-ack** (driver relies on read clearing pending bits to deassert INTx).
- [ ] ISR status bits follow the virtio ISR definition (required for the Win7 INTx ISR/DPC dispatch):
  - [ ] Bit 0 (`QUEUE_INTERRUPT`) is set when the device has published used-ring entries for any queue.
  - [ ] Bit 1 (`CONFIG_INTERRUPT`) is set only for device-specific config change notifications.
  - [ ] Bits 2–7 are `0`.
- [ ] **INTx** is implemented and functional (baseline requirement for `AERO-W7-VIRTIO` v1; also used as fallback if MSI/MSI-X is unavailable or cannot be connected).
- [ ] In **INTx mode** (PCI MSI-X disabled), the driver programs all virtio MSI-X selectors (`msix_config`, `queue_msix_vector`) to `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) and the device delivers interrupts via INTx + ISR semantics.
  - When MSI-X is enabled at the PCI layer, `0xFFFF` suppresses interrupts for that source (no INTx fallback), so vectors must be programmed to valid indices for MSI/MSI-X mode.

## virtio-snd `DEVICE_CFG` (read-only)

The driver reads the virtio-snd device config and requires:

- [ ] `jacks = 0` (**preferred**) or `jacks = 2` (**tolerated**, matches the driver’s fixed two-jack topology and enables virtio-snd JACK eventq notifications)
- [ ] `streams = 2`
- [ ] `chmaps = 0`

## `PCM_INFO` (controlq) sanity checks

On `VIRTIO_SND_R_PCM_INFO(start_id=0, count=2)`, the driver expects **two** `virtio_snd_pcm_info` entries
**in order**: stream 0 then stream 1:

- [ ] Controlq response framing:
  - [ ] Response begins with `u32 status` and completes with `used.len >= 4`.
  - [ ] For `PCM_INFO`, response completes with `used.len >= 4 + 2*32 = 68` (status + 2 entries).

- [ ] **Stream 0 (render / playback)**:
  - [ ] `direction = VIRTIO_SND_D_OUTPUT`
  - [ ] channels allow **2ch** (`channels_min <= 2 <= channels_max`)
  - [ ] formats include **S16** (`VIRTIO_SND_PCM_FMT_S16`)
  - [ ] rates include **48,000 Hz** (`VIRTIO_SND_PCM_RATE_48000`)
- [ ] **Stream 1 (capture)**:
  - [ ] `direction = VIRTIO_SND_D_INPUT`
  - [ ] channels allow **1ch** (`channels_min <= 1 <= channels_max`)
  - [ ] formats include **S16** (`VIRTIO_SND_PCM_FMT_S16`)
  - [ ] rates include **48,000 Hz** (`VIRTIO_SND_PCM_RATE_48000`)

## Controlq commands used by the Win7 driver (required subset)

- [ ] Device supports these request codes for **both** streams 0 and 1:
  - `VIRTIO_SND_R_PCM_INFO` (`0x0100`)
  - `VIRTIO_SND_R_PCM_SET_PARAMS` (`0x0101`)
  - `VIRTIO_SND_R_PCM_PREPARE` (`0x0102`)
  - `VIRTIO_SND_R_PCM_RELEASE` (`0x0103`)
  - `VIRTIO_SND_R_PCM_START` (`0x0104`)
  - `VIRTIO_SND_R_PCM_STOP` (`0x0105`)
- [ ] Contract v1 `PCM_SET_PARAMS` parameters expected by the driver:
  - Stream 0 (output): `channels=2`, `format=S16`, `rate=48000`, `features=0`
  - Stream 1 (input): `channels=1`, `format=S16`, `rate=48000`, `features=0`
  - (Optional/non-contract) If the device advertises additional formats/rates/channel counts in `PCM_INFO`, the Win7
    driver may send those values in `PCM_SET_PARAMS` when Windows selects them. If you advertise extra capabilities,
    ensure you actually implement them.

## TX/RX buffer framing (PCM xfer + status)

### TX (queue 2, stream 0) — stereo S16_LE

- [ ] Each TX chain begins with an 8-byte header:
  - `u32 stream_id = 0`
  - `u32 reserved = 0`
- [ ] The header and PCM payload may be split across multiple **device-readable** descriptors; the device must treat them as a single concatenated byte stream.
- [ ] PCM payload format matches the negotiated `PCM_SET_PARAMS` tuple.
  - Contract v1: **interleaved stereo S16_LE** (payload length is a multiple of **4** bytes).
  - Optional/non-contract: if you advertise extra `PCM_INFO` caps and accept non-baseline `PCM_SET_PARAMS`, payload length is a multiple of the negotiated **frame size** (`channels * bytes_per_sample`).
    - Note: virtio-snd format codes follow ALSA `snd_pcm_format_t`. In ALSA, `S24`/`U24` are 24-bit samples stored in a 32-bit container (`bytes_per_sample = 4`), not packed 3-byte samples.
- [ ] **Safety cap (contract v1):** PCM **payload** is **≤ 256 KiB** (`262,144` bytes), where payload bytes exclude:
  - the 8-byte `virtio_snd_pcm_xfer` header, and
  - the final 8-byte `virtio_snd_pcm_status` response descriptor.
- [ ] Device writes an 8-byte `virtio_snd_pcm_status` response:
  - `u32 status`
  - `u32 latency_bytes`
- [ ] Used length (`used.len`) must include at least the 8-byte status (otherwise the driver treats it as `BAD_MSG`).
  - `BAD_MSG` / `NOT_SUPP` completions are treated as **fatal** by the Win7 driver (streaming stops).

### RX (queue 3, stream 1) — mono S16_LE

- [ ] Each RX chain begins with an 8-byte header:
  - `u32 stream_id = 1`
  - `u32 reserved = 0`
- [ ] The PCM payload may be split across multiple **device-writable** descriptors; the device must write sequential PCM bytes across them.
- [ ] PCM payload format matches the negotiated `PCM_SET_PARAMS` tuple.
  - Contract v1: **mono S16_LE** (payload length is a multiple of **2** bytes).
  - Optional/non-contract: if you advertise extra `PCM_INFO` caps and accept non-baseline `PCM_SET_PARAMS`, payload length is a multiple of the negotiated **frame size** (`channels * bytes_per_sample`).
    - Note: virtio-snd format codes follow ALSA `snd_pcm_format_t`. In ALSA, `S24`/`U24` are 24-bit samples stored in a 32-bit container (`bytes_per_sample = 4`).
- [ ] **Safety cap (contract v1):** PCM **payload** is **≤ 256 KiB** (`262,144` bytes), where payload bytes exclude:
  - the 8-byte `virtio_snd_pcm_xfer` header, and
  - the final 8-byte `virtio_snd_pcm_status` response descriptor.
- [ ] Device writes an 8-byte `virtio_snd_pcm_status` response:
  - `u32 status`
  - `u32 latency_bytes`
- [ ] Used length (`used.len`) must include at least the 8-byte status (otherwise the driver treats it as `BAD_MSG`).
  - `BAD_MSG` / `NOT_SUPP` completions are treated as **fatal** by the Win7 driver (capture stops).

### Status codes

The driver consumes these `virtio_snd_pcm_status.status` values:

| Name | Value |
| --- | ---: |
| `VIRTIO_SND_S_OK` | `0` |
| `VIRTIO_SND_S_BAD_MSG` | `1` |
| `VIRTIO_SND_S_NOT_SUPP` | `2` |
| `VIRTIO_SND_S_IO_ERR` | `3` |
