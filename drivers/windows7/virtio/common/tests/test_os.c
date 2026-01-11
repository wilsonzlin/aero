/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "test_os.h"

#include <assert.h>
#include <stdlib.h>
#include <string.h>

#include "fake_pci_device.h"
#include "fake_pci_device_modern.h"

#if defined(_WIN32)
#include <windows.h>
#endif
#if defined(_MSC_VER)
#include <malloc.h>
#endif

static void *test_aligned_malloc(size_t alignment, size_t size)
{
    void *p;

    if (alignment < sizeof(void *)) {
        alignment = sizeof(void *);
    }
    /* alignment must be power-of-two for posix_memalign. */
    if ((alignment & (alignment - 1u)) != 0) {
        return NULL;
    }

#if defined(_MSC_VER)
    p = _aligned_malloc(size, alignment);
    return p;
#else
    p = NULL;
    if (posix_memalign(&p, alignment, size) != 0) {
        return NULL;
    }
    return p;
#endif
}

static void test_aligned_free(void *p)
{
#if defined(_MSC_VER)
    _aligned_free(p);
#else
    free(p);
#endif
}

void test_os_ctx_init(test_os_ctx_t *ctx)
{
    if (ctx == NULL) {
        return;
    }
    memset(ctx, 0, sizeof(*ctx));
    ctx->next_paddr = 0x100000; /* 1 MiB */
}

void *test_os_phys_to_virt(test_os_ctx_t *ctx, uint64_t paddr)
{
    size_t i;

    if (ctx == NULL) {
        return NULL;
    }

    for (i = 0; i < ctx->dma_count; i++) {
        uint64_t base;
        uint64_t end;

        base = ctx->dma[i].paddr;
        end = base + (uint64_t)ctx->dma[i].size;
        if (paddr >= base && paddr < end) {
            uintptr_t off;
            off = (uintptr_t)(paddr - base);
            return (void *)((uint8_t *)ctx->dma[i].vaddr + off);
        }
    }

    return NULL;
}

uint64_t test_os_virt_to_phys(test_os_ctx_t *ctx, const void *vaddr)
{
    size_t i;
    const uint8_t *p;

    if (ctx == NULL || vaddr == NULL) {
        return 0;
    }

    p = (const uint8_t *)vaddr;
    for (i = 0; i < ctx->dma_count; i++) {
        const uint8_t *base;
        const uint8_t *end;

        base = (const uint8_t *)ctx->dma[i].vaddr;
        end = base + ctx->dma[i].size;
        if (p >= base && p < end) {
            return ctx->dma[i].paddr + (uint64_t)(p - base);
        }
    }

    return 0;
}

static void *test_alloc(void *ctx, size_t size, virtio_os_alloc_flags_t flags)
{
    (void)ctx;
    if ((flags & VIRTIO_OS_ALLOC_ZERO) != 0) {
        return calloc(1, size);
    }
    return malloc(size);
}

static void test_free(void *ctx, void *ptr)
{
    (void)ctx;
    free(ptr);
}

static virtio_bool_t test_alloc_dma(void *ctx, size_t size, size_t alignment, virtio_dma_buffer_t *out)
{
    test_os_ctx_t *c;
    uint64_t paddr;
    void *vaddr;

    c = (test_os_ctx_t *)ctx;
    if (c == NULL || out == NULL || size == 0 || alignment == 0) {
        return VIRTIO_FALSE;
    }
    if (c->dma_count >= TEST_OS_MAX_DMA) {
        return VIRTIO_FALSE;
    }

    paddr = virtio_align_up_u64(c->next_paddr, (uint64_t)alignment);
    c->next_paddr = paddr + virtio_align_up_u64((uint64_t)size, (uint64_t)alignment);

    vaddr = test_aligned_malloc(alignment, size);
    if (vaddr == NULL) {
        return VIRTIO_FALSE;
    }
    memset(vaddr, 0, size);

    c->dma[c->dma_count].paddr = paddr;
    c->dma[c->dma_count].vaddr = vaddr;
    c->dma[c->dma_count].size = size;
    c->dma_count++;

    out->vaddr = vaddr;
    out->paddr = paddr;
    out->size = size;
    return VIRTIO_TRUE;
}

