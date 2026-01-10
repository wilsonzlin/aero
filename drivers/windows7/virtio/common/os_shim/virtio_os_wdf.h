/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Reference OS shim for KMDF drivers.
 */

#ifndef AERO_VIRTIO_OS_WDF_H_
#define AERO_VIRTIO_OS_WDF_H_

#include "../include/virtio_os.h"

typedef struct virtio_os_wdf_ctx {
    uint32_t pool_tag;
} virtio_os_wdf_ctx_t;

void virtio_os_wdf_get_ops(virtio_os_ops_t *out_ops);

#endif /* AERO_VIRTIO_OS_WDF_H_ */

