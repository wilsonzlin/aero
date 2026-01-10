#pragma once

/*
 * Minimal virtio core definitions for Windows 7 SP1 drivers.
 *
 * This code is intended to be clean-room / spec-based.  It implements only
 * the pieces required by the Aero virtio-net miniport driver:
 *   - virtio status bits
 *   - common feature bits used by the legacy/split-virtqueue transport
 */

// Virtio device status bits (virtio spec)
#define VIRTIO_STATUS_ACKNOWLEDGE 0x01
#define VIRTIO_STATUS_DRIVER      0x02
#define VIRTIO_STATUS_DRIVER_OK   0x04
#define VIRTIO_STATUS_FEATURES_OK 0x08
#define VIRTIO_STATUS_FAILED      0x80

// Virtqueue/ring feature bits (legacy/split virtqueues).
#define VIRTIO_RING_F_INDIRECT_DESC (1u << 28)
#define VIRTIO_RING_F_EVENT_IDX     (1u << 29)

// Common "legacy" feature bits (lower 32 bits).
#define VIRTIO_F_NOTIFY_ON_EMPTY (1u << 24)
#define VIRTIO_F_ANY_LAYOUT      (1u << 27)

