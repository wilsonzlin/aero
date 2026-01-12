/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

/*
 * Driver-local sizing limits derived from the Aero virtio-snd contract.
 *
 * Contract v1 (§3.4.6) allows the device to reject a single PCM transfer whose
 * payload exceeds 4 MiB with VIRTIO_SND_S_BAD_MSG. The current TX/RX engines
 * treat BAD_MSG as fatal, so the driver must never submit larger payloads.
 */
#define VIRTIOSND_MAX_PCM_PAYLOAD_BYTES (4u * 1024u * 1024u) /* 4 MiB */

/*
 * Upper bound for the WaveRT cyclic buffer (DMA common buffer) allocation.
 *
 * This buffer is allocated from nonpaged contiguous (common) memory and its
 * size is influenced by user-mode buffering/latency requests via PortCls.
 * Cap the allocation to avoid unbounded memory consumption / OOM conditions.
 *
 * 2 MiB corresponds to ~10.9 seconds of 48 kHz stereo S16_LE render audio
 * (192,000 bytes/sec) and ~21.8 seconds of mono capture audio (96,000 bytes/sec),
 * which is far above typical Windows audio engine buffering needs.
 */
#define VIRTIOSND_MAX_CYCLIC_BUFFER_BYTES (2u * 1024u * 1024u) /* 2 MiB */

