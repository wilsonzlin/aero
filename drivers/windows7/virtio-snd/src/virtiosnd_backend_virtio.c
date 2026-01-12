/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "trace.h"
#include "virtiosnd_backend.h"

#ifndef VIRTIO_CORE_USE_WDF
#define VIRTIO_CORE_USE_WDF 0
#endif
#if VIRTIO_CORE_USE_WDF
#error "virtiosnd_backend_virtio.c requires VIRTIO_CORE_USE_WDF=0 (WDM)"
#endif

#include "virtio_pci_modern.h"

#include "virtqueue_split_legacy.h"
#include "virtio_pci_intx_wdm.h"

#define VIRTIOSND_BACKEND_POOL_TAG 'BkSV'

#define VIRTIO_F_RING_INDIRECT_DESC (1ui64 << 28)
#define VIRTIO_F_RING_EVENT_IDX (1ui64 << 29)

#define VIRTIO_SND_QUEUE_CONTROL 0
#define VIRTIO_SND_QUEUE_TX 2

#define VIRTIO_SND_R_PCM_INFO 0x0100u
#define VIRTIO_SND_R_PCM_SET_PARAMS 0x0101u
#define VIRTIO_SND_R_PCM_PREPARE 0x0102u
#define VIRTIO_SND_R_PCM_RELEASE 0x0103u
#define VIRTIO_SND_R_PCM_START 0x0104u
#define VIRTIO_SND_R_PCM_STOP 0x0105u

#define VIRTIO_SND_S_OK 0x0000u
#define VIRTIO_SND_S_BAD_MSG 0x0001u
#define VIRTIO_SND_S_NOT_SUPP 0x0002u
#define VIRTIO_SND_S_IO_ERR 0x0003u

#define VIRTIO_SND_PCM_FMT_S16_LE 0x05u
#define VIRTIO_SND_PCM_RATE_48000 0x07u

#define VIRTIOSND_STREAM_ID 0u

#define VIRTIOSND_DEFAULT_TX_CONTEXTS 64u

#pragma pack(push, 1)
typedef struct _VIRTIO_SND_PCM_INFO_REQ {
    UINT32 code;
    UINT32 start_id;
    UINT32 count;
} VIRTIO_SND_PCM_INFO_REQ;

typedef struct _VIRTIO_SND_PCM_HDR {
    UINT32 code;
    UINT32 stream_id;
} VIRTIO_SND_PCM_HDR;

typedef struct _VIRTIO_SND_PCM_SET_PARAMS {
    UINT32 code;
    UINT32 stream_id;
    UINT32 buffer_bytes;
    UINT32 period_bytes;
    UINT32 features;
    UINT8 channels;
    UINT8 format;
    UINT8 rate;
    UINT8 padding;
} VIRTIO_SND_PCM_SET_PARAMS;

typedef struct _VIRTIO_SND_TX_HDR {
    UINT32 stream_id;
    UINT32 reserved;
} VIRTIO_SND_TX_HDR;

typedef struct _VIRTIO_SND_TX_RESP {
    UINT32 status;
    UINT32 latency_bytes;
} VIRTIO_SND_TX_RESP;
#pragma pack(pop)

typedef struct _VIRTIOSND_CTRL_COOKIE {
    KEVENT Event;
    volatile LONG Completed;
} VIRTIOSND_CTRL_COOKIE, *PVIRTIOSND_CTRL_COOKIE;

typedef struct _VIRTIOSND_TX_CONTEXT {
    LIST_ENTRY Link;

    virtio_dma_buffer_t Dma;
    UINT32 MaxPcmBytes;

    volatile VIRTIO_SND_TX_HDR* Hdr;
    volatile UCHAR* Pcm;
    volatile VIRTIO_SND_TX_RESP* Resp;

    UINT64 HdrPa;
    UINT64 RespPa;
} VIRTIOSND_TX_CONTEXT, *PVIRTIOSND_TX_CONTEXT;

struct _VIRTIOSND_BACKEND {
    PDEVICE_OBJECT DeviceObject;
    PDEVICE_OBJECT LowerDeviceObject;

    volatile LONG ShuttingDown;

    VIRTIO_PCI_MODERN_DEVICE Virtio;
    UINT64 NegotiatedFeatures;

    virtqueue_split_t ControlVq;
    virtio_dma_buffer_t ControlRing;
    KSPIN_LOCK ControlLock;

    virtqueue_split_t TxVq;
    virtio_dma_buffer_t TxRing;
    KSPIN_LOCK TxLock;

    PVIRTIOSND_TX_CONTEXT TxContexts;
    ULONG TxContextCount;
    ULONG TxFreeCount;
    LIST_ENTRY TxFreeList;
    ULONG TxMaxPcmBytes;

    ULONG BufferBytes;
    ULONG PeriodBytes;

