/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Reference OS shim for NDIS 6.20 miniport drivers (Windows 7).
 */

#ifndef AERO_VIRTIO_OS_NDIS_H_
#define AERO_VIRTIO_OS_NDIS_H_

#include "../include/virtio_os.h"

typedef struct virtio_os_ndis_ctx {
    uint32_t pool_tag;
} virtio_os_ndis_ctx_t;

void virtio_os_ndis_get_ops(virtio_os_ops_t *out_ops);

#endif /* AERO_VIRTIO_OS_NDIS_H_ */

