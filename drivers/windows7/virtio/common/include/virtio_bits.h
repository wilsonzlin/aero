#pragma once

/*
 * Minimal virtio core definitions for Windows 7 SP1 drivers.
 *
 * This code is intended to be clean-room / spec-based.  It implements only
 * the pieces required by the Aero virtio-net miniport driver:
 *   - virtio status bits
 *   - common feature bits used by the legacy/split-virtqueue transport
 */

// Virtio device status bits (virtio spec).
//
// These are shared with virtio-core's virtio_spec.h; guard definitions to allow
// either header to be included first.
#ifndef VIRTIO_STATUS_ACKNOWLEDGE
#define VIRTIO_STATUS_ACKNOWLEDGE 0x01
#endif
#ifndef VIRTIO_STATUS_DRIVER
#define VIRTIO_STATUS_DRIVER 0x02
#endif
#ifndef VIRTIO_STATUS_DRIVER_OK
#define VIRTIO_STATUS_DRIVER_OK 0x04
#endif
#ifndef VIRTIO_STATUS_FEATURES_OK
#define VIRTIO_STATUS_FEATURES_OK 0x08
#endif
#ifndef VIRTIO_STATUS_DEVICE_NEEDS_RESET
#define VIRTIO_STATUS_DEVICE_NEEDS_RESET 0x40
#endif
#ifndef VIRTIO_STATUS_FAILED
#define VIRTIO_STATUS_FAILED 0x80
#endif

// Virtqueue/ring feature bits (split virtqueues).
#ifndef VIRTIO_RING_F_INDIRECT_DESC
#define VIRTIO_RING_F_INDIRECT_DESC (1u << 28)
#endif
#ifndef VIRTIO_RING_F_EVENT_IDX
#define VIRTIO_RING_F_EVENT_IDX (1u << 29)
#endif

// Common "legacy" feature bits (lower 32 bits).
#define VIRTIO_F_NOTIFY_ON_EMPTY (1u << 24)
#define VIRTIO_F_ANY_LAYOUT      (1u << 27)
