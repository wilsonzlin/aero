#include "virtio_sg.h"

#include <ntintsafe.h>

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_WDFDMA_MAPPING, VirtioWdfDmaGetMappingContext);

static _Must_inspect_result_ NTSTATUS
VirtioSgGetMdlChainByteCount(
    _In_ PMDL Mdl,
    _Out_ size_t* TotalBytes
    )
{
    size_t total;
    PMDL cur;
    NTSTATUS status;

    if (Mdl == NULL || TotalBytes == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    total = 0;

    for (cur = Mdl; cur != NULL; cur = cur->Next) {
        status = RtlSizeTAdd(total, (size_t)MmGetMdlByteCount(cur), &total);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    *TotalBytes = total;
    return STATUS_SUCCESS;
}

static VOID
VirtioSgFreeMdlChain(
    _In_opt_ PMDL Mdl
    )
{
    PMDL cur = Mdl;
    while (cur != NULL) {
        PMDL next = cur->Next;
        cur->Next = NULL;
        IoFreeMdl(cur);
        cur = next;
    }
}

static _Must_inspect_result_ NTSTATUS
VirtioSgBuildPartialMdlChain(
    _In_ PMDL SourceMdl,
    _In_ size_t ByteOffset,
    _In_ size_t ByteLength,
    _Out_ PMDL* OutPartialMdlChain
    )
{
    size_t remainingOffset;
    size_t remainingLen;
    PMDL head;
    PMDL tail;
    PMDL cur;
    size_t mdlBytes;
    size_t localOffset;
    size_t localLen;
    PUCHAR startVa;
    PMDL partial;

    if (OutPartialMdlChain == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutPartialMdlChain = NULL;

    if (ByteLength == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    remainingOffset = ByteOffset;
    remainingLen = ByteLength;

    head = NULL;
    tail = NULL;

    for (cur = SourceMdl; cur != NULL && remainingLen != 0; cur = cur->Next) {
        mdlBytes = (size_t)MmGetMdlByteCount(cur);
        if (remainingOffset >= mdlBytes) {
            remainingOffset -= mdlBytes;
            continue;
        }

        localOffset = remainingOffset;
        localLen = min(remainingLen, mdlBytes - localOffset);
        remainingOffset = 0;

        if (localLen > MAXULONG) {
            VirtioSgFreeMdlChain(head);
            return STATUS_INVALID_PARAMETER;
        }

        startVa = (PUCHAR)MmGetMdlVirtualAddress(cur) + localOffset;

        partial = IoAllocateMdl(startVa, (ULONG)localLen, FALSE, FALSE, NULL);
        if (partial == NULL) {
            VirtioSgFreeMdlChain(head);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        IoBuildPartialMdl(cur, partial, startVa, (ULONG)localLen);

        if (head == NULL) {
            head = partial;
            tail = partial;
        } else {
            tail->Next = partial;
            tail = partial;
        }

        remainingLen -= localLen;
    }

    if (remainingLen != 0) {
        VirtioSgFreeMdlChain(head);
        return STATUS_INVALID_PARAMETER;
    }

    *OutPartialMdlChain = head;
    return STATUS_SUCCESS;
}

static _Must_inspect_result_ NTSTATUS
VirtioSgCopyScatterGatherListToVirtio(
    _In_ const SCATTER_GATHER_LIST* SgList,
    _In_ BOOLEAN DeviceWrite,
    _Out_writes_(OutCapacity) VIRTIO_SG_ELEM* OutElems,
    _In_ ULONG OutCapacity,
    _Out_ ULONG* OutCount
    )
{
    ULONG elemCount;
    UINT64 lastAddr;
    ULONG lastLen;
    BOOLEAN haveLast;
    ULONG i;
    const SCATTER_GATHER_ELEMENT* e;
    UINT64 addr;
    ULONG len;

    if (SgList == NULL || OutElems == NULL || OutCount == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    elemCount = 0;
    lastAddr = 0;
    lastLen = 0;
    haveLast = FALSE;

    for (i = 0; i < SgList->NumberOfElements; i++) {
        e = &SgList->Elements[i];
        addr = (UINT64)e->Address.QuadPart;
        len = e->Length;

        if (len == 0) {
            continue;
        }

        if (haveLast &&
            (lastAddr + (UINT64)lastLen) == addr &&
            ((UINT64)lastLen + len) <= MAXULONG) {
            lastLen += len;
            OutElems[elemCount - 1].Len = lastLen;
        } else {
            if (elemCount >= OutCapacity) {
                *OutCount = elemCount;
                return STATUS_BUFFER_TOO_SMALL;
            }

            OutElems[elemCount].Addr = addr;
            OutElems[elemCount].Len = len;
            OutElems[elemCount].DeviceWrite = DeviceWrite;

            elemCount++;
            haveLast = TRUE;
            lastAddr = addr;
            lastLen = len;
        }
    }

    *OutCount = elemCount;
    return STATUS_SUCCESS;
}

static BOOLEAN
VirtioWdfDmaEvtProgramDma(
    _In_ WDFDMATRANSACTION Transaction,
    _In_ WDFDEVICE Device,
    _In_ PVOID Context,
    _In_ WDF_DMA_DIRECTION Direction,
    _In_ PSCATTER_GATHER_LIST SgList
    )
{
    VIRTIO_WDFDMA_MAPPING* mapping;
    BOOLEAN deviceWrite;
    ULONG count;
    NTSTATUS status;
    size_t bytesMapped;
    ULONG i;

    UNREFERENCED_PARAMETER(Transaction);
    UNREFERENCED_PARAMETER(Device);

    mapping = (VIRTIO_WDFDMA_MAPPING*)Context;
    if (mapping == NULL || SgList == NULL) {
        return FALSE;
    }

    deviceWrite = (Direction == WdfDmaDirectionReadFromDevice) ? TRUE : FALSE;

    count = 0;
    status = VirtioSgCopyScatterGatherListToVirtio(
        SgList,
        deviceWrite,
        mapping->Sg.Elems,
        mapping->SgCapacity,
        &count);

    if (!NT_SUCCESS(status)) {
        mapping->Sg.Count = 0;
        return FALSE;
    }

    bytesMapped = 0;
    for (i = 0; i < SgList->NumberOfElements; i++) {
        status = RtlSizeTAdd(bytesMapped, (size_t)SgList->Elements[i].Length, &bytesMapped);
        if (!NT_SUCCESS(status)) {
            mapping->Sg.Count = 0;
            return FALSE;
        }
    }

    /*
     * Virtio expects a single descriptor chain to describe the entire buffer.
     * If the DMA adapter/framework split the mapping (max-length, max-SG, etc.),
     * bytesMapped will be smaller than the requested transfer length.
     */
    if (bytesMapped != mapping->ByteLength) {
        mapping->Sg.Count = 0;
        return FALSE;
    }

    mapping->Sg.Count = count;

    if (mapping->UserEvtProgramDma != NULL) {
        return mapping->UserEvtProgramDma(Transaction, Device, Context, Direction, SgList);
    }

    return TRUE;
}

_Must_inspect_result_
NTSTATUS
VirtioWdfDmaStartMapping(
    _In_ VIRTIO_DMA_CONTEXT* Dma,
    _In_opt_ WDFREQUEST RequestOrNull,
    _In_opt_ PMDL Mdl,
    _In_ size_t Offset,
    _In_ size_t Length,
    _In_ WDF_DMA_DIRECTION Direction,
    _In_opt_ EVT_WDF_PROGRAM_DMA* EvtProgramDma,
    _In_ WDFOBJECT Parent,
    _Out_ VIRTIO_WDFDMA_MAPPING** OutMapping
    )
{
    PMDL sourceMdl;
    NTSTATUS status;
    size_t totalBytes;
    ULONG maxElems;
    ULONG maxSg;
    WDF_OBJECT_ATTRIBUTES objAttributes;
    WDFOBJECT obj;
    VIRTIO_WDFDMA_MAPPING* mapping;
    WDF_OBJECT_ATTRIBUTES memAttributes;
    PMDL mappingMdl;
    WDF_OBJECT_ATTRIBUTES txAttributes;

    if (Dma == NULL || OutMapping == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutMapping = NULL;

    if (KeGetCurrentIrql() > APC_LEVEL) {
        /*
         * This routine allocates WDF objects and (optionally) builds a partial
         * MDL chain; require <= APC_LEVEL to avoid allocating at DISPATCH_LEVEL.
         */
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Length == 0 || Length > MAXULONG) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dma->DmaEnabler == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    {
        size_t maxLen = Dma->MaxTransferLength;
        if (Length > maxLen) {
            /*
             * WDF will split transfers larger than the enabler/adapter maximum
             * into multiple EvtProgramDma invocations. This mapping helper is
             * intentionally single-shot (one SG list for the entire virtqueue
             * submission), so reject oversize buffers early.
             */
            return STATUS_INVALID_BUFFER_SIZE;
        }
    }

    sourceMdl = Mdl;
    if (sourceMdl == NULL) {
        if (RequestOrNull == NULL) {
            return STATUS_INVALID_PARAMETER;
        }

        if (Direction == WdfDmaDirectionReadFromDevice) {
            status = WdfRequestRetrieveOutputWdmMdl(RequestOrNull, &sourceMdl);
        } else {
            status = WdfRequestRetrieveInputWdmMdl(RequestOrNull, &sourceMdl);
        }

        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    totalBytes = 0;
    status = VirtioSgGetMdlChainByteCount(sourceMdl, &totalBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (Offset > totalBytes || Length > (totalBytes - Offset)) {
        return STATUS_INVALID_PARAMETER;
    }

    maxElems = VirtioSgMaxElemsForMdl(sourceMdl, Offset, Length);
    if (maxElems == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    maxSg = Dma->MaxScatterGatherElements;
    if (maxSg == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&objAttributes, VIRTIO_WDFDMA_MAPPING);
    objAttributes.ParentObject = Parent;

    status = WdfObjectCreate(&objAttributes, &obj);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    mapping = VirtioWdfDmaGetMappingContext(obj);
    RtlZeroMemory(mapping, sizeof(*mapping));

    mapping->Object = obj;
    mapping->Transaction = NULL;
    mapping->PartialMdlChain = NULL;
    mapping->ElemMemory = NULL;
    mapping->Sg.Elems = NULL;
    mapping->Sg.Count = 0;
    mapping->SgCapacity = maxSg;
    mapping->ByteLength = Length;
    mapping->UserEvtProgramDma = EvtProgramDma;

    WDF_OBJECT_ATTRIBUTES_INIT(&memAttributes);
    memAttributes.ParentObject = obj;

    {
        size_t elemBytes = 0;
        status = RtlSizeTMult((size_t)mapping->SgCapacity, sizeof(VIRTIO_SG_ELEM), &elemBytes);
        if (NT_SUCCESS(status)) {
            status = WdfMemoryCreate(
                &memAttributes,
                NonPagedPool,
                'gSIV',
                elemBytes,
                &mapping->ElemMemory,
                (PVOID*)&mapping->Sg.Elems);
        }
    }

    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(obj);
        return status;
    }

    mappingMdl = sourceMdl;
    if (Offset != 0 || Length != totalBytes) {
        status = VirtioSgBuildPartialMdlChain(sourceMdl, Offset, Length, &mapping->PartialMdlChain);
        if (!NT_SUCCESS(status)) {
            WdfObjectDelete(obj);
            return status;
        }
        mappingMdl = mapping->PartialMdlChain;
    }

    WDF_OBJECT_ATTRIBUTES_INIT(&txAttributes);
    txAttributes.ParentObject = obj;

    status = WdfDmaTransactionCreate(Dma->DmaEnabler, &txAttributes, &mapping->Transaction);
    if (!NT_SUCCESS(status)) {
        VirtioSgFreeMdlChain(mapping->PartialMdlChain);
        mapping->PartialMdlChain = NULL;
        WdfObjectDelete(obj);
        return status;
    }

    status = WdfDmaTransactionInitialize(
        mapping->Transaction,
        VirtioWdfDmaEvtProgramDma,
        Direction,
        mappingMdl,
        MmGetMdlVirtualAddress(mappingMdl),
        Length);

    if (!NT_SUCCESS(status)) {
        VirtioSgFreeMdlChain(mapping->PartialMdlChain);
        mapping->PartialMdlChain = NULL;
        WdfObjectDelete(obj);
        return status;
    }

    status = WdfDmaTransactionExecute(mapping->Transaction, mapping);
    if (!NT_SUCCESS(status)) {
        VirtioSgFreeMdlChain(mapping->PartialMdlChain);
        mapping->PartialMdlChain = NULL;
        WdfObjectDelete(obj);
        return status;
    }

    *OutMapping = mapping;
    return STATUS_SUCCESS;
}

VOID
VirtioWdfDmaCompleteAndRelease(
    _In_ VIRTIO_WDFDMA_MAPPING* Mapping
    )
{
    if (Mapping == NULL) {
        return;
    }

    if (Mapping->Transaction != NULL) {
        NTSTATUS status;
        (VOID)WdfDmaTransactionDmaCompletedFinal(Mapping->Transaction, Mapping->ByteLength, &status);
    }

    if (Mapping->PartialMdlChain != NULL) {
        VirtioSgFreeMdlChain(Mapping->PartialMdlChain);
        Mapping->PartialMdlChain = NULL;
    }

    if (Mapping->Object != NULL) {
        WdfObjectDelete(Mapping->Object);
    }
}
