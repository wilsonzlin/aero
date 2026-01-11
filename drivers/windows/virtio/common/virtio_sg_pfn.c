#include "virtio_sg_pfn.h"

#define VIRTIO_U64_MAX ((UINT64)~(UINT64)0)
#define VIRTIO_SG_MAX_LEN ((UINT32)0xFFFFFFFFu)

typedef struct _VIRTIO_SG_BUILDER {
	VIRTQ_SG *out;
	UINT32 out_cap;
	UINT32 count; /* required count (may exceed out_cap) */
	BOOLEAN write;

	BOOLEAN have_last;
	UINT64 last_addr;
	UINT32 last_len;
} VIRTIO_SG_BUILDER;

static __inline size_t VirtioMinSize(size_t a, size_t b) { return (a < b) ? a : b; }

static NTSTATUS VirtioSgBuilderAddRange(VIRTIO_SG_BUILDER *b, UINT64 addr, size_t len)
{
	while (len != 0) {
		UINT64 last_end = 0;
		BOOLEAN can_merge = FALSE;

		if (b->have_last && b->last_addr <= (VIRTIO_U64_MAX - (UINT64)b->last_len)) {
			last_end = b->last_addr + (UINT64)b->last_len;
			can_merge = (last_end == addr && b->last_len != VIRTIO_SG_MAX_LEN);
		}

		if (can_merge) {
			UINT32 space = VIRTIO_SG_MAX_LEN - b->last_len;
			size_t take = VirtioMinSize(len, (size_t)space);

			b->last_len += (UINT32)take;
			if (b->out != NULL && b->count != 0 && b->count <= b->out_cap) {
				b->out[b->count - 1].len = b->last_len;
			}

			if (addr > (VIRTIO_U64_MAX - (UINT64)take)) {
				return STATUS_INVALID_PARAMETER;
			}
			addr += (UINT64)take;
			len -= take;
			continue;
		}

		/* Start a new segment. */
		{
			size_t take = (len > (size_t)VIRTIO_SG_MAX_LEN) ? (size_t)VIRTIO_SG_MAX_LEN : len;

			if (b->count == 0xFFFFu) {
				/* VirtqSplitAddBuffer takes a UINT16 sg_count. */
				return STATUS_INVALID_PARAMETER;
			}
			b->count++;

			b->have_last = TRUE;
			b->last_addr = addr;
			b->last_len = (UINT32)take;

			if (b->out != NULL && b->count <= b->out_cap) {
				b->out[b->count - 1].addr = addr;
				b->out[b->count - 1].len = (UINT32)take;
				b->out[b->count - 1].write = b->write;
			}

			if (addr > (VIRTIO_U64_MAX - (UINT64)take)) {
				return STATUS_INVALID_PARAMETER;
			}
			addr += (UINT64)take;
			len -= take;
		}
	}

	return STATUS_SUCCESS;
}

