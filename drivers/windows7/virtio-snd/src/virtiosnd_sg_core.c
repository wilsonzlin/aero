/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtiosnd_sg_core.h"

/*
 * Avoid relying on UINT64_MAX/UINT32_MAX from <stdint.h> for maximum toolchain
 * compatibility (WDK 7.1 + older MSVC). Use explicit constants instead.
 */
#define VIRTIOSND_U64_MAX ((uint64_t)~(uint64_t)0)
#define VIRTIOSND_U32_MAX ((uint32_t)0xffffffffu)

static uint32_t virtiosnd_sg_pages_spanned(uint32_t mdl_byte_offset, uint32_t start_offset, uint32_t length)
{
    uint64_t start;
    uint64_t end;
    uint64_t first;
    uint64_t last;

    /* length must be > 0 for the (end - 1) calculation to be valid. */
    if (length == 0) {
        return 0;
    }

    start = (uint64_t)mdl_byte_offset + (uint64_t)start_offset;
    end = start + (uint64_t)length;

    first = start >> VIRTIOSND_SG_PAGE_SHIFT;
    last = (end - 1u) >> VIRTIOSND_SG_PAGE_SHIFT;
    return (uint32_t)(last - first + 1u);
}

uint32_t virtiosnd_sg_max_elems_for_region(uint32_t mdl_byte_offset,
                                          uint32_t mdl_byte_count,
                                          uint32_t buffer_bytes,
                                          uint32_t offset_bytes,
                                          uint32_t length_bytes,
                                          virtio_bool_t wrap)
{
    uint64_t end;

    if (buffer_bytes == 0) {
        return 0;
    }
    if (mdl_byte_offset >= VIRTIOSND_SG_PAGE_SIZE) {
        return 0;
    }
    if (offset_bytes >= buffer_bytes) {
        return 0;
    }
    if (length_bytes == 0 || length_bytes > buffer_bytes) {
        return 0;
    }
    if (buffer_bytes > mdl_byte_count) {
        return 0;
    }

    end = (uint64_t)offset_bytes + (uint64_t)length_bytes;
    if (end <= (uint64_t)buffer_bytes) {
        return virtiosnd_sg_pages_spanned(mdl_byte_offset, offset_bytes, length_bytes);
    }

    if (wrap == VIRTIO_FALSE) {
        return 0;
    }

    /* Split into [offset, buffer_bytes) and [0, end-buffer_bytes). */
    {
        uint32_t len1;
        uint32_t len2;
        uint32_t p1;
        uint32_t p2;

        len1 = buffer_bytes - offset_bytes;
        len2 = (uint32_t)(end - (uint64_t)buffer_bytes);

        p1 = virtiosnd_sg_pages_spanned(mdl_byte_offset, offset_bytes, len1);
        p2 = virtiosnd_sg_pages_spanned(mdl_byte_offset, 0, len2);
        return p1 + p2;
    }
}

