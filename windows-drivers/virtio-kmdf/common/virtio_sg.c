#include "virtio_sg.h"

#include <ntintsafe.h>

static _Must_inspect_result_ NTSTATUS
VirtioSgGetMdlChainByteCount(
    _In_ PMDL Mdl,
    _Out_ size_t* TotalBytes
    )
{
    size_t total = 0;
    PMDL cur;
    NTSTATUS status;

    if (Mdl == NULL || TotalBytes == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    for (cur = Mdl; cur != NULL; cur = cur->Next) {
        status = RtlSizeTAdd(total, (size_t)MmGetMdlByteCount(cur), &total);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    *TotalBytes = total;
    return STATUS_SUCCESS;
}

static _Must_inspect_result_ NTSTATUS
VirtioSgValidateMdlChainRange(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _Out_opt_ size_t* TotalBytes
    )
{
    size_t total = 0;
    NTSTATUS status = VirtioSgGetMdlChainByteCount(Mdl, &total);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (TotalBytes != NULL) {
        *TotalBytes = total;
    }

    if (ByteOffset > total) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ByteLength > (total - ByteOffset)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (ByteLength > MAXULONG) {
        /* Virtio descriptor length is 32-bit. */
        return STATUS_INVALID_PARAMETER;
    }

    return STATUS_SUCCESS;
}

ULONG
VirtioSgMaxElemsForMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength
    )
{
    size_t remainingOffset;
    size_t remainingLen;
    ULONG pages;
    PMDL cur;
    size_t mdlBytes;
    size_t localOffset;
    size_t localLen;
    size_t start;
    size_t end;
    ULONG startPage;
    ULONG endPageExclusive;
    ULONG spanPages;

    if (!NT_SUCCESS(VirtioSgValidateMdlChainRange(Mdl, ByteOffset, ByteLength, NULL))) {
        return 0;
    }

    if (ByteLength == 0) {
        return 0;
    }

    remainingOffset = ByteOffset;
    remainingLen = ByteLength;
    pages = 0;

    for (cur = Mdl; cur != NULL && remainingLen != 0; cur = cur->Next) {
        mdlBytes = (size_t)MmGetMdlByteCount(cur);
        if (remainingOffset >= mdlBytes) {
            remainingOffset -= mdlBytes;
            continue;
        }

        localOffset = remainingOffset;
        localLen = min(remainingLen, mdlBytes - localOffset);
        remainingOffset = 0;

        start = (size_t)MmGetMdlByteOffset(cur) + localOffset;
        end = start + localLen; /* one past last byte */

        startPage = (ULONG)(start >> PAGE_SHIFT);
        endPageExclusive = (ULONG)((end + (PAGE_SIZE - 1)) >> PAGE_SHIFT);
        spanPages = endPageExclusive - startPage;

        if (spanPages > (MAXULONG - pages)) {
            return MAXULONG;
        }

        pages += spanPages;
        remainingLen -= localLen;
    }

    return (remainingLen == 0) ? pages : 0;
}

_Must_inspect_result_
NTSTATUS
VirtioSgBuildFromMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _In_ BOOLEAN DeviceWrite,
    _Out_writes_opt_(OutCapacity) VIRTIO_SG_ELEM* OutElems,
    _In_ ULONG OutCapacity,
    _Out_ ULONG* OutCount
    )
{
    NTSTATUS status;
    PMDL cur;
    size_t remainingOffset;
    size_t remainingLen;
    ULONG elemCount;
    UINT64 lastAddr;
    ULONG lastLen;
    BOOLEAN haveLast;
    size_t mdlBytes;
    size_t localOffset;
    size_t localLen;
    const PFN_NUMBER* pfns;
    ULONG mdlByteOffset;
    size_t start;
    ULONG pfnIndex;
    ULONG offsetInPage;
    size_t remainLocal;
    PFN_NUMBER pfn;
    UINT64 addr;
    size_t chunk;

    if (OutCount == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutCount = 0;

    if (OutElems == NULL && OutCapacity != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSgValidateMdlChainRange(Mdl, ByteOffset, ByteLength, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (ByteLength == 0) {
        return STATUS_SUCCESS;
    }

    /*
     * KeFlushIoBuffers is a no-op on coherent x86/x64, but required on
     * non-coherent architectures. It is safe to call at DISPATCH_LEVEL.
     *
     * Flush the entire MDL chain up-front; we could be mapping a subrange but
     * flushing the full chain is conservative and keeps the API simple.
     */
    for (cur = Mdl; cur != NULL; cur = cur->Next) {
        KeFlushIoBuffers(cur, /*ReadOperation*/ DeviceWrite, /*DmaOperation*/ TRUE);
    }

    remainingOffset = ByteOffset;
    remainingLen = ByteLength;

    elemCount = 0;
    lastAddr = 0;
    lastLen = 0;
    haveLast = FALSE;

    for (cur = Mdl; cur != NULL && remainingLen != 0; cur = cur->Next) {
        mdlBytes = (size_t)MmGetMdlByteCount(cur);
        if (remainingOffset >= mdlBytes) {
            remainingOffset -= mdlBytes;
            continue;
        }

        localOffset = remainingOffset;
        localLen = min(remainingLen, mdlBytes - localOffset);
        remainingOffset = 0;

        pfns = MmGetMdlPfnArray(cur);
        mdlByteOffset = MmGetMdlByteOffset(cur);

        start = (size_t)mdlByteOffset + localOffset;
        pfnIndex = (ULONG)(start >> PAGE_SHIFT);
        offsetInPage = (ULONG)(start & (PAGE_SIZE - 1));

        remainLocal = localLen;
        while (remainLocal != 0) {
            pfn = pfns[pfnIndex];
            addr = ((UINT64)pfn << PAGE_SHIFT) + offsetInPage;

            chunk = min((size_t)(PAGE_SIZE - offsetInPage), remainLocal);
            NT_ASSERT(chunk <= MAXULONG);

            if (haveLast &&
                (lastAddr + (UINT64)lastLen) == addr &&
                ((UINT64)lastLen + chunk) <= MAXULONG) {
                lastLen += (ULONG)chunk;
                if (elemCount <= OutCapacity && OutElems != NULL) {
                    OutElems[elemCount - 1].Len = lastLen;
                }
            } else {
                elemCount++;
                haveLast = TRUE;
                lastAddr = addr;
                lastLen = (ULONG)chunk;

                if (elemCount <= OutCapacity && OutElems != NULL) {
                    OutElems[elemCount - 1].Addr = addr;
                    OutElems[elemCount - 1].Len = (ULONG)chunk;
                    OutElems[elemCount - 1].DeviceWrite = DeviceWrite;
                }
            }

            remainLocal -= chunk;
            offsetInPage = 0;
            pfnIndex++;
        }

        remainingLen -= localLen;
    }

    *OutCount = elemCount;

    if (elemCount > OutCapacity) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    return STATUS_SUCCESS;
}

#if DBG
VOID
VirtioSgDebugDumpList(
    _In_reads_(Count) const VIRTIO_SG_ELEM* Elems,
    _In_ ULONG Count,
    _In_opt_ PCSTR Prefix
    )
{
    PCSTR pfx = (Prefix != NULL) ? Prefix : "virtio-sg";
    ULONG i;
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "%s: %lu elems\n", pfx, Count);

    for (i = 0; i < Count; i++) {
        const VIRTIO_SG_ELEM* e = &Elems[i];
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_INFO_LEVEL,
            "%s:   [%lu] addr=0x%I64x len=%lu deviceWrite=%u\n",
            pfx,
            i,
            e->Addr,
            e->Len,
            (UINT)e->DeviceWrite);
    }
}

VOID
VirtioSgDebugDumpMdl(
    _In_ PMDL Mdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _In_ BOOLEAN DeviceWrite
    )
{
    ULONG maxElems;
    VIRTIO_SG_ELEM stackElems[32];
    VIRTIO_SG_ELEM* elems;
    ULONG capacity;
    WDFMEMORY mem;
    ULONG count;
    NTSTATUS status;

    maxElems = VirtioSgMaxElemsForMdl(Mdl, ByteOffset, ByteLength);
    if (maxElems == 0) {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_INFO_LEVEL,
            "virtio-sg: invalid MDL range (offset=%Iu len=%Iu)\n",
            ByteOffset,
            ByteLength);
        return;
    }

    elems = stackElems;
    capacity = ARRAYSIZE(stackElems);
    mem = NULL;

    if (maxElems > capacity) {
        size_t elemBytes = 0;
        status = RtlSizeTMult((size_t)maxElems, sizeof(VIRTIO_SG_ELEM), &elemBytes);
        if (!NT_SUCCESS(status)) {
            DbgPrintEx(
                DPFLTR_IHVDRIVER_ID,
                DPFLTR_INFO_LEVEL,
                "virtio-sg: size overflow for %lu elems: 0x%08X\n",
                maxElems,
                status);
            return;
        }

        status = WdfMemoryCreate(
            WDF_NO_OBJECT_ATTRIBUTES,
            NonPagedPool,
            'gSIV',
            elemBytes,
            &mem,
            (PVOID*)&elems);
        if (!NT_SUCCESS(status)) {
            DbgPrintEx(
                DPFLTR_IHVDRIVER_ID,
                DPFLTR_INFO_LEVEL,
                "virtio-sg: WdfMemoryCreate failed: 0x%08X\n",
                status);
            return;
        }
        capacity = maxElems;
    }

    count = 0;
    status = VirtioSgBuildFromMdl(Mdl, ByteOffset, ByteLength, DeviceWrite, elems, capacity, &count);
    if (NT_SUCCESS(status)) {
        VirtioSgDebugDumpList(elems, count, "virtio-sg");
    } else {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_INFO_LEVEL,
            "virtio-sg: VirtioSgBuildFromMdl failed: 0x%08X (count=%lu)\n",
            status,
            count);
    }

    if (mem != NULL) {
        WdfObjectDelete(mem);
    }
}
#endif
