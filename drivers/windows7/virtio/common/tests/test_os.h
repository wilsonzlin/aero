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

typedef enum test_io_region_kind {
    TEST_IO_REGION_LEGACY_PIO = 1,
    TEST_IO_REGION_MODERN_PCI_CFG = 2,
    TEST_IO_REGION_MODERN_BAR0_MMIO = 3,
} test_io_region_kind_t;

/*
 * Opaque I/O base handle passed through virtio_os_ops_t read/write callbacks.
 *
 * Tests pass pointers to instances of this struct as `uintptr_t base`.
 */
typedef struct test_io_region {
    test_io_region_kind_t kind;
    void *dev;
} test_io_region_t;

void test_os_ctx_init(test_os_ctx_t *ctx);
void test_os_get_ops(virtio_os_ops_t *out_ops);

void *test_os_phys_to_virt(test_os_ctx_t *ctx, uint64_t paddr);
uint64_t test_os_virt_to_phys(test_os_ctx_t *ctx, const void *vaddr);

#endif /* AERO_VIRTIO_TEST_OS_H_ */
