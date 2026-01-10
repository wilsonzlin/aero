/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#ifndef AERO_VIRTIO_TEST_OS_H_
#define AERO_VIRTIO_TEST_OS_H_

#include <stddef.h>
#include <stdint.h>

#include "virtio_os.h"

#define TEST_OS_MAX_DMA 256u

typedef struct test_dma_mapping {
    uint64_t paddr;
    void *vaddr;
    size_t size;
} test_dma_mapping_t;

typedef struct test_os_ctx {
    uint64_t next_paddr;
    test_dma_mapping_t dma[TEST_OS_MAX_DMA];
    size_t dma_count;
} test_os_ctx_t;

void test_os_ctx_init(test_os_ctx_t *ctx);
void test_os_get_ops(virtio_os_ops_t *out_ops);

void *test_os_phys_to_virt(test_os_ctx_t *ctx, uint64_t paddr);
uint64_t test_os_virt_to_phys(test_os_ctx_t *ctx, const void *vaddr);

#endif /* AERO_VIRTIO_TEST_OS_H_ */

