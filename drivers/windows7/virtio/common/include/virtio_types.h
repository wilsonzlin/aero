/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Aero Windows 7 virtio common library: shared types and helpers.
 *
 * This library intentionally avoids depending on WDK headers so it can be
 * reused by StorPort, NDIS, and KMDF drivers via a small OS shim layer.
 */

#ifndef AERO_VIRTIO_TYPES_H_
#define AERO_VIRTIO_TYPES_H_

#include <stddef.h>
#include <stdint.h>

/* Avoid <stdbool.h> for maximum compatibility with older MSVC/WDK setups. */
typedef uint8_t virtio_bool_t;
#define VIRTIO_TRUE ((virtio_bool_t)1u)
#define VIRTIO_FALSE ((virtio_bool_t)0u)

enum {
    VIRTIO_OK = 0,
    VIRTIO_ERR_INVAL = -1,
    VIRTIO_ERR_NOMEM = -2,
    VIRTIO_ERR_NOSPC = -3,
    VIRTIO_ERR_RANGE = -4,
    VIRTIO_ERR_IO = -5,
};

#define VIRTIO_ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define VIRTIO_MIN(a, b) ((a) < (b) ? (a) : (b))
#define VIRTIO_MAX(a, b) ((a) > (b) ? (a) : (b))

static __inline size_t virtio_align_up_size(size_t value, size_t alignment)
{
    /* Alignment must be a power of two. */
    return (value + alignment - 1u) & ~(alignment - 1u);
}

static __inline uint64_t virtio_align_up_u64(uint64_t value, uint64_t alignment)
{
    return (value + alignment - 1u) & ~(alignment - 1u);
}

#endif /* AERO_VIRTIO_TYPES_H_ */