static int virtiosnd_sg_emit_range(const uintptr_t *pfn_array,
                                  uint32_t pfn_count,
                                  uint32_t mdl_byte_offset,
                                  uint32_t range_offset,
                                  uint32_t range_length,
                                  virtio_sg_entry_t *out,
                                  uint16_t max_elems,
                                  uint16_t *inout_count)
{
    uint64_t abs;
    uint32_t remaining;

    abs = (uint64_t)mdl_byte_offset + (uint64_t)range_offset;
    remaining = range_length;

    while (remaining != 0) {
        uint32_t page_index;
        uint32_t page_off;
        uint32_t bytes_in_page;
        uint32_t chunk;
        uint64_t pfn;
        uint64_t paddr;
        virtio_sg_entry_t *prev;

        page_index = (uint32_t)(abs >> VIRTIOSND_SG_PAGE_SHIFT);
        page_off = (uint32_t)(abs & VIRTIOSND_SG_PAGE_MASK);
        if (page_index >= pfn_count) {
            return VIRTIO_ERR_RANGE;
        }

        bytes_in_page = VIRTIOSND_SG_PAGE_SIZE - page_off;
        chunk = remaining < bytes_in_page ? remaining : bytes_in_page;

        pfn = (uint64_t)pfn_array[page_index];
        if (pfn > (VIRTIOSND_U64_MAX >> VIRTIOSND_SG_PAGE_SHIFT)) {
            return VIRTIO_ERR_INVAL;
        }
        paddr = (pfn << VIRTIOSND_SG_PAGE_SHIFT) + (uint64_t)page_off;

        /* Coalesce adjacent physical ranges. */
        if (*inout_count != 0) {
            prev = &out[(uint16_t)(*inout_count - 1u)];
            if (prev->device_writes == VIRTIO_FALSE && prev->addr <= (VIRTIOSND_U64_MAX - (uint64_t)prev->len) &&
                (prev->addr + (uint64_t)prev->len) == paddr) {
                uint64_t merged_len;
                merged_len = (uint64_t)prev->len + (uint64_t)chunk;
                if (merged_len > VIRTIOSND_U32_MAX) {
                    return VIRTIO_ERR_INVAL;
                }
                prev->len = (uint32_t)merged_len;
            } else {
                if (*inout_count >= max_elems) {
                    return VIRTIO_ERR_NOSPC;
                }
                out[*inout_count].addr = paddr;
                out[*inout_count].len = chunk;
                out[*inout_count].device_writes = VIRTIO_FALSE;
                (*inout_count)++;
            }
        } else {
            if (max_elems == 0) {
                return VIRTIO_ERR_NOSPC;
            }
            out[0].addr = paddr;
            out[0].len = chunk;
            out[0].device_writes = VIRTIO_FALSE;
            *inout_count = 1;
        }

        abs += (uint64_t)chunk;
        remaining -= chunk;
    }

    return VIRTIO_OK;
}

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
                                             uint16_t *out_count)
{
    uint64_t span_bytes;
    uint64_t required_pages;
    uint64_t end;
    int rc;
    uint16_t count;

    if (out_count == NULL) {
        return VIRTIO_ERR_INVAL;
    }
    *out_count = 0;

    if (pfn_array == NULL || out == NULL) {
        return VIRTIO_ERR_INVAL;
    }
    if (pfn_count == 0) {
        return VIRTIO_ERR_INVAL;
    }

    if (buffer_bytes == 0) {
        return VIRTIO_ERR_INVAL;
    }
    if (mdl_byte_offset >= VIRTIOSND_SG_PAGE_SIZE) {
        return VIRTIO_ERR_INVAL;
    }
    if (offset_bytes >= buffer_bytes) {
        return VIRTIO_ERR_INVAL;
    }
    if (length_bytes == 0 || length_bytes > buffer_bytes) {
        return VIRTIO_ERR_INVAL;
    }
    if (buffer_bytes > mdl_byte_count) {
        return VIRTIO_ERR_INVAL;
    }

    /*
     * Validate that the PFN array is large enough for the MDL.
     * required_pages = ceil((mdl_byte_offset + mdl_byte_count) / PAGE_SIZE).
     */
    span_bytes = (uint64_t)mdl_byte_offset + (uint64_t)mdl_byte_count;
    required_pages = (span_bytes + (uint64_t)VIRTIOSND_SG_PAGE_SIZE - 1u) >> VIRTIOSND_SG_PAGE_SHIFT;
    if (required_pages > (uint64_t)pfn_count) {
        return VIRTIO_ERR_RANGE;
    }

    end = (uint64_t)offset_bytes + (uint64_t)length_bytes;
    if (end > (uint64_t)buffer_bytes && wrap == VIRTIO_FALSE) {
        return VIRTIO_ERR_INVAL;
    }

    count = 0;

    if (end <= (uint64_t)buffer_bytes) {
        rc = virtiosnd_sg_emit_range(pfn_array,
                                    pfn_count,
                                    mdl_byte_offset,
                                    offset_bytes,
                                    length_bytes,
                                    out,
                                    max_elems,
                                    &count);
        if (rc != VIRTIO_OK) {
            *out_count = 0;
            return rc;
        }
        *out_count = count;
        return VIRTIO_OK;
    }

    /*
     * Wrap case: split into [offset, buffer_bytes) and [0, end-buffer_bytes).
     */
    {
        uint32_t len1;
        uint32_t len2;

        len1 = buffer_bytes - offset_bytes;
        len2 = (uint32_t)(end - (uint64_t)buffer_bytes);

        rc = virtiosnd_sg_emit_range(pfn_array,
                                    pfn_count,
                                    mdl_byte_offset,
                                    offset_bytes,
                                    len1,
                                    out,
                                    max_elems,
                                    &count);
        if (rc != VIRTIO_OK) {
            *out_count = 0;
            return rc;
        }

        rc = virtiosnd_sg_emit_range(pfn_array,
                                    pfn_count,
                                    mdl_byte_offset,
                                    0,
                                    len2,
                                    out,
                                    max_elems,
                                    &count);
        if (rc != VIRTIO_OK) {
            *out_count = 0;
            return rc;
        }
    }

    *out_count = count;
    return VIRTIO_OK;
}