    BOOLEAN StreamRunning;

    VIRTIO_INTX Intx;
    CM_PARTIAL_RESOURCE_DESCRIPTOR InterruptDesc;
    BOOLEAN InterruptDescPresent;

    volatile UINT16** NotifyAddrCache;
    USHORT NotifyAddrCacheCount;
};

static void*
VirtioSndOsAlloc(void* ctx, size_t size, virtio_os_alloc_flags_t flags)
{
    UNREFERENCED_PARAMETER(ctx);

    if (size == 0) {
        return NULL;
    }

    {
        POOL_TYPE poolType;
        PVOID p;

        poolType = (flags & VIRTIO_OS_ALLOC_PAGED) ? PagedPool : NonPagedPool;
        p = ExAllocatePoolWithTag(poolType, size, VIRTIOSND_BACKEND_POOL_TAG);
        if (p == NULL) {
            return NULL;
        }

        if (flags & VIRTIO_OS_ALLOC_ZERO) {
            RtlZeroMemory(p, size);
        }

        return p;
    }
}

static void
VirtioSndOsFree(void* ctx, void* ptr)
{
    UNREFERENCED_PARAMETER(ctx);

    if (ptr == NULL) {
        return;
    }

    ExFreePool(ptr);
}

static virtio_bool_t
VirtioSndOsAllocDma(void* ctx, size_t size, size_t alignment, virtio_dma_buffer_t* out)
{
    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;
    PVOID va;

    UNREFERENCED_PARAMETER(ctx);

    if (out == NULL || size == 0) {
        return VIRTIO_FALSE;
    }

    if (alignment == 0 || ((alignment & (alignment - 1u)) != 0)) {
        return VIRTIO_FALSE;
    }

    low.QuadPart = 0;
    high.QuadPart = ~0ull;
    boundary.QuadPart = 0;

    va = MmAllocateContiguousMemorySpecifyCache(size, low, high, boundary, MmNonCached);
    if (va == NULL) {
        return VIRTIO_FALSE;
    }

    RtlZeroMemory(va, size);

    out->vaddr = va;
    out->paddr = (UINT64)MmGetPhysicalAddress(va).QuadPart;
    out->size = size;

    if ((out->paddr & ((UINT64)alignment - 1u)) != 0) {
        MmFreeContiguousMemorySpecifyCache(va, size, MmNonCached);
        RtlZeroMemory(out, sizeof(*out));
        return VIRTIO_FALSE;
    }

    return VIRTIO_TRUE;
}

static void
VirtioSndOsFreeDma(void* ctx, virtio_dma_buffer_t* buf)
{
    UNREFERENCED_PARAMETER(ctx);

    if (buf == NULL || buf->vaddr == NULL || buf->size == 0) {
        return;
    }

    MmFreeContiguousMemorySpecifyCache(buf->vaddr, buf->size, MmNonCached);
    RtlZeroMemory(buf, sizeof(*buf));
}

static uint64_t
VirtioSndOsVirtToPhys(void* ctx, const void* vaddr)
{
    UNREFERENCED_PARAMETER(ctx);

    if (vaddr == NULL) {
        return 0;
    }

    return (uint64_t)MmGetPhysicalAddress((PVOID)vaddr).QuadPart;
}

static void
VirtioSndOsMb(void* ctx)
{
    UNREFERENCED_PARAMETER(ctx);
    KeMemoryBarrier();
}

static const virtio_os_ops_t g_VirtioSndOsOps = {
    VirtioSndOsAlloc,
    VirtioSndOsFree,
    VirtioSndOsAllocDma,
    VirtioSndOsFreeDma,
    VirtioSndOsVirtToPhys,
    NULL,
    VirtioSndOsMb,
    VirtioSndOsMb,
    VirtioSndOsMb,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
};

static NTSTATUS
VirtioSndStatusToNtStatus(_In_ UINT32 Status)
{
    switch (Status) {
    case VIRTIO_SND_S_OK:
        return STATUS_SUCCESS;
    case VIRTIO_SND_S_BAD_MSG:
        return STATUS_INVALID_PARAMETER;
    case VIRTIO_SND_S_NOT_SUPP:
        return STATUS_NOT_SUPPORTED;
    case VIRTIO_SND_S_IO_ERR:
        return STATUS_INVALID_DEVICE_STATE;
    default:
        return STATUS_IO_DEVICE_ERROR;
    }
}

