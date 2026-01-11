#ifndef VIRTIO_SG_PFN_H_
#define VIRTIO_SG_PFN_H_

/*
 * WDF-free scatter/gather (SG) builder for WDM virtio drivers.
 *
 * This module converts a PFN list (or, in kernel mode, an MDL chain) into an
 * array of VIRTQ_SG segments suitable for VirtqSplitAddBuffer().
 *
 * The core PFN builder is portable and unit-tested in user-mode.
 */

#include "virtqueue_split.h"

/*
 * WDK provides PAGE_SHIFT/PAGE_SIZE in kernel mode. Define a 4KiB default for
 * user-mode tests.
 */
#ifndef PAGE_SHIFT
#define PAGE_SHIFT 12
#endif
#ifndef PAGE_SIZE
#define PAGE_SIZE ((size_t)1u << PAGE_SHIFT)
#endif

/*
 * Build a VIRTQ_SG list from a PFN array describing a physically-backed buffer.
 *
 * `pfns[i]` is treated as a page frame number (PFN). The corresponding segment
 * address is:
 *
 *   addr = (pfn << PAGE_SHIFT) + offset_in_page
 *
 * The builder walks the requested range and:
 *  - Coalesces physically contiguous PFNs into larger segments when possible.
 *  - Ensures each segment length fits in 32-bit (virtio descriptor `len`).
 *
 * Return values:
 *  - STATUS_SUCCESS:
 *      *out_count is set to the number of SG elements written (<= out_cap).
 *  - STATUS_BUFFER_TOO_SMALL:
 *      *out_count is set to the number of SG elements required.
 *      `out` (if non-NULL) contains the first out_cap elements.
 *
 * Notes:
 *  - No allocations; suitable for DISPATCH_LEVEL.
 *  - If the mapping would require > UINT16_MAX SG elements, the function
 *    returns STATUS_INVALID_PARAMETER.
 */
NTSTATUS VirtioSgBuildFromPfns(const UINT64 *pfns, UINT32 pfn_count, size_t first_page_offset, size_t byte_length,
			      BOOLEAN device_write, VIRTQ_SG *out, UINT16 out_cap, UINT16 *out_count);

#if VIRTIO_OSDEP_KERNEL_MODE

/*
 * Returns a worst-case upper bound on the number of SG elements required to
 * describe the requested byte range within an MDL chain (essentially the number
 * of pages spanned by the range).
 *
 * Returns 0 if the range is invalid.
 */
ULONG VirtioSgMaxElemsForMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength);

/*
 * Build a VIRTQ_SG list from an MDL chain by walking the PFN array(s) and
 * generating per-page segments, coalescing physically-contiguous PFNs.
 *
 * Calls KeFlushIoBuffers() on each MDL in the chain for cache coherency.
 *
 * No allocations; suitable for DISPATCH_LEVEL.
 *
 * Return values follow VirtioSgBuildFromPfns() (STATUS_SUCCESS /
 * STATUS_BUFFER_TOO_SMALL with required count).
 */
NTSTATUS VirtioSgBuildFromMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength, BOOLEAN device_write, VIRTQ_SG *out,
			     UINT16 out_cap, UINT16 *out_count);

#endif /* VIRTIO_OSDEP_KERNEL_MODE */

#endif /* VIRTIO_SG_PFN_H_ */
