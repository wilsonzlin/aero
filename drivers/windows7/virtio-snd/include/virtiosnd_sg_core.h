#pragma once

/*
 * Pure scatter/gather builder for a circular buffer backed by an MDL PFN array.
 *
 * This file is intentionally OS-agnostic so it can be unit tested in user mode
 * (CMake host tests) without requiring WDK headers.
 */

#include <stdint.h>

/* Use the Aero Windows 7 virtio common SG entry shape (virtio_sg_entry_t). */
#include "../../virtio/common/include/virtqueue_split.h"

/*
 * Windows 7 (x86/x64) uses 4KiB pages. The virtio-snd TX path only needs to
 * split/coalesce on these boundaries.
 */
#define VIRTIOSND_SG_PAGE_SHIFT 12u
#define VIRTIOSND_SG_PAGE_SIZE (1u << VIRTIOSND_SG_PAGE_SHIFT)
#define VIRTIOSND_SG_PAGE_MASK (VIRTIOSND_SG_PAGE_SIZE - 1u)

/*
 * Returns a conservative upper bound on the number of SG elements required to
 * describe the requested region. This assumes the worst case where every page
 * is physically discontiguous, and therefore may require one SG element per
 * page per logical range (wrap may split into two ranges).
 *
 * Returns 0 on invalid parameters.
 */
uint32_t virtiosnd_sg_max_elems_for_region(uint32_t mdl_byte_offset,
                                          uint32_t mdl_byte_count,
                                          uint32_t buffer_bytes,
                                          uint32_t offset_bytes,
                                          uint32_t length_bytes,
                                          virtio_bool_t wrap);

/*
 * Build an SG list for a logical region within a circular PCM buffer.
 *
 * The buffer begins at (pfn_array[0] << PAGE_SHIFT) + mdl_byte_offset and is
 * `buffer_bytes` long. The requested region is [offset_bytes,
 * offset_bytes+length_bytes) in logical buffer coordinates. If wrap == TRUE
 * and the region crosses buffer_bytes, it is split into two ranges.
 *
 * Returns VIRTIO_OK on success or a negative VIRTIO_ERR_* code on failure:
 *  - VIRTIO_ERR_INVAL: invalid parameters.
 *  - VIRTIO_ERR_NOSPC: MaxElems too small.
 *  - VIRTIO_ERR_RANGE: PFN array too small for the requested mapping.
 */
int virtiosnd_sg_build_from_pfn_array_region(const uintptr_t *pfn_array,
                                             uint32_t pfn_count,
                                             uint32_t mdl_byte_offset,
                                             uint32_t mdl_byte_count,
                                             uint32_t buffer_bytes,
                                             uint32_t offset_bytes,
                                             uint32_t length_bytes,
                                             virtio_bool_t wrap,
                                             virtio_sg_entry_t *out,
                                             uint16_t max_elems,
                                             uint16_t *out_count);
