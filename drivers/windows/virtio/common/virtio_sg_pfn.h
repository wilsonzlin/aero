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

NTSTATUS VirtioSgBuildFromPfns(const UINT64 *pfns, UINT32 pfn_count, size_t first_page_offset, size_t byte_length,
			      BOOLEAN device_write, VIRTQ_SG *out, UINT16 out_cap, UINT16 *out_count);

#if VIRTIO_OSDEP_KERNEL_MODE

ULONG VirtioSgMaxElemsForMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength);

NTSTATUS VirtioSgBuildFromMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength, BOOLEAN device_write, VIRTQ_SG *out,
			     UINT16 out_cap, UINT16 *out_count);

#endif /* VIRTIO_OSDEP_KERNEL_MODE */

#endif /* VIRTIO_SG_PFN_H_ */
