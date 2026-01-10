#include "virtio_dma.h"

static UINT64 VirtioDmaLogicalAddressToU64(_In_ WDF_LOGICAL_ADDRESS Address)
{
    return (UINT64)(ULONGLONG)Address.QuadPart;
}

static const char* VirtioDmaProfileName(_In_ WDF_DMA_PROFILE Profile)
{
    switch (Profile) {
    case WdfDmaProfileScatterGatherDuplex:
        return "ScatterGatherDuplex";
    case WdfDmaProfileScatterGather64Duplex:
        return "ScatterGather64Duplex";
    default:
        return "Unknown";
    }
}

_Must_inspect_result_
NTSTATUS VirtioDmaCreate(
    _In_ WDFDEVICE Device,
    _In_ size_t MaxTransferLength,
    _In_ ULONG MaxSgElements,
    _In_ BOOLEAN Prefer64Bit,
    _Outptr_result_nullonfailure_ VIRTIO_DMA_CONTEXT** OutCtx)
{
    NTSTATUS status;
    WDFOBJECT ctxObject;
    VIRTIO_DMA_CONTEXT* ctx;

    if (OutCtx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *OutCtx = NULL;

    if ((MaxTransferLength == 0) || (MaxSgElements == 0)) {
        return STATUS_INVALID_PARAMETER;
    }

    WDF_OBJECT_ATTRIBUTES ctxAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&ctxAttributes, VIRTIO_DMA_CONTEXT);
    ctxAttributes.ParentObject = Device;

    status = WdfObjectCreate(&ctxAttributes, &ctxObject);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    ctx = VirtioDmaGetContext(ctxObject);
    RtlZeroMemory(ctx, sizeof(*ctx));
    ctx->Object = ctxObject;
    ctx->Prefer64Bit = Prefer64Bit;
    ctx->MaxTransferLength = MaxTransferLength;
    ctx->MaxScatterGatherElements = MaxSgElements;

    WDF_DMA_PROFILE profile = WdfDmaProfileScatterGatherDuplex;
    if (Prefer64Bit) {
        profile = WdfDmaProfileScatterGather64Duplex;
    }

    WDF_DMA_ENABLER_CONFIG dmaConfig;
    WDF_DMA_ENABLER_CONFIG_INIT(&dmaConfig, profile, MaxTransferLength);

    WDF_OBJECT_ATTRIBUTES dmaAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&dmaAttributes);
    dmaAttributes.ParentObject = ctxObject;

    status = WdfDmaEnablerCreate(Device, &dmaConfig, &dmaAttributes, &ctx->DmaEnabler);
    if ((status == STATUS_NOT_SUPPORTED) && Prefer64Bit) {
        VIRTIO_DMA_TRACE(
            "profile=%s not supported (status=0x%08x); falling back to %s\n",
            VirtioDmaProfileName(profile),
            (unsigned)status,
            VirtioDmaProfileName(WdfDmaProfileScatterGatherDuplex));

        profile = WdfDmaProfileScatterGatherDuplex;
        WDF_DMA_ENABLER_CONFIG_INIT(&dmaConfig, profile, MaxTransferLength);
        status = WdfDmaEnablerCreate(Device, &dmaConfig, &dmaAttributes, &ctx->DmaEnabler);
    }
    if (!NT_SUCCESS(status)) {
        WdfObjectDelete(ctxObject);
        return status;
    }

    ctx->Profile = profile;
    ctx->Used64BitProfile = (profile == WdfDmaProfileScatterGather64Duplex) ? TRUE : FALSE;

    WdfDmaEnablerSetMaximumScatterGatherElements(ctx->DmaEnabler, MaxSgElements);

    VIRTIO_DMA_TRACE(
        "created profile=%s (%u) maxTransfer=%Iu maxSg=%lu\n",
        VirtioDmaProfileName(profile),
        (unsigned)profile,
        MaxTransferLength,
        MaxSgElements);

    *OutCtx = ctx;
    return STATUS_SUCCESS;
}

VOID VirtioDmaDestroy(_Inout_opt_ VIRTIO_DMA_CONTEXT** Ctx)
{
    if ((Ctx == NULL) || (*Ctx == NULL)) {
        return;
    }

    WDFOBJECT ctxObject = (*Ctx)->Object;
    *Ctx = NULL;

    WdfObjectDelete(ctxObject);
}

_Must_inspect_result_
NTSTATUS VirtioDmaAllocCommonBuffer(
    _In_ VIRTIO_DMA_CONTEXT* Ctx,
    _In_ size_t Length,
    _In_ size_t Alignment,
    _In_ BOOLEAN CacheEnabled,
    _Out_ VIRTIO_COMMON_BUFFER* Out)
{
    if (Ctx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioDmaAllocCommonBufferWithParent(Ctx, Length, Alignment, CacheEnabled, Ctx->Object, Out);
}

_Must_inspect_result_
NTSTATUS VirtioDmaAllocCommonBufferWithParent(
    _In_ VIRTIO_DMA_CONTEXT* Ctx,
    _In_ size_t Length,
    _In_ size_t Alignment,
    _In_ BOOLEAN CacheEnabled,
    _In_ WDFOBJECT ParentObject,
    _Out_ VIRTIO_COMMON_BUFFER* Out)
{
    NTSTATUS status;
    ULONG alignmentRequirement;

    if ((Ctx == NULL) || (Out == NULL) || (ParentObject == NULL)) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Out, sizeof(*Out));

    if (Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Alignment == 0) {
        alignmentRequirement = 0;
    } else {
        if ((Alignment > MAXULONG) || ((Alignment & (Alignment - 1)) != 0)) {
            return STATUS_INVALID_PARAMETER;
        }
        alignmentRequirement = (ULONG)Alignment;
    }

    WDF_COMMON_BUFFER_CONFIG cbConfig;
    WDF_COMMON_BUFFER_CONFIG_INIT(&cbConfig, alignmentRequirement);
    cbConfig.CacheEnabled = CacheEnabled;

    WDF_OBJECT_ATTRIBUTES cbAttributes;
    WDF_OBJECT_ATTRIBUTES_INIT(&cbAttributes);
    cbAttributes.ParentObject = ParentObject;

    status = WdfCommonBufferCreateWithConfig(
        Ctx->DmaEnabler,
        Length,
        &cbAttributes,
        &cbConfig,
        &Out->Handle);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    Out->Va = WdfCommonBufferGetAlignedVirtualAddress(Out->Handle);
    Out->Dma = VirtioDmaLogicalAddressToU64(WdfCommonBufferGetAlignedLogicalAddress(Out->Handle));
    Out->Length = Length;

    NT_ASSERT(Out->Va != NULL);
    if (Alignment != 0) {
        NT_ASSERT(((ULONG_PTR)Out->Va & (Alignment - 1)) == 0);
        NT_ASSERT((Out->Dma & (Alignment - 1)) == 0);
    }

    VIRTIO_DMA_TRACE(
        "alloc common buffer len=%Iu align=%lu cache=%u va=%p dma=0x%I64x\n",
        Length,
        alignmentRequirement,
        CacheEnabled ? 1U : 0U,
        Out->Va,
        (unsigned long long)Out->Dma);

    return STATUS_SUCCESS;
}

VOID VirtioDmaFreeCommonBuffer(_Inout_ VIRTIO_COMMON_BUFFER* Buffer)
{
    if (Buffer == NULL) {
        return;
    }

    if (Buffer->Handle != NULL) {
        WdfObjectDelete(Buffer->Handle);
    }

    RtlZeroMemory(Buffer, sizeof(*Buffer));
}
