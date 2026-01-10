/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#if !defined(_KERNEL_MODE)
/* These shims are intended for Windows kernel-mode drivers only. */
#error virtio_os_storport.c requires _KERNEL_MODE
#endif

#include <ntddk.h>
#include <stdarg.h>

#include "virtio_os_storport.h"

static void *stor_alloc(void *ctx, size_t size, virtio_os_alloc_flags_t flags)
{
    virtio_os_storport_ctx_t *c;
    POOL_TYPE pool_type;
    void *p;

    c = (virtio_os_storport_ctx_t *)ctx;
    pool_type = (flags & VIRTIO_OS_ALLOC_PAGED) ? PagedPool : NonPagedPool;
    p = ExAllocatePoolWithTag(pool_type, size, c ? c->pool_tag : 'oiV ');
    if (p != NULL && (flags & VIRTIO_OS_ALLOC_ZERO) != 0) {
        RtlZeroMemory(p, size);
    }
    return p;
}

static void stor_free(void *ctx, void *ptr)
{
    (void)ctx;
    if (ptr != NULL) {
        ExFreePool(ptr);
    }
}

static virtio_bool_t stor_alloc_dma(void *ctx, size_t size, size_t alignment, virtio_dma_buffer_t *out)
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
    if (((uintptr_t)vaddr & (alignment - 1u)) != 0 || ((uint64_t)pa.QuadPart & ((uint64_t)alignment - 1u)) != 0) {
        MmFreeContiguousMemorySpecifyCache(vaddr, size, MmCached);
        return VIRTIO_FALSE;
    }

    out->vaddr = vaddr;
    out->paddr = (uint64_t)pa.QuadPart;
    out->size = size;
    return VIRTIO_TRUE;
}

static void stor_free_dma(void *ctx, virtio_dma_buffer_t *buf)
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

static uint64_t stor_virt_to_phys(void *ctx, const void *vaddr)
{
    PHYSICAL_ADDRESS pa;
    (void)ctx;
    pa = MmGetPhysicalAddress((PVOID)vaddr);
    return (uint64_t)pa.QuadPart;
}

static void stor_mb(void *ctx)
{
    (void)ctx;
    KeMemoryBarrier();
}

static void *stor_spinlock_create(void *ctx)
{
    virtio_os_storport_ctx_t *c;
    KSPIN_LOCK *lock;

    c = (virtio_os_storport_ctx_t *)ctx;
    lock = (KSPIN_LOCK *)ExAllocatePoolWithTag(NonPagedPool, sizeof(KSPIN_LOCK), c ? c->pool_tag : 'oiV ');
    if (lock != NULL) {
        KeInitializeSpinLock(lock);
    }
    return lock;
}

static void stor_spinlock_destroy(void *ctx, void *lock)
{
    (void)ctx;
    if (lock != NULL) {
        ExFreePool(lock);
    }
}

static void stor_spinlock_acquire(void *ctx, void *lock, virtio_spinlock_state_t *state)
{
    KIRQL old_irql;
    (void)ctx;
    KeAcquireSpinLock((KSPIN_LOCK *)lock, &old_irql);
    if (state != NULL) {
        *state = (virtio_spinlock_state_t)old_irql;
    }
}

static void stor_spinlock_release(void *ctx, void *lock, virtio_spinlock_state_t state)
{
    (void)ctx;
    KeReleaseSpinLock((KSPIN_LOCK *)lock, (KIRQL)state);
}

static uint8_t stor_read_io8(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_UCHAR((PUCHAR)(base + offset));
}

static uint16_t stor_read_io16(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_USHORT((PUSHORT)(base + offset));
}

static uint32_t stor_read_io32(void *ctx, uintptr_t base, uint32_t offset)
{
    (void)ctx;
    return READ_PORT_ULONG((PULONG)(base + offset));
}

static void stor_write_io8(void *ctx, uintptr_t base, uint32_t offset, uint8_t value)
{
    (void)ctx;
    WRITE_PORT_UCHAR((PUCHAR)(base + offset), value);
}

static void stor_write_io16(void *ctx, uintptr_t base, uint32_t offset, uint16_t value)
{
    (void)ctx;
    WRITE_PORT_USHORT((PUSHORT)(base + offset), value);
}

static void stor_write_io32(void *ctx, uintptr_t base, uint32_t offset, uint32_t value)
{
    (void)ctx;
    WRITE_PORT_ULONG((PULONG)(base + offset), value);
}

static void stor_log(void *ctx, const char *fmt, ...)
{
    va_list args;
    (void)ctx;
    va_start(args, fmt);
    vDbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, fmt, args);
    va_end(args);
}

void virtio_os_storport_get_ops(virtio_os_ops_t *out_ops)
{
    if (out_ops == NULL) {
        return;
    }

    RtlZeroMemory(out_ops, sizeof(*out_ops));
    out_ops->alloc = stor_alloc;
    out_ops->free = stor_free;
    out_ops->alloc_dma = stor_alloc_dma;
    out_ops->free_dma = stor_free_dma;
    out_ops->virt_to_phys = stor_virt_to_phys;
    out_ops->log = stor_log;
    out_ops->mb = stor_mb;
    out_ops->rmb = stor_mb;
    out_ops->wmb = stor_mb;
    out_ops->spinlock_create = stor_spinlock_create;
    out_ops->spinlock_destroy = stor_spinlock_destroy;
    out_ops->spinlock_acquire = stor_spinlock_acquire;
    out_ops->spinlock_release = stor_spinlock_release;
    out_ops->read_io8 = stor_read_io8;
    out_ops->read_io16 = stor_read_io16;
    out_ops->read_io32 = stor_read_io32;
    out_ops->write_io8 = stor_write_io8;
    out_ops->write_io16 = stor_write_io16;
    out_ops->write_io32 = stor_write_io32;
}