static VOID
VirtioSndBackendDrainControlLocked(_Inout_ PVIRTIOSND_BACKEND Backend)
{
    void* cookie;
    uint32_t usedLen;

    for (;;) {
        for (;;) {
            cookie = NULL;
            usedLen = 0;

            if (virtqueue_split_pop_used(&Backend->ControlVq, &cookie, &usedLen) == VIRTIO_FALSE) {
                break;
            }

            if (cookie == NULL) {
                continue;
            }

            InterlockedExchange(&((PVIRTIOSND_CTRL_COOKIE)cookie)->Completed, 1);
            KeSetEvent(&((PVIRTIOSND_CTRL_COOKIE)cookie)->Event, IO_NO_INCREMENT, FALSE);
        }

        if (Backend->ControlVq.event_idx != VIRTIO_FALSE && Backend->ControlVq.used_event != NULL) {
            *((volatile uint16_t*)Backend->ControlVq.used_event) = Backend->ControlVq.last_used_idx;
            KeMemoryBarrier();

            if (Backend->ControlVq.used->idx == Backend->ControlVq.last_used_idx) {
                break;
            }

            continue;
        }

        break;
    }
}

static VOID
VirtioSndBackendDrainTxLocked(_Inout_ PVIRTIOSND_BACKEND Backend)
{
    void* cookie;
    uint32_t usedLen;

    for (;;) {
        for (;;) {
            PVIRTIOSND_TX_CONTEXT ctx;
            UINT32 respStatus;

            cookie = NULL;
            usedLen = 0;

            if (virtqueue_split_pop_used(&Backend->TxVq, &cookie, &usedLen) == VIRTIO_FALSE) {
                break;
            }

            ctx = (PVIRTIOSND_TX_CONTEXT)cookie;
            if (ctx == NULL) {
                continue;
            }

            respStatus = ctx->Resp->status;
            if (respStatus != VIRTIO_SND_S_OK) {
                VIRTIOSND_TRACE_ERROR("tx complete: status=%lu\n", respStatus);
            }

            InsertTailList(&Backend->TxFreeList, &ctx->Link);
            Backend->TxFreeCount++;
        }

        if (Backend->TxVq.event_idx != VIRTIO_FALSE && Backend->TxVq.used_event != NULL) {
            *((volatile uint16_t*)Backend->TxVq.used_event) = Backend->TxVq.last_used_idx;
            KeMemoryBarrier();

            if (Backend->TxVq.used->idx == Backend->TxVq.last_used_idx) {
                break;
            }

            continue;
        }

        break;
    }
}

static VOID
VirtioSndBackendIntxQueueWork(_Inout_ PVIRTIO_INTX Intx, _In_opt_ PVOID Cookie)
{
    KIRQL oldIrql;
    PVIRTIOSND_BACKEND backend;

    UNREFERENCED_PARAMETER(Intx);

    backend = (PVIRTIOSND_BACKEND)Cookie;
    if (backend == NULL) {
        return;
    }

    if (InterlockedCompareExchange(&backend->ShuttingDown, 0, 0) != 0) {
        return;
    }

    KeAcquireSpinLock(&backend->ControlLock, &oldIrql);
    VirtioSndBackendDrainControlLocked(backend);
    KeReleaseSpinLock(&backend->ControlLock, oldIrql);

    KeAcquireSpinLock(&backend->TxLock, &oldIrql);
    VirtioSndBackendDrainTxLocked(backend);
    KeReleaseSpinLock(&backend->TxLock, oldIrql);
}