NTSTATUS VirtioSgBuildFromPfns(const UINT64 *pfns, UINT32 pfn_count, size_t first_page_offset, size_t byte_length,
			      BOOLEAN device_write, VIRTQ_SG *out, UINT16 out_cap, UINT16 *out_count)
{
	VIRTIO_SG_BUILDER b;
	UINT64 total_bytes;
	UINT64 start_off;
	UINT64 len64;
	size_t remaining;
	size_t offset;
	UINT32 page_index;

	if (out_count == NULL) {
		return STATUS_INVALID_PARAMETER;
	}
	*out_count = 0;

	if (out == NULL && out_cap != 0) {
		return STATUS_INVALID_PARAMETER;
	}

	if (byte_length == 0) {
		return STATUS_SUCCESS;
	}

	if (pfns == NULL || pfn_count == 0) {
		return STATUS_INVALID_PARAMETER;
	}

	if (first_page_offset >= PAGE_SIZE) {
		return STATUS_INVALID_PARAMETER;
	}

	total_bytes = ((UINT64)pfn_count) << PAGE_SHIFT;
	start_off = (UINT64)first_page_offset;
	len64 = (UINT64)byte_length;

	if (start_off > total_bytes) {
		return STATUS_INVALID_PARAMETER;
	}
	if (len64 > (total_bytes - start_off)) {
		return STATUS_INVALID_PARAMETER;
	}

	b.out = out;
	b.out_cap = (UINT32)out_cap;
	b.count = 0;
	b.write = device_write ? TRUE : FALSE;
	b.have_last = FALSE;
	b.last_addr = 0;
	b.last_len = 0;

	remaining = byte_length;
	offset = first_page_offset;
	page_index = 0;

	while (remaining != 0) {
		UINT64 pfn;
		UINT64 addr;
		size_t chunk;
		NTSTATUS status;

		if (page_index >= pfn_count) {
			return STATUS_INVALID_PARAMETER;
		}

		pfn = pfns[page_index];
		if (pfn > (VIRTIO_U64_MAX >> PAGE_SHIFT)) {
			return STATUS_INVALID_PARAMETER;
		}
		addr = (pfn << PAGE_SHIFT) + (UINT64)offset;

		chunk = PAGE_SIZE - offset;
		chunk = VirtioMinSize(chunk, remaining);

		status = VirtioSgBuilderAddRange(&b, addr, chunk);
		if (!NT_SUCCESS(status)) {
			return status;
		}

		remaining -= chunk;
		offset = 0;
		page_index++;
	}

	*out_count = (UINT16)b.count;
	return (b.count > b.out_cap) ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
}

#if VIRTIO_OSDEP_KERNEL_MODE

static NTSTATUS VirtioSgGetMdlChainByteCount64(PMDL Mdl, UINT64 *TotalBytes)
{
	UINT64 total = 0;
	PMDL cur;

	if (Mdl == NULL || TotalBytes == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	for (cur = Mdl; cur != NULL; cur = cur->Next) {
		UINT64 mdl_bytes = (UINT64)MmGetMdlByteCount(cur);
		if (total > (VIRTIO_U64_MAX - mdl_bytes)) {
			return STATUS_INVALID_PARAMETER;
		}
		total += mdl_bytes;
	}

	*TotalBytes = total;
	return STATUS_SUCCESS;
}

static NTSTATUS VirtioSgValidateMdlChainRange(PMDL Mdl, size_t ByteOffset, size_t ByteLength)
{
	UINT64 total;
	UINT64 off = (UINT64)ByteOffset;
	UINT64 len = (UINT64)ByteLength;
	NTSTATUS status;

	status = VirtioSgGetMdlChainByteCount64(Mdl, &total);
	if (!NT_SUCCESS(status)) {
		return status;
	}

	if (off > total) {
		return STATUS_INVALID_PARAMETER;
	}

	if (len > (total - off)) {
		return STATUS_INVALID_PARAMETER;
	}

	return STATUS_SUCCESS;
}

ULONG VirtioSgMaxElemsForMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength)
{
	size_t remaining_offset;
	size_t remaining_len;
	ULONG pages;
	PMDL cur;

	if (!NT_SUCCESS(VirtioSgValidateMdlChainRange(Mdl, ByteOffset, ByteLength))) {
		return 0;
	}

	if (ByteLength == 0) {
		return 0;
	}

	remaining_offset = ByteOffset;
	remaining_len = ByteLength;
	pages = 0;

	for (cur = Mdl; cur != NULL && remaining_len != 0; cur = cur->Next) {
		size_t mdl_bytes = (size_t)MmGetMdlByteCount(cur);
		size_t local_offset;
		size_t local_len;
		UINT64 start;
		UINT64 end;
		ULONG start_page;
		ULONG end_page_excl;
		ULONG span_pages;

		if (remaining_offset >= mdl_bytes) {
			remaining_offset -= mdl_bytes;
			continue;
		}

		local_offset = remaining_offset;
		local_len = VirtioMinSize(remaining_len, mdl_bytes - local_offset);
		remaining_offset = 0;

		start = (UINT64)MmGetMdlByteOffset(cur) + (UINT64)local_offset;
		end = start + (UINT64)local_len; /* one past last byte */

		start_page = (ULONG)(start >> PAGE_SHIFT);
		end_page_excl = (ULONG)((end + (PAGE_SIZE - 1)) >> PAGE_SHIFT);
		span_pages = end_page_excl - start_page;

		if (span_pages > (MAXULONG - pages)) {
			return MAXULONG;
		}

		pages += span_pages;
		remaining_len -= local_len;
	}

	return (remaining_len == 0) ? pages : 0;
}

