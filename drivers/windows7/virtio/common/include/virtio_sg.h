/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Generic virtio scatter/gather entry description.
 *
 * This is shared by:
 *  - the legacy/transitional split virtqueue implementation
 *    (`virtqueue_split_legacy.*`), and
 *  - Windows 7 virtio-snd MDL/PFN scatter-gather helpers.
 *
 * Keep this header OS-agnostic so it can be reused by host-side unit tests and
 * by drivers that avoid directly depending on WDK headers.
 */

#ifndef AERO_VIRTIO_SG_H_
#define AERO_VIRTIO_SG_H_

#include "virtio_types.h"

typedef struct virtio_sg_entry {
    uint64_t addr;
    uint32_t len;
    virtio_bool_t device_writes; /* set VRING_DESC_F_WRITE */
} virtio_sg_entry_t;

#endif /* AERO_VIRTIO_SG_H_ */
