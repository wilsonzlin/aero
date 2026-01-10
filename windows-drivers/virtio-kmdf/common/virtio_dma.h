#pragma once

#include <ntddk.h>
#include <wdf.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__cplusplus)
#define VIRTIO_STATIC_ASSERT(expr, msg) static_assert((expr), msg)
#else
#define VIRTIO_STATIC_ASSERT(expr, msg) C_ASSERT(expr)
#endif

#if DBG
#define VIRTIO_DMA_TRACE(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "virtio_dma: " __VA_ARGS__)
#else
#define VIRTIO_DMA_TRACE(...) ((void)0)
#endif

//
// Virtio rings use packed structures defined by the spec. Compile-time checks here
// prevent accidental padding changes if these types are shared by virtqueue code.
//
typedef struct _VIRTIO_VRING_DESC {
    UINT64 Addr;
    UINT32 Len;
    UINT16 Flags;
    UINT16 Next;
} VIRTIO_VRING_DESC;

VIRTIO_STATIC_ASSERT(sizeof(VIRTIO_VRING_DESC) == 16, "vring desc must be 16 bytes");
VIRTIO_STATIC_ASSERT(FIELD_OFFSET(VIRTIO_VRING_DESC, Addr) == 0, "Addr offset");
VIRTIO_STATIC_ASSERT(FIELD_OFFSET(VIRTIO_VRING_DESC, Len) == 8, "Len offset");
VIRTIO_STATIC_ASSERT(FIELD_OFFSET(VIRTIO_VRING_DESC, Flags) == 12, "Flags offset");
VIRTIO_STATIC_ASSERT(FIELD_OFFSET(VIRTIO_VRING_DESC, Next) == 14, "Next offset");

typedef struct _VIRTIO_VRING_USED_ELEM {
    UINT32 Id;
    UINT32 Len;
} VIRTIO_VRING_USED_ELEM;

VIRTIO_STATIC_ASSERT(sizeof(VIRTIO_VRING_USED_ELEM) == 8, "vring used elem must be 8 bytes");

typedef struct _VIRTIO_COMMON_BUFFER {
    WDFCOMMONBUFFER Handle;
    PVOID Va;
    UINT64 Dma;
    size_t Length;
} VIRTIO_COMMON_BUFFER;

typedef struct _VIRTIO_DMA_CONTEXT {
    WDFOBJECT Object;
    WDFDMAENABLER DmaEnabler;

    WDF_DMA_PROFILE Profile;
    size_t MaxTransferLength;
    ULONG MaxScatterGatherElements;

    BOOLEAN Prefer64Bit;
    BOOLEAN Used64BitProfile;
} VIRTIO_DMA_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(VIRTIO_DMA_CONTEXT, VirtioDmaGetContext);

_Must_inspect_result_
NTSTATUS VirtioDmaCreate(
    _In_ WDFDEVICE Device,
    _In_ size_t MaxTransferLength,
    _In_ ULONG MaxSgElements,
    _In_ BOOLEAN Prefer64Bit,
    _Outptr_result_nullonfailure_ VIRTIO_DMA_CONTEXT** OutCtx);

VOID VirtioDmaDestroy(_Inout_opt_ VIRTIO_DMA_CONTEXT** Ctx);

_Must_inspect_result_
NTSTATUS VirtioDmaAllocCommonBuffer(
    _In_ VIRTIO_DMA_CONTEXT* Ctx,
    _In_ size_t Length,
    _In_ size_t Alignment,
    _In_ BOOLEAN CacheEnabled,
    _Out_ VIRTIO_COMMON_BUFFER* Out);

VOID VirtioDmaFreeCommonBuffer(_Inout_ VIRTIO_COMMON_BUFFER* Buffer);

__forceinline WDFDMAENABLER VirtioDmaGetEnabler(_In_ VIRTIO_DMA_CONTEXT* Ctx)
{
    NT_ASSERT(Ctx != NULL);
    return Ctx->DmaEnabler;
}

#ifdef __cplusplus
} // extern "C"
#endif

