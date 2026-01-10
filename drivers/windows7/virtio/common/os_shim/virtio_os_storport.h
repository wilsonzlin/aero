/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Reference OS shim for StorPort miniport drivers.
 *
 * This is optional; drivers may provide their own virtio_os_ops_t.
 */

#ifndef AERO_VIRTIO_OS_STORPORT_H_
#define AERO_VIRTIO_OS_STORPORT_H_

#include "../include/virtio_os.h"

typedef struct virtio_os_storport_ctx {
    /* Pool tag used for ExAllocatePoolWithTag allocations. */
    uint32_t pool_tag;
} virtio_os_storport_ctx_t;

void virtio_os_storport_get_ops(virtio_os_ops_t *out_ops);

#endif /* AERO_VIRTIO_OS_STORPORT_H_ */

