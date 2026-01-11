/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "virtiosnd_sg_core.h"

/*
 * Keep assertions active in all build configurations.
 *
 * These host tests are typically built as part of a CMake Release configuration
 * in CI, which defines NDEBUG and would normally compile out assert() checks.
 * Override assert() so failures are still caught.
 */
#undef assert
#define assert(expr)                                                                                                   \
    do {                                                                                                               \
        if (!(expr)) {                                                                                                 \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                \
            abort();                                                                                                   \
        }                                                                                                              \
    } while (0)

static void test_coalesce_contiguous_pfns(void)
{
    uintptr_t pfns[] = {0x100u, 0x101u, 0x102u};
    virtio_sg_entry_t sg[8];
    uint16_t count;
    int rc;

    count = 0;
    rc = virtiosnd_sg_build_from_pfn_array_region(pfns,
                                                  (uint32_t)VIRTIO_ARRAY_SIZE(pfns),
                                                  0,
                                                  3u * VIRTIOSND_SG_PAGE_SIZE,
                                                  3u * VIRTIOSND_SG_PAGE_SIZE,
                                                  0,
                                                  3u * VIRTIOSND_SG_PAGE_SIZE,
                                                  VIRTIO_FALSE,
                                                  sg,
                                                  (uint16_t)VIRTIO_ARRAY_SIZE(sg),
                                                  &count);
    assert(rc == VIRTIO_OK);
    assert(count == 1);
    assert(sg[0].addr == ((uint64_t)pfns[0] << VIRTIOSND_SG_PAGE_SHIFT));
    assert(sg[0].len == 3u * VIRTIOSND_SG_PAGE_SIZE);
    assert(sg[0].device_writes == VIRTIO_FALSE);
}

static void test_mdl_byte_offset_merges_across_pages(void)
{
    uintptr_t pfns[] = {0x200u, 0x201u};
    virtio_sg_entry_t sg[8];
    uint16_t count;
    int rc;

    count = 0;
    rc = virtiosnd_sg_build_from_pfn_array_region(pfns,
                                                  (uint32_t)VIRTIO_ARRAY_SIZE(pfns),
                                                  128u,
                                                  4096u,
                                                  4096u,
                                                  0,
                                                  4096u,
                                                  VIRTIO_FALSE,
                                                  sg,
                                                  (uint16_t)VIRTIO_ARRAY_SIZE(sg),
                                                  &count);
    assert(rc == VIRTIO_OK);
    assert(count == 1);
    assert(sg[0].addr == (((uint64_t)pfns[0] << VIRTIOSND_SG_PAGE_SHIFT) + 128u));
    assert(sg[0].len == 4096u);
}

static void test_wrap_splits_into_two_ranges(void)
{
    uintptr_t pfns[] = {0x300u, 0x301u};
    virtio_sg_entry_t sg[4];
    uint16_t count;
    int rc;
    uint32_t max_elems;

    max_elems = virtiosnd_sg_max_elems_for_region(0,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  6144u,
                                                  4096u,
                                                  VIRTIO_TRUE);
    assert(max_elems >= 2);

    count = 0;
    rc = virtiosnd_sg_build_from_pfn_array_region(pfns,
                                                  (uint32_t)VIRTIO_ARRAY_SIZE(pfns),
                                                  0,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  6144u,
                                                  4096u,
                                                  VIRTIO_TRUE,
                                                  sg,
                                                  (uint16_t)VIRTIO_ARRAY_SIZE(sg),
                                                  &count);
    assert(rc == VIRTIO_OK);
    assert(count == 2);

    assert(sg[0].addr == (((uint64_t)pfns[1] << VIRTIOSND_SG_PAGE_SHIFT) + 2048u));
    assert(sg[0].len == 2048u);
    assert(sg[0].device_writes == VIRTIO_FALSE);

    assert(sg[1].addr == ((uint64_t)pfns[0] << VIRTIOSND_SG_PAGE_SHIFT));
    assert(sg[1].len == 2048u);
    assert(sg[1].device_writes == VIRTIO_FALSE);

    /* MaxElems too small should fail. */
    count = 0;
    rc = virtiosnd_sg_build_from_pfn_array_region(pfns,
                                                  (uint32_t)VIRTIO_ARRAY_SIZE(pfns),
                                                  0,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  2u * VIRTIOSND_SG_PAGE_SIZE,
                                                  6144u,
                                                  4096u,
                                                  VIRTIO_TRUE,
                                                  sg,
                                                  1,
                                                  &count);
    assert(rc == VIRTIO_ERR_NOSPC);
    assert(count == 0);
}

int main(void)
{
    test_coalesce_contiguous_pfns();
    test_mdl_byte_offset_merges_across_pages();
    test_wrap_splits_into_two_ranges();
    printf("virtiosnd_sg_tests: PASS\n");
    return 0;
}
