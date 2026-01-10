/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * OS abstraction layer for the Aero Windows 7 virtio common library.
 *
 * The core virtio code must not depend on StorPort/NDIS/KMDF headers. Drivers
 * provide an implementation of these callbacks appropriate for their context.
 */

#ifndef AERO_VIRTIO_OS_H_
#define AERO_VIRTIO_OS_H_

#include "virtio_types.h"

typedef struct virtio_dma_buffer {
    void *vaddr;
    uint64_t paddr;
    size_t size;
} virtio_dma_buffer_t;

typedef enum virtio_os_alloc_flags {
    VIRTIO_OS_ALLOC_PAGED = 1u << 0,
    VIRTIO_OS_ALLOC_NONPAGED = 1u << 1,
    VIRTIO_OS_ALLOC_ZERO = 1u << 2,
} virtio_os_alloc_flags_t;

typedef uintptr_t virtio_spinlock_state_t;

typedef struct virtio_os_ops {
    /* Memory allocation for small driver-private metadata. */
    void *(*alloc)(void *ctx, size_t size, virtio_os_alloc_flags_t flags);
    void (*free)(void *ctx, void *ptr);

    /*
     * Physically contiguous, DMA-able "common buffer" allocation.
     * Required for legacy virtio-pci split virtqueue rings (queue PFN
     * register provides a single base address).
     *
     * `alignment` is a byte alignment, typically 4096 for legacy virtqueues.
     */
    virtio_bool_t (*alloc_dma)(void *ctx, size_t size, size_t alignment, virtio_dma_buffer_t *out);
    void (*free_dma)(void *ctx, virtio_dma_buffer_t *buf);

    /*
     * Optional virtual->physical translation helper.
     * Most drivers can provide physical addresses directly from their DMA APIs.
     */
    uint64_t (*virt_to_phys)(void *ctx, const void *vaddr);

    /* Logging (optional). */
    void (*log)(void *ctx, const char *fmt, ...);

    /* Memory barriers (SMP safe). */
    void (*mb)(void *ctx);
    void (*rmb)(void *ctx);
    void (*wmb)(void *ctx);

    /* Spinlocks (optional - core code does not assume they exist). */
    void *(*spinlock_create)(void *ctx);
    void (*spinlock_destroy)(void *ctx, void *lock);
    void (*spinlock_acquire)(void *ctx, void *lock, virtio_spinlock_state_t *state);
    void (*spinlock_release)(void *ctx, void *lock, virtio_spinlock_state_t state);

    /*
     * I/O register access helpers.
     *
     * For virtio-pci legacy transport these are typically port I/O
     * (READ/WRITE_PORT_* on Windows). For unit tests they may be backed by a
     * memory-mapped struct.
     */
    uint8_t (*read_io8)(void *ctx, uintptr_t base, uint32_t offset);
    uint16_t (*read_io16)(void *ctx, uintptr_t base, uint32_t offset);
    uint32_t (*read_io32)(void *ctx, uintptr_t base, uint32_t offset);
    void (*write_io8)(void *ctx, uintptr_t base, uint32_t offset, uint8_t value);
    void (*write_io16)(void *ctx, uintptr_t base, uint32_t offset, uint16_t value);
    void (*write_io32)(void *ctx, uintptr_t base, uint32_t offset, uint32_t value);
} virtio_os_ops_t;

#endif /* AERO_VIRTIO_OS_H_ */
