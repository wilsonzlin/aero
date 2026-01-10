/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#if !defined(_KERNEL_MODE)
#error virtio_os_wdf.c requires _KERNEL_MODE
#endif

#include <ntddk.h>
#include <stdarg.h>

#include "virtio_os_wdf.h"

static void *wdf_alloc(void *ctx, size_t size, virtio_os_alloc_flags_t flags)
{
    virtio_os_wdf_ctx_t *c;
    POOL_TYPE pool_type;
    void *p;

    c = (virtio_os_wdf_ctx_t *)ctx;
    pool_type = (flags & VIRTIO_OS_ALLOC_PAGED) ? PagedPool : NonPagedPool;
    p = ExAllocatePoolWithTag(pool_type, size, c ? c->pool_tag : 'oiV ');
    if (p != NULL && (flags & VIRTIO_OS_ALLOC_ZERO) != 0) {
        RtlZeroMemory(p, size);
    }
    return p;
}

static void wdf_free(void *ctx, void *ptr)
{
    (void)ctx;
    if (ptr != NULL) {
        ExFreePool(ptr);
    }
}

static virtio_bool_t wdf_alloc_dma(void *ctx, size_t size, size_t alignment, virtio_dma_buffer_t *out)
{
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;
    void *vaddr;
    PHYSICAL_ADDRESS pa;

    (void)ctx;

    if (out == NULL || size == 0) {
        return VIRTIO_FALSE;
    }

    low.QuadPart = 0;
    high.QuadPart = -1;
    boundary.QuadPart = 0;

    vaddr = MmAllocateContiguousMemorySpecifyCache(size, low, high, boundary, MmCached);
    if (vaddr == NULL) {
        return VIRTIO_FALSE;
    }

    pa = MmGetPhysicalAddress(vaddr);
    if (alignment != 0 &&
        (((uintptr_t)vaddr & (alignment - 1u)) != 0 || ((uint64_t)pa.QuadPart & ((uint64_t)alignment - 1u)) != 0)) {
        MmFreeContiguousMemorySpecifyCache(vaddr, size, MmCached);
        return VIRTIO_FALSE;
    }

    out->vaddr = vaddr;
    out->paddr = (uint64_t)pa.QuadPart;
    out->size = size;
    return VIRTIO_TRUE;
}

static void wdf_free_dma(void *ctx, virtio_dma_buffer_t *buf)
{
    (void)ctx;
    if (buf == NULL || buf->vaddr == NULL || buf->size == 0) {
        return;
    }

    MmFreeContiguousMemorySpecifyCache(buf->vaddr, buf->size, MmCached);
    buf->vaddr = NULL;
    buf->paddr = 0;
    buf->size = 0;
}

static uint64_t wdf_virt_to_phys(void *ctx, const void *vaddr)
{
    PHYSICAL_ADDRESS pa;
    (void)ctx;
    pa = MmGetPhysicalAddress((PVOID)vaddr);
    return (uint64_t)pa.QuadPart;
}

static void wdf_mb(void *ctx)
{
    (void)ctx;
    KeMemoryBarrier();
}

static void *wdf_spinlock_create(void *ctx)
{
    virtio_os_wdf_ctx_t *c;
    KSPIN_LOCK *lock;

    c = (virtio_os_wdf_ctx_t *)ctx;
    lock = (KSPIN_LOCK *)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSPIN_LOCK), c ? c->pool_tag : 'oiV ');
    if (lock != NULL) {
        KeInitializeSpinLock(lock);
    }
    return lock;
}

static void wdf_spinlock_destroy(void *ctx, void *lock)
{
    (void)ctx;
    if (lock != NULL) {
        ExFreePool(lock);
    }
}

static void wdf_spinlock_acquire(void *ctx, void *lock, virtio_spinlock_state_t *state)
{
    KIRQL old_irql;
    (void)ctx;
    KeAcquireSpinLock((KSPIN_LOCK *)lock, &old_irql);
    if (state != NULL) {
        *state = (virtio_spinlock_state_t)old_irql;
    }
}

static void wdf_spinlock_release(void *ctx, void *lock, virtio_spinlock_state_t state)
{
    (void)ctx;
    KeReleaseSpinLock((KSPIN_LOCK *)lock, (KIRQL)state);
}

static uint8_t wdf_read_io8(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_UCHAR((PUCHAR)(base + offset));
}

static uint16_t wdf_read_io16(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_USHORT((PUSHORT)(base + offset));
}

static uint32_t wdf_read_io32(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_ULONG((PULONG)(base + offset));
}

static void wdf_write_io8(void *ctx, uintptr_t base, uint32_t offset, uint8_t value)
{
    (void)ctx;
    WRITE_PORT_UCHAR((PUCHAR)(base + offset), value);
}

static void wdf_write_io16(void *ctx, uintptr_t base, uint32_t offset, uint16_t value)
{
    (void)ctx;
    WRITE_PORT_USHORT((PUSHORT)(base + offset), value);
}

static void wdf_write_io32(void *ctx, uintptr_t base, uint32_t offset, uint32_t value)
{
    (void)ctx;
    WRITE_PORT_ULONG((PULONG)(base + offset), value);
}

static void wdf_log(void *ctx, const char *fmt, ...)
{
    va_list args;
    (void)ctx;
    va_start(args, fmt);
    vDbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, fmt, args);
    va_end(args);
}

void virtio_os_wdf_get_ops(virtio_os_ops_t *out_ops)
{
    if (out_ops == NULL) {
        return;
    }

    RtlZeroMemory(out_ops, sizeof(*out_ops));
    out_ops->alloc = wdf_alloc;
    out_ops->free = wdf_free;
    out_ops->alloc_dma = wdf_alloc_dma;
    out_ops->free_dma = wdf_free_dma;
    out_ops->virt_to_phys = wdf_virt_to_phys;
    out_ops->log = wdf_log;
    out_ops->mb = wdf_mb;
    out_ops->rmb = wdf_mb;
    out_ops->wmb = wdf_mb;
    out_ops->spinlock_create = wdf_spinlock_create;
    out_ops->spinlock_destroy = wdf_spinlock_destroy;
    out_ops->spinlock_acquire = wdf_spinlock_acquire;
    out_ops->spinlock_release = wdf_spinlock_release;
    out_ops->read_io8 = wdf_read_io8;
    out_ops->read_io16 = wdf_read_io16;
    out_ops->read_io32 = wdf_read_io32;
    out_ops->write_io8 = wdf_write_io8;
    out_ops->write_io16 = wdf_write_io16;
    out_ops->write_io32 = wdf_write_io32;
}
