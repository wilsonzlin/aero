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
- [ ] Device does **not** require `VIRTIO_F_RING_EVENT_IDX` / packed rings.
  - The Win7 driver will **not negotiate** `EVENT_IDX` or `PACKED`.
  - Contract v1 device models should avoid offering them (they are unused here).

## Virtqueues (must exist and match exact sizes)

- [ ] Virtqueues **0..3** exist.
- [ ] Queue sizes are **exactly**:
  - [ ] `0 controlq`: `64`
  - [ ] `1 eventq`: `64`
  - [ ] `2 txq`: `256`
  - [ ] `3 rxq`: `64`
- [ ] `queue_notify_off` is valid for each queue (maps to a doorbell address within the NOTIFY region).
- [ ] (Contract/strict mode) `queue_notify_off(q) == q` for queues `0..3` (driver treats mismatches as unsupported).
- [ ] `queue_enable` readback works: after the driver programs a queue and writes `queue_enable=1`, reading `queue_enable` returns `1`.
- [ ] Notify “doorbell” accepts a **16-bit** write of the queue index (this driver uses 16-bit MMIO writes).

## Interrupt delivery

- [ ] **INTx** is implemented and functional (required).
- [ ] MSI/MSI-X is **not used** by this driver package.
- [ ] Guest must see a **line-based** interrupt resource (not MSI/MSI-X-only / “message interrupt” only).
- [ ] PCI **Interrupt Pin** register is `1` (INTA#).
- [ ] ISR status byte is **read-to-ack** (driver relies on read clearing pending bits to deassert INTx).

## virtio-snd `DEVICE_CFG` (read-only)

The driver reads the virtio-snd device config and requires:

- [ ] `jacks = 0`
- [ ] `streams = 2`
- [ ] `chmaps = 0`

## `PCM_INFO` (controlq) sanity checks

On `VIRTIO_SND_R_PCM_INFO(start_id=0, count=2)`, the driver expects **two** `virtio_snd_pcm_info` entries
**in order**: stream 0 then stream 1:

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

## TX/RX buffer framing (PCM xfer + status)

### TX (queue 2, stream 0) — stereo S16_LE

- [ ] Each TX chain begins with an 8-byte header:
  - `u32 stream_id = 0`
  - `u32 reserved = 0`
- [ ] PCM payload is **interleaved stereo S16_LE** (payload length is a multiple of **4** bytes).
- [ ] Device writes an 8-byte `virtio_snd_pcm_status` response:
  - `u32 status`
  - `u32 latency_bytes`
- [ ] Used length must include at least the 8-byte status (otherwise the driver treats it as `BAD_MSG`).

### RX (queue 3, stream 1) — mono S16_LE

- [ ] Each RX chain begins with an 8-byte header:
  - `u32 stream_id = 1`
  - `u32 reserved = 0`
- [ ] PCM payload is **mono S16_LE** (payload length is a multiple of **2** bytes).
- [ ] Device writes an 8-byte `virtio_snd_pcm_status` response:
  - `u32 status`
  - `u32 latency_bytes`
- [ ] Used length must include at least the 8-byte status (otherwise the driver treats it as `BAD_MSG`).

### Status codes

The driver consumes these `virtio_snd_pcm_status.status` values:

| Name | Value |
| --- | ---: |
| `VIRTIO_SND_S_OK` | `0` |
| `VIRTIO_SND_S_BAD_MSG` | `1` |
| `VIRTIO_SND_S_NOT_SUPP` | `2` |
| `VIRTIO_SND_S_IO_ERR` | `3` |