static void test_free_dma(void *ctx, virtio_dma_buffer_t *buf)
{
    test_os_ctx_t *c;
    size_t i;

    c = (test_os_ctx_t *)ctx;
    if (c == NULL || buf == NULL || buf->vaddr == NULL) {
        return;
    }

    for (i = 0; i < c->dma_count; i++) {
        if (c->dma[i].vaddr == buf->vaddr) {
            test_aligned_free(c->dma[i].vaddr);

            /* Remove mapping by swapping with last. */
            c->dma[i] = c->dma[c->dma_count - 1u];
            c->dma_count--;
            break;
        }
    }

    buf->vaddr = NULL;
    buf->paddr = 0;
    buf->size = 0;
}

static void test_mb(void *ctx)
{
    (void)ctx;
#if defined(__clang__) || defined(__GNUC__)
    __sync_synchronize();
#elif defined(_WIN32)
    /* Best-effort: MSVC user-mode barrier. */
    MemoryBarrier();
#else
    /* Fallback: nothing. */
#endif
}

static uint64_t test_virt_to_phys(void *ctx, const void *vaddr)
{
    return test_os_virt_to_phys((test_os_ctx_t *)ctx, vaddr);
}

static uint8_t test_read_io8(void *ctx, uintptr_t base, uint32_t offset)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return 0;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        return fake_pci_read8((fake_pci_device_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_PCI_CFG:
        return fake_pci_modern_cfg_read8((fake_pci_device_modern_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        return fake_pci_modern_mmio_read8((fake_pci_device_modern_t *)r->dev, offset);
    default:
        return 0;
    }
}

static uint16_t test_read_io16(void *ctx, uintptr_t base, uint32_t offset)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return 0;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        return fake_pci_read16((fake_pci_device_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_PCI_CFG:
        return fake_pci_modern_cfg_read16((fake_pci_device_modern_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        return fake_pci_modern_mmio_read16((fake_pci_device_modern_t *)r->dev, offset);
    default:
        return 0;
    }
}

static uint32_t test_read_io32(void *ctx, uintptr_t base, uint32_t offset)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return 0;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        return fake_pci_read32((fake_pci_device_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_PCI_CFG:
        return fake_pci_modern_cfg_read32((fake_pci_device_modern_t *)r->dev, offset);
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        return fake_pci_modern_mmio_read32((fake_pci_device_modern_t *)r->dev, offset);
    default:
        return 0;
    }
}

static void test_write_io8(void *ctx, uintptr_t base, uint32_t offset, uint8_t value)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        fake_pci_write8((fake_pci_device_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_PCI_CFG:
        fake_pci_modern_cfg_write8((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        fake_pci_modern_mmio_write8((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    default:
        break;
    }
}

static void test_write_io16(void *ctx, uintptr_t base, uint32_t offset, uint16_t value)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        fake_pci_write16((fake_pci_device_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_PCI_CFG:
        fake_pci_modern_cfg_write16((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        fake_pci_modern_mmio_write16((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    default:
        break;
    }
}

static void test_write_io32(void *ctx, uintptr_t base, uint32_t offset, uint32_t value)
{
    const test_io_region_t *r;
    (void)ctx;
    r = (const test_io_region_t *)base;
    if (r == NULL) {
        return;
    }

    switch (r->kind) {
    case TEST_IO_REGION_LEGACY_PIO:
        fake_pci_write32((fake_pci_device_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_PCI_CFG:
        fake_pci_modern_cfg_write32((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    case TEST_IO_REGION_MODERN_BAR0_MMIO:
        fake_pci_modern_mmio_write32((fake_pci_device_modern_t *)r->dev, offset, value);
        break;
    default:
        break;
    }
}

void test_os_get_ops(virtio_os_ops_t *out_ops)
{
    assert(out_ops != NULL);
    memset(out_ops, 0, sizeof(*out_ops));

    out_ops->alloc = test_alloc;
    out_ops->free = test_free;
    out_ops->alloc_dma = test_alloc_dma;
    out_ops->free_dma = test_free_dma;
    out_ops->virt_to_phys = test_virt_to_phys;
    out_ops->mb = test_mb;
    out_ops->rmb = test_mb;
    out_ops->wmb = test_mb;

    out_ops->read_io8 = test_read_io8;
    out_ops->read_io16 = test_read_io16;
    out_ops->read_io32 = test_read_io32;
    out_ops->write_io8 = test_write_io8;
    out_ops->write_io16 = test_write_io16;
    out_ops->write_io32 = test_write_io32;
}