NTSTATUS VirtioSgBuildFromMdl(PMDL Mdl, size_t ByteOffset, size_t ByteLength, BOOLEAN device_write, VIRTQ_SG *out,
			     UINT16 out_cap, UINT16 *out_count)
{
	VIRTIO_SG_BUILDER b;
	NTSTATUS status;
	PMDL cur;
	size_t remaining_offset;
	size_t remaining_len;

	if (out_count == NULL) {
		return STATUS_INVALID_PARAMETER;
	}
	*out_count = 0;

	if (out == NULL && out_cap != 0) {
		return STATUS_INVALID_PARAMETER;
	}

	status = VirtioSgValidateMdlChainRange(Mdl, ByteOffset, ByteLength);
	if (!NT_SUCCESS(status)) {
		return status;
	}

	if (ByteLength == 0) {
		return STATUS_SUCCESS;
	}

	/*
	 * KeFlushIoBuffers is safe at DISPATCH_LEVEL. On coherent x86/x64 it is
	 * typically a no-op, but it is required for non-coherent platforms.
	 */
	for (cur = Mdl; cur != NULL; cur = cur->Next) {
		KeFlushIoBuffers(cur, /*ReadOperation*/ device_write, /*DmaOperation*/ TRUE);
	}

	b.out = out;
	b.out_cap = (UINT32)out_cap;
	b.count = 0;
	b.write = device_write ? TRUE : FALSE;
	b.have_last = FALSE;
	b.last_addr = 0;
	b.last_len = 0;

	remaining_offset = ByteOffset;
	remaining_len = ByteLength;

	for (cur = Mdl; cur != NULL && remaining_len != 0; cur = cur->Next) {
		size_t mdl_bytes = (size_t)MmGetMdlByteCount(cur);
		size_t local_offset;
		size_t local_len;
		const PFN_NUMBER *pfns;
		UINT64 start;
		ULONG pfn_index;
		size_t offset_in_page;
		size_t remain_local;

		if (remaining_offset >= mdl_bytes) {
			remaining_offset -= mdl_bytes;
			continue;
		}

		local_offset = remaining_offset;
		local_len = VirtioMinSize(remaining_len, mdl_bytes - local_offset);
		remaining_offset = 0;

		pfns = MmGetMdlPfnArray(cur);
		start = (UINT64)MmGetMdlByteOffset(cur) + (UINT64)local_offset;
		pfn_index = (ULONG)(start >> PAGE_SHIFT);
		offset_in_page = (size_t)(start & (PAGE_SIZE - 1));

		remain_local = local_len;
		while (remain_local != 0) {
			PFN_NUMBER pfn;
			UINT64 addr;
			size_t chunk;

			pfn = pfns[pfn_index];
			if ((UINT64)pfn > (VIRTIO_U64_MAX >> PAGE_SHIFT)) {
				return STATUS_INVALID_PARAMETER;
			}
			addr = ((UINT64)pfn << PAGE_SHIFT) + (UINT64)offset_in_page;

			chunk = PAGE_SIZE - offset_in_page;
			chunk = VirtioMinSize(chunk, remain_local);

			status = VirtioSgBuilderAddRange(&b, addr, chunk);
			if (!NT_SUCCESS(status)) {
				return status;
			}

			remain_local -= chunk;
			offset_in_page = 0;
			pfn_index++;
		}

		remaining_len -= local_len;
	}

	if (remaining_len != 0) {
		return STATUS_INVALID_PARAMETER;
	}

	*out_count = (UINT16)b.count;
	return (b.count > b.out_cap) ? STATUS_BUFFER_TOO_SMALL : STATUS_SUCCESS;
}

#endif /* VIRTIO_OSDEP_KERNEL_MODE */