static NTSTATUS
VirtioSndBackendConnectInterrupt(_Inout_ PVIRTIOSND_BACKEND Backend, _In_opt_ PCM_RESOURCE_LIST TranslatedResources)
{
    ULONG listIndex;
    BOOLEAN sawMessageInterrupt;

    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    Backend->InterruptDescPresent = FALSE;
    RtlZeroMemory(&Backend->InterruptDesc, sizeof(Backend->InterruptDesc));

    if (Backend->Virtio.IsrStatus == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (TranslatedResources == NULL || TranslatedResources->Count == 0) {
        return STATUS_NOT_FOUND;
    }

    sawMessageInterrupt = FALSE;

    for (listIndex = 0; listIndex < TranslatedResources->Count; listIndex++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc;
        ULONG count;
        ULONG i;

        count = TranslatedResources->List[listIndex].PartialResourceList.Count;
        desc = TranslatedResources->List[listIndex].PartialResourceList.PartialDescriptors;

        for (i = 0; i < count; i++) {
            KIRQL irql;
            KAFFINITY affinity;
            KINTERRUPT_MODE mode;

            if (desc[i].Type != CmResourceTypeInterrupt) {
                continue;
            }

            if (desc[i].Flags & CM_RESOURCE_INTERRUPT_MESSAGE) {
                sawMessageInterrupt = TRUE;
                continue;
            }

            Backend->InterruptDesc = desc[i];
            Backend->InterruptDescPresent = TRUE;

            irql = (KIRQL)Backend->InterruptDesc.u.Interrupt.Level;
            affinity = (KAFFINITY)Backend->InterruptDesc.u.Interrupt.Affinity;
            mode = (Backend->InterruptDesc.Flags & CM_RESOURCE_INTERRUPT_LATCHED) ? Latched : LevelSensitive;

            UNREFERENCED_PARAMETER(irql);
            UNREFERENCED_PARAMETER(affinity);
            UNREFERENCED_PARAMETER(mode);

            return VirtioIntxConnect(Backend->DeviceObject,
                                     &Backend->InterruptDesc,
                                     Backend->Virtio.IsrStatus,
                                     NULL,
                                     VirtioSndBackendIntxQueueWork,
                                     NULL,
                                     Backend,
                                     &Backend->Intx);
        }
    }

    return sawMessageInterrupt ? STATUS_NOT_SUPPORTED : STATUS_NOT_FOUND;
}

static NTSTATUS
VirtioSndBackendInitQueue(_Inout_ PVIRTIOSND_BACKEND Backend,
                          _In_ USHORT QueueIndex,
                          _Inout_ virtqueue_split_t* Vq,
                          _Inout_ virtio_dma_buffer_t* Ring)
{
    NTSTATUS status;
    USHORT qsz;
    int rc;
    virtio_bool_t eventIdx;
    ULONGLONG descPa;
    ULONGLONG availPa;
    ULONGLONG usedPa;

    qsz = 0;
    status = VirtioPciGetQueueSize(&Backend->Virtio, QueueIndex, &qsz);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (qsz == 0) {
        return STATUS_NOT_FOUND;
    }

    eventIdx = (Backend->NegotiatedFeatures & VIRTIO_F_RING_EVENT_IDX) ? VIRTIO_TRUE : VIRTIO_FALSE;

    rc = virtqueue_split_alloc_ring(&g_VirtioSndOsOps, Backend, qsz, PAGE_SIZE, eventIdx, Ring);
    if (rc != VIRTIO_OK) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    rc = virtqueue_split_init(Vq,
                              &g_VirtioSndOsOps,
                              Backend,
                              QueueIndex,
                              qsz,
                              PAGE_SIZE,
                              Ring,
                              eventIdx,
                              VIRTIO_FALSE,
                              0);
    if (rc != VIRTIO_OK) {
        virtqueue_split_free_ring(&g_VirtioSndOsOps, Backend, Ring);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    descPa = Ring->paddr + (ULONGLONG)((PUCHAR)Vq->desc - (PUCHAR)Ring->vaddr);
    availPa = Ring->paddr + (ULONGLONG)((PUCHAR)Vq->avail - (PUCHAR)Ring->vaddr);
    usedPa = Ring->paddr + (ULONGLONG)((PUCHAR)Vq->used - (PUCHAR)Ring->vaddr);

    status = VirtioPciSetupQueue(&Backend->Virtio, QueueIndex, descPa, availPa, usedPa);
    if (!NT_SUCCESS(status)) {
        virtqueue_split_destroy(Vq);
        virtqueue_split_free_ring(&g_VirtioSndOsOps, Backend, Ring);
        return status;
    }

    return STATUS_SUCCESS;
}

static VOID
VirtioSndBackendFreeTxPool(_Inout_ PVIRTIOSND_BACKEND Backend)
{
    ULONG i;

    if (Backend->TxContexts == NULL) {
        return;
    }

    for (i = 0; i < Backend->TxContextCount; i++) {
        virtio_dma_buffer_t dma = Backend->TxContexts[i].Dma;
        if (dma.vaddr != NULL) {
            VirtioSndOsFreeDma(Backend, &Backend->TxContexts[i].Dma);
        }
    }

    ExFreePool(Backend->TxContexts);
    Backend->TxContexts = NULL;
    Backend->TxContextCount = 0;
    Backend->TxFreeCount = 0;
    InitializeListHead(&Backend->TxFreeList);
    Backend->TxMaxPcmBytes = 0;
}

static NTSTATUS
VirtioSndBackendAllocTxPool(_Inout_ PVIRTIOSND_BACKEND Backend, _In_ ULONG MaxPcmBytes)
{
    ULONG ctxCount;
    ULONG i;
    SIZE_T dmaSize;

    if (MaxPcmBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    VirtioSndBackendFreeTxPool(Backend);

    ctxCount = VIRTIOSND_DEFAULT_TX_CONTEXTS;
    if (Backend->TxVq.queue_size != 0 && ctxCount > (ULONG)(Backend->TxVq.queue_size / 2)) {
        ctxCount = (ULONG)(Backend->TxVq.queue_size / 2);
    }
    if (ctxCount == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    Backend->TxContexts = (PVIRTIOSND_TX_CONTEXT)ExAllocatePoolWithTag(NonPagedPool,
                                                                       sizeof(VIRTIOSND_TX_CONTEXT) * ctxCount,
                                                                       VIRTIOSND_BACKEND_POOL_TAG);
    if (Backend->TxContexts == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(Backend->TxContexts, sizeof(VIRTIOSND_TX_CONTEXT) * ctxCount);

    Backend->TxContextCount = ctxCount;
    Backend->TxFreeCount = 0;
    InitializeListHead(&Backend->TxFreeList);

    dmaSize = sizeof(VIRTIO_SND_TX_HDR) + (SIZE_T)MaxPcmBytes + sizeof(VIRTIO_SND_TX_RESP);

    for (i = 0; i < ctxCount; i++) {
        PVIRTIOSND_TX_CONTEXT ctx;
        virtio_dma_buffer_t dma;

        ctx = &Backend->TxContexts[i];
        InitializeListHead(&ctx->Link);
        ctx->MaxPcmBytes = MaxPcmBytes;

        RtlZeroMemory(&dma, sizeof(dma));
        if (VirtioSndOsAllocDma(Backend, dmaSize, 16u, &dma) == VIRTIO_FALSE) {
            VirtioSndBackendFreeTxPool(Backend);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        ctx->Dma = dma;
        ctx->Hdr = (volatile VIRTIO_SND_TX_HDR*)dma.vaddr;
        ctx->Pcm = (volatile UCHAR*)((PUCHAR)dma.vaddr + sizeof(VIRTIO_SND_TX_HDR));
        ctx->Resp = (volatile VIRTIO_SND_TX_RESP*)((PUCHAR)dma.vaddr + sizeof(VIRTIO_SND_TX_HDR) + MaxPcmBytes);

        ctx->HdrPa = dma.paddr;
        ctx->RespPa = dma.paddr + sizeof(VIRTIO_SND_TX_HDR) + MaxPcmBytes;

        InsertTailList(&Backend->TxFreeList, &ctx->Link);
        Backend->TxFreeCount++;
    }

    Backend->TxMaxPcmBytes = MaxPcmBytes;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtioSndBackendControlSync(_Inout_ PVIRTIOSND_BACKEND Backend,
                            _In_reads_bytes_(ReqLen) const VOID* Req,
                            _In_ ULONG ReqLen,
                            _Out_writes_bytes_(RespLen) VOID* Resp,
                            _In_ ULONG RespLen)
{
    virtio_dma_buffer_t dma;
    SIZE_T respOff;
    virtio_sg_entry_t sg[2];
    VIRTIOSND_CTRL_COOKIE cookie;
    int rc;
    USHORT head;
    KIRQL oldIrql;
    LARGE_INTEGER timeout;
    virtio_bool_t needKick;
    NTSTATUS status;

    if (Req == NULL || Resp == NULL || ReqLen == 0 || RespLen == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (InterlockedCompareExchange(&Backend->ShuttingDown, 0, 0) != 0) {
        return STATUS_DEVICE_REMOVED;
    }

    RtlZeroMemory(&dma, sizeof(dma));

    respOff = (SIZE_T)((ReqLen + 3u) & ~3u);
    if (respOff + RespLen < respOff) {
        return STATUS_INVALID_PARAMETER;
    }

    if (VirtioSndOsAllocDma(Backend, respOff + RespLen, 16u, &dma) == VIRTIO_FALSE) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlCopyMemory(dma.vaddr, Req, ReqLen);
    RtlZeroMemory((PUCHAR)dma.vaddr + respOff, RespLen);

    KeInitializeEvent(&cookie.Event, NotificationEvent, FALSE);
    cookie.Completed = 0;

    sg[0].addr = dma.paddr;
    sg[0].len = ReqLen;
    sg[0].device_writes = VIRTIO_FALSE;
    sg[1].addr = dma.paddr + respOff;
    sg[1].len = RespLen;
    sg[1].device_writes = VIRTIO_TRUE;

    head = 0;
    needKick = VIRTIO_FALSE;

    KeAcquireSpinLock(&Backend->ControlLock, &oldIrql);
    rc = virtqueue_split_add_sg(&Backend->ControlVq, sg, (uint16_t)VIRTIO_ARRAY_SIZE(sg), &cookie, VIRTIO_FALSE, &head);
    if (rc == VIRTIO_OK) {
        needKick = virtqueue_split_kick_prepare(&Backend->ControlVq);
    }
    KeReleaseSpinLock(&Backend->ControlLock, oldIrql);

    if (rc != VIRTIO_OK) {
        VirtioSndOsFreeDma(Backend, &dma);
        return STATUS_DEVICE_BUSY;
    }

    if (needKick != VIRTIO_FALSE) {
        VirtioPciNotifyQueue(&Backend->Virtio, VIRTIO_SND_QUEUE_CONTROL);
    }

    timeout.QuadPart = -10 * 1000 * 10;

    for (;;) {
        if (InterlockedCompareExchange(&cookie.Completed, 0, 0) != 0) {
            break;
        }

        status = KeWaitForSingleObject(&cookie.Event, Executive, KernelMode, FALSE, &timeout);
        UNREFERENCED_PARAMETER(status);
        VirtioSndBackend_Service(Backend);
    }

    KeMemoryBarrier();
    RtlCopyMemory(Resp, (PUCHAR)dma.vaddr + respOff, RespLen);

    VirtioSndOsFreeDma(Backend, &dma);
    return STATUS_SUCCESS;
}

static VOID
VirtioSndBackendTryPcmInfo(_Inout_ PVIRTIOSND_BACKEND Backend)
{
    VIRTIO_SND_PCM_INFO_REQ req;
    struct {
        UINT32 status;
        UCHAR info[32];
    } resp;
    NTSTATUS st;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_INFO;
    req.start_id = VIRTIOSND_STREAM_ID;
    req.count = 1;

    RtlZeroMemory(&resp, sizeof(resp));
    st = VirtioSndBackendControlSync(Backend, &req, sizeof(req), &resp, sizeof(resp));
    if (!NT_SUCCESS(st)) {
        VIRTIOSND_TRACE_ERROR("PCM_INFO failed: 0x%08X\n", st);
        return;
    }

    if (resp.status != VIRTIO_SND_S_OK) {
        VIRTIOSND_TRACE_ERROR("PCM_INFO status=%lu\n", resp.status);
        return;
    }

    VIRTIOSND_TRACE("PCM_INFO ok\n");
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Create(PDEVICE_OBJECT DeviceObject,
                        PDEVICE_OBJECT LowerDeviceObject,
                        PCM_RESOURCE_LIST RawResources,
                        PCM_RESOURCE_LIST TranslatedResources,
                        PVIRTIOSND_BACKEND* BackendOut)
{
    PVIRTIOSND_BACKEND backend;
    NTSTATUS status;
    UINT64 negotiated;
    USHORT numQueues;
    SIZE_T cacheBytes;

    if (BackendOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *BackendOut = NULL;

    if (DeviceObject == NULL || LowerDeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    backend = (PVIRTIOSND_BACKEND)ExAllocatePoolWithTag(NonPagedPool, sizeof(*backend), VIRTIOSND_BACKEND_POOL_TAG);
    if (backend == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(backend, sizeof(*backend));

    backend->DeviceObject = DeviceObject;
    backend->LowerDeviceObject = LowerDeviceObject;
    backend->ShuttingDown = 0;
    RtlZeroMemory(&backend->Intx, sizeof(backend->Intx));
    RtlZeroMemory(&backend->InterruptDesc, sizeof(backend->InterruptDesc));
    backend->InterruptDescPresent = FALSE;
    backend->NotifyAddrCache = NULL;
    backend->NotifyAddrCacheCount = 0;
    backend->TxContexts = NULL;
    backend->TxContextCount = 0;
    backend->TxFreeCount = 0;
    InitializeListHead(&backend->TxFreeList);

    KeInitializeSpinLock(&backend->ControlLock);
    KeInitializeSpinLock(&backend->TxLock);

    status = VirtioPciModernInitWdm(DeviceObject, LowerDeviceObject, &backend->Virtio);
    if (!NT_SUCCESS(status)) {
        VirtioSndBackend_Destroy(backend);
        return status;
    }

    status = VirtioPciModernMapBarsWdm(&backend->Virtio, RawResources, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VirtioSndBackend_Destroy(backend);
        return status;
    }

    numQueues = VirtioPciGetNumQueues(&backend->Virtio);
    if (numQueues != 0) {
        cacheBytes = sizeof(volatile UINT16*) * (SIZE_T)numQueues;
        backend->NotifyAddrCache = (volatile UINT16**)ExAllocatePoolWithTag(NonPagedPool, cacheBytes, VIRTIOSND_BACKEND_POOL_TAG);
        if (backend->NotifyAddrCache == NULL) {
            VirtioSndBackend_Destroy(backend);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory((PVOID)backend->NotifyAddrCache, cacheBytes);
        backend->NotifyAddrCacheCount = numQueues;

        backend->Virtio.QueueNotifyAddrCache = backend->NotifyAddrCache;
        backend->Virtio.QueueNotifyAddrCacheCount = numQueues;
    }

    negotiated = 0;
    status = VirtioPciNegotiateFeatures(&backend->Virtio,
                                        VIRTIO_F_RING_INDIRECT_DESC,
                                        VIRTIO_F_RING_EVENT_IDX,
                                        &negotiated);
    if (!NT_SUCCESS(status)) {
        VirtioSndBackend_Destroy(backend);
        return status;
    }

    backend->NegotiatedFeatures = negotiated;

    status = VirtioSndBackendInitQueue(backend, VIRTIO_SND_QUEUE_CONTROL, &backend->ControlVq, &backend->ControlRing);
    if (!NT_SUCCESS(status)) {
        VirtioSndBackend_Destroy(backend);
        return status;
    }

    status = VirtioSndBackendInitQueue(backend, VIRTIO_SND_QUEUE_TX, &backend->TxVq, &backend->TxRing);
    if (!NT_SUCCESS(status)) {
        VirtioSndBackend_Destroy(backend);
        return status;
    }

    status = VirtioSndBackendConnectInterrupt(backend, TranslatedResources);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("interrupt not connected: 0x%08X (polling only)\n", status);
    }

    VirtioPciAddStatus(&backend->Virtio, VIRTIO_STATUS_DRIVER_OK);

    VirtioSndBackendTryPcmInfo(backend);

    *BackendOut = backend;
    VIRTIOSND_TRACE("backend ready: features=0x%I64x\n", negotiated);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
VOID
VirtioSndBackend_Destroy(PVIRTIOSND_BACKEND Backend)
{
    ULONG cacheBytes;

    if (Backend == NULL) {
        return;
    }

    InterlockedExchange(&Backend->ShuttingDown, 1);

    VirtioIntxDisconnect(&Backend->Intx);

    if (Backend->Virtio.CommonCfg != NULL) {
        VirtioPciResetDevice(&Backend->Virtio);
    }

    VirtioSndBackendFreeTxPool(Backend);

    virtqueue_split_destroy(&Backend->TxVq);
    virtqueue_split_free_ring(&g_VirtioSndOsOps, Backend, &Backend->TxRing);

    virtqueue_split_destroy(&Backend->ControlVq);
    virtqueue_split_free_ring(&g_VirtioSndOsOps, Backend, &Backend->ControlRing);

    VirtioPciModernUninit(&Backend->Virtio);

    if (Backend->NotifyAddrCache != NULL) {
        cacheBytes = (ULONG)(sizeof(volatile UINT16*) * (SIZE_T)Backend->NotifyAddrCacheCount);
        UNREFERENCED_PARAMETER(cacheBytes);
        ExFreePool((PVOID)Backend->NotifyAddrCache);
        Backend->NotifyAddrCache = NULL;
        Backend->NotifyAddrCacheCount = 0;
    }

    ExFreePool(Backend);
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_SetParams(PVIRTIOSND_BACKEND Backend, ULONG BufferBytes, ULONG PeriodBytes)
{
    NTSTATUS status;
    VIRTIO_SND_PCM_SET_PARAMS req;
    UINT32 respStatus;

    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    BufferBytes &= ~3u;
    PeriodBytes &= ~3u;

    if (PeriodBytes == 0 || BufferBytes == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndBackendAllocTxPool(Backend, PeriodBytes);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    Backend->BufferBytes = BufferBytes;
    Backend->PeriodBytes = PeriodBytes;

    RtlZeroMemory(&req, sizeof(req));
    req.code = VIRTIO_SND_R_PCM_SET_PARAMS;
    req.stream_id = VIRTIOSND_STREAM_ID;
    req.buffer_bytes = BufferBytes;
    req.period_bytes = PeriodBytes;
    req.features = 0;
    req.channels = 2;
    req.format = VIRTIO_SND_PCM_FMT_S16_LE;
    req.rate = VIRTIO_SND_PCM_RATE_48000;
    req.padding = 0;

    respStatus = 0;
    status = VirtioSndBackendControlSync(Backend, &req, sizeof(req), &respStatus, sizeof(respStatus));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioSndStatusToNtStatus(respStatus);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PCM_SET_PARAMS failed: %lu\n", respStatus);
    }
    return status;
}

static NTSTATUS
VirtioSndBackendSimplePcmCmd(_Inout_ PVIRTIOSND_BACKEND Backend, _In_ UINT32 Code)
{
    VIRTIO_SND_PCM_HDR req;
    UINT32 respStatus;
    NTSTATUS status;

    RtlZeroMemory(&req, sizeof(req));
    req.code = Code;
    req.stream_id = VIRTIOSND_STREAM_ID;

    respStatus = 0;
    status = VirtioSndBackendControlSync(Backend, &req, sizeof(req), &respStatus, sizeof(respStatus));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioSndStatusToNtStatus(respStatus);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PCM cmd 0x%lx failed: %lu\n", Code, respStatus);
    }
    return status;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Prepare(PVIRTIOSND_BACKEND Backend)
{
    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioSndBackendSimplePcmCmd(Backend, VIRTIO_SND_R_PCM_PREPARE);
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Start(PVIRTIOSND_BACKEND Backend)
{
    NTSTATUS status;

    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndBackendSimplePcmCmd(Backend, VIRTIO_SND_R_PCM_START);
    if (NT_SUCCESS(status)) {
        Backend->StreamRunning = TRUE;
    }
    return status;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Stop(PVIRTIOSND_BACKEND Backend)
{
    NTSTATUS status;

    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndBackendSimplePcmCmd(Backend, VIRTIO_SND_R_PCM_STOP);
    Backend->StreamRunning = FALSE;
    return status;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Release(PVIRTIOSND_BACKEND Backend)
{
    NTSTATUS status;

    if (Backend == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = VirtioSndBackendSimplePcmCmd(Backend, VIRTIO_SND_R_PCM_RELEASE);
    Backend->StreamRunning = FALSE;
    return status;
}

_Use_decl_annotations_
NTSTATUS
VirtioSndBackend_Write(PVIRTIOSND_BACKEND Backend, const VOID* Pcm, ULONG Bytes, PULONG BytesWritten)
{
    KIRQL oldIrql;
    ULONG remaining;
    ULONG submitted;
    const UCHAR* src;
    virtio_bool_t needKick;
    int rc;

    if (BytesWritten == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *BytesWritten = 0;

    if (Backend == NULL || Pcm == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (InterlockedCompareExchange(&Backend->ShuttingDown, 0, 0) != 0) {
        return STATUS_DEVICE_REMOVED;
    }

    if (!Backend->StreamRunning || Backend->TxContexts == NULL || Backend->TxMaxPcmBytes == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    Bytes &= ~3u;
    if (Bytes == 0) {
        return STATUS_SUCCESS;
    }

    src = (const UCHAR*)Pcm;
    remaining = Bytes;
    submitted = 0;
    needKick = VIRTIO_FALSE;

    KeAcquireSpinLock(&Backend->TxLock, &oldIrql);

    VirtioSndBackendDrainTxLocked(Backend);

    while (remaining != 0 && Backend->TxFreeCount != 0) {
        LIST_ENTRY* entry;
        PVIRTIOSND_TX_CONTEXT ctx;
        ULONG chunk;
        virtio_sg_entry_t sg[2];
        USHORT head;

        entry = RemoveHeadList(&Backend->TxFreeList);
        Backend->TxFreeCount--;
        ctx = CONTAINING_RECORD(entry, VIRTIOSND_TX_CONTEXT, Link);

        chunk = remaining;
        if (chunk > Backend->TxMaxPcmBytes) {
            chunk = Backend->TxMaxPcmBytes;
        }

        ctx->Hdr->stream_id = VIRTIOSND_STREAM_ID;
        ctx->Hdr->reserved = 0;
        RtlCopyMemory((PVOID)ctx->Pcm, src, chunk);
        ctx->Resp->status = 0xFFFFFFFFu;
        ctx->Resp->latency_bytes = 0;

        sg[0].addr = ctx->HdrPa;
        sg[0].len = (uint32_t)(sizeof(VIRTIO_SND_TX_HDR) + chunk);
        sg[0].device_writes = VIRTIO_FALSE;
        sg[1].addr = ctx->RespPa;
        sg[1].len = sizeof(VIRTIO_SND_TX_RESP);
        sg[1].device_writes = VIRTIO_TRUE;

        head = 0;
        rc = virtqueue_split_add_sg(&Backend->TxVq, sg, (uint16_t)VIRTIO_ARRAY_SIZE(sg), ctx, VIRTIO_FALSE, &head);
        if (rc != VIRTIO_OK) {
            InsertHeadList(&Backend->TxFreeList, &ctx->Link);
            Backend->TxFreeCount++;
            break;
        }

        submitted += chunk;
        remaining -= chunk;
        src += chunk;
    }

    if (submitted != 0) {
        needKick = virtqueue_split_kick_prepare(&Backend->TxVq);
    }

    KeReleaseSpinLock(&Backend->TxLock, oldIrql);

    if (submitted != 0 && needKick != VIRTIO_FALSE) {
        VirtioPciNotifyQueue(&Backend->Virtio, VIRTIO_SND_QUEUE_TX);
    }

    *BytesWritten = submitted;
    return (submitted != 0) ? STATUS_SUCCESS : STATUS_DEVICE_BUSY;
}

_Use_decl_annotations_
VOID
VirtioSndBackend_Service(PVIRTIOSND_BACKEND Backend)
{
    KIRQL oldIrql;

    if (Backend == NULL) {
        return;
    }

    if (InterlockedCompareExchange(&Backend->ShuttingDown, 0, 0) != 0) {
        return;
    }

    KeAcquireSpinLock(&Backend->ControlLock, &oldIrql);
    VirtioSndBackendDrainControlLocked(Backend);
    KeReleaseSpinLock(&Backend->ControlLock, oldIrql);

    KeAcquireSpinLock(&Backend->TxLock, &oldIrql);
    VirtioSndBackendDrainTxLocked(Backend);
    KeReleaseSpinLock(&Backend->TxLock, oldIrql);
}
