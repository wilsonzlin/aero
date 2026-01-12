/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#if !defined(_KERNEL_MODE)
#error virtio-snd is a kernel-mode driver
#endif

#include <ntddk.h>

#include "trace.h"
#include "aero_virtio_snd_ioport.h"

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

#define VIRTIO_SND_D_OUTPUT 0x00u

#define VIRTIO_SND_PCM_FMT_S16 0x05u
#define VIRTIO_SND_PCM_RATE_48000 0x07u

typedef struct _VIRTIOSND_CONTROL_REQUEST {
  KEVENT Event;
  ULONG UsedLen;
  NTSTATUS CompletionStatus;
} VIRTIOSND_CONTROL_REQUEST, *PVIRTIOSND_CONTROL_REQUEST;

static __forceinline VOID VirtIoSndHwAddRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  InterlockedIncrement(&Dx->RefCount);
}

static __forceinline VOID VirtIoSndHwReleaseRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (InterlockedDecrement(&Dx->RefCount) == 0) {
    VirtIoSndHwStop(Dx);
  }
}

static BOOLEAN VirtIoSndInterruptIsr(_In_ PKINTERRUPT Interrupt, _In_ PVOID Context) {
  PAEROVIOSND_DEVICE_EXTENSION dx = (PAEROVIOSND_DEVICE_EXTENSION)Context;
  UCHAR isr;

  UNREFERENCED_PARAMETER(Interrupt);

  if (!dx || !dx->Started) {
    return FALSE;
  }

  isr = VirtioPciReadIsr(&dx->Vdev);
  if (isr == 0) {
    return FALSE;
  }

  KeInsertQueueDpc(&dx->InterruptDpc, NULL, NULL);
  return TRUE;
}

static VOID VirtIoSndInterruptDpc(_In_ PKDPC Dpc, _In_ PVOID DeferredContext, _In_ PVOID SystemArgument1, _In_ PVOID SystemArgument2) {
  PAEROVIOSND_DEVICE_EXTENSION dx = (PAEROVIOSND_DEVICE_EXTENSION)DeferredContext;
  USHORT head;
  ULONG len;
  PVOID ctx;
  KIRQL oldIrql;

  UNREFERENCED_PARAMETER(Dpc);
  UNREFERENCED_PARAMETER(SystemArgument1);
  UNREFERENCED_PARAMETER(SystemArgument2);

  if (!dx || !dx->Started) {
    return;
  }

  KeAcquireSpinLock(&dx->Lock, &oldIrql);

  while (VirtioQueuePopUsed(&dx->ControlVq, &head, &len, &ctx)) {
    PVIRTIOSND_CONTROL_REQUEST req = (PVIRTIOSND_CONTROL_REQUEST)ctx;
    if (req) {
      req->UsedLen = len;
      req->CompletionStatus = STATUS_SUCCESS;
      KeSetEvent(&req->Event, IO_NO_INCREMENT, FALSE);
    }
  }

  while (VirtioQueuePopUsed(&dx->TxVq, &head, &len, &ctx)) {
    PAEROVIOSND_TX_ENTRY entry = (PAEROVIOSND_TX_ENTRY)ctx;
    UNREFERENCED_PARAMETER(len);
    if (entry) {
      // Return the entry to the free list.
      RemoveEntryList(&entry->Link);
      InsertTailList(&dx->TxFreeList, &entry->Link);
    }
  }

  KeReleaseSpinLock(&dx->Lock, oldIrql);
}

static VOID VirtIoSndFreeTxPool(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (Dx->TxEntries) {
    ExFreePoolWithTag(Dx->TxEntries, VIRTIOSND_POOL_TAG);
    Dx->TxEntries = NULL;
  }
  Dx->TxEntryCount = 0;

  if (Dx->TxBufferVa) {
    NT_ASSERT(Dx->TxBufferBytes != 0);
    if (Dx->TxBufferBytes != 0) {
      MmFreeContiguousMemorySpecifyCache(Dx->TxBufferVa, Dx->TxBufferBytes, MmCached);
    }
    Dx->TxBufferVa = NULL;
  }
  Dx->TxBufferPa.QuadPart = 0;
  Dx->TxBufferBytes = 0;

  InitializeListHead(&Dx->TxFreeList);
  InitializeListHead(&Dx->TxSubmittedList);
}

static VOID VirtIoSndFreeControlBuffer(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (Dx->ControlBufferVa) {
    NT_ASSERT(Dx->ControlBufferBytes != 0);
    if (Dx->ControlBufferBytes != 0) {
      MmFreeContiguousMemorySpecifyCache(Dx->ControlBufferVa, Dx->ControlBufferBytes, MmCached);
    }
    Dx->ControlBufferVa = NULL;
  }
  Dx->ControlBufferPa.QuadPart = 0;
  Dx->ControlBufferBytes = 0;
}

static NTSTATUS VirtIoSndSendControl(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx,
                                    _In_reads_bytes_(ReqBytes) const VOID* Req,
                                    _In_ ULONG ReqBytes,
                                    _Out_writes_bytes_(RespBytes) VOID* Resp,
                                    _In_ ULONG RespBytes) {
  PVIRTIOSND_CONTROL_REQUEST ctx;
  VIRTIO_SG_ENTRY sg[2];
  USHORT head;
  NTSTATUS status;
  KIRQL oldIrql;

  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }
  if (!Req || ReqBytes == 0 || !Resp || RespBytes == 0) {
    return STATUS_INVALID_PARAMETER;
  }
  if (Dx->ControlBufferVa == NULL || Dx->ControlBufferBytes < (ReqBytes + RespBytes)) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
    return STATUS_INVALID_DEVICE_STATE;
  }

  // Serialize access to the shared control DMA buffer so we never have multiple
  // control requests in flight (avoids buffer overwrite races).
  status = KeWaitForSingleObject(&Dx->ControlMutex, Executive, KernelMode, FALSE, NULL);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  ctx = (PVIRTIOSND_CONTROL_REQUEST)ExAllocatePoolWithTag(NonPagedPool, sizeof(*ctx), VIRTIOSND_POOL_TAG);
  if (!ctx) {
    KeReleaseMutex(&Dx->ControlMutex, FALSE);
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  RtlZeroMemory(ctx, sizeof(*ctx));
  KeInitializeEvent(&ctx->Event, NotificationEvent, FALSE);
  ctx->UsedLen = 0;
  ctx->CompletionStatus = STATUS_PENDING;

  RtlCopyMemory(Dx->ControlBufferVa, Req, ReqBytes);
  RtlZeroMemory(Dx->ControlBufferVa + ReqBytes, RespBytes);

  sg[0].Address = Dx->ControlBufferPa;
  sg[0].Length = ReqBytes;
  sg[0].Write = FALSE;

  sg[1].Address = Dx->ControlBufferPa;
  sg[1].Address.QuadPart += ReqBytes;
  sg[1].Length = RespBytes;
  sg[1].Write = TRUE;

  KeAcquireSpinLock(&Dx->Lock, &oldIrql);
  status = VirtioQueueAddBuffer(&Dx->ControlVq, sg, 2, ctx, &head);
  if (NT_SUCCESS(status)) {
    VirtioQueueNotify(&Dx->Vdev, &Dx->ControlVq);
  }
  KeReleaseSpinLock(&Dx->Lock, oldIrql);

  if (!NT_SUCCESS(status)) {
    ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
    KeReleaseMutex(&Dx->ControlMutex, FALSE);
    return status;
  }

  status = KeWaitForSingleObject(&ctx->Event, Executive, KernelMode, FALSE, NULL);
  if (NT_SUCCESS(status)) {
    RtlCopyMemory(Resp, Dx->ControlBufferVa + ReqBytes, RespBytes);
  }
  ExFreePoolWithTag(ctx, VIRTIOSND_POOL_TAG);
  KeReleaseMutex(&Dx->ControlMutex, FALSE);
  return status;
}

static NTSTATUS VirtIoSndControlSimple(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx, _In_ ULONG Code, _In_ ULONG StreamId) {
  ULONG req[2];
  ULONG resp;
  NTSTATUS status;

  req[0] = Code;
  req[1] = StreamId;

  resp = 0;
  status = VirtIoSndSendControl(Dx, req, sizeof(req), &resp, sizeof(resp));
  if (!NT_SUCCESS(status)) {
    return status;
  }

  if (resp != VIRTIO_SND_S_OK) {
    return STATUS_UNSUCCESSFUL;
  }

  return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndQueryPcmInfo(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  ULONG req[3];
  UCHAR resp[4 + 32];
  ULONG status;

  RtlZeroMemory(resp, sizeof(resp));

  req[0] = VIRTIO_SND_R_PCM_INFO;
  req[1] = 0; // start_id
  req[2] = 1; // count

  {
    NTSTATUS st = VirtIoSndSendControl(Dx, req, sizeof(req), resp, sizeof(resp));
    if (!NT_SUCCESS(st)) {
      return st;
    }
  }

  status = *(ULONG*)&resp[0];
  if (status != VIRTIO_SND_S_OK) {
    return STATUS_UNSUCCESSFUL;
  }

  // Validate the single info entry returned by the in-tree device model.
  {
    const ULONG stream_id = *(ULONG*)&resp[4];
    const UCHAR direction = resp[4 + 24];
    const UCHAR ch_min = resp[4 + 25];
    const UCHAR ch_max = resp[4 + 26];

    if (stream_id != VIRTIOSND_STREAM_ID_PLAYBACK) {
      return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if (direction != VIRTIO_SND_D_OUTPUT) {
      return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if (ch_min != VIRTIOSND_CHANNELS || ch_max != VIRTIOSND_CHANNELS) {
      return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
  }

  return STATUS_SUCCESS;
}

static NTSTATUS VirtIoSndAllocateTxPool(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  PHYSICAL_ADDRESS low = {0};
  PHYSICAL_ADDRESS high;
  PHYSICAL_ADDRESS skip = {0};
  ULONG i;
  ULONG entryBytes;
  ULONG maxEntries;

  high.QuadPart = ~0ull;

  maxEntries = (ULONG)(Dx->TxVq.QueueSize / 2);
  if (maxEntries == 0) {
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }

  Dx->TxEntryCount = maxEntries;
  if (Dx->TxEntryCount > 64) {
    Dx->TxEntryCount = 64;
  }

  Dx->TxEntries = (AEROVIOSND_TX_ENTRY*)ExAllocatePoolWithTag(NonPagedPool, sizeof(AEROVIOSND_TX_ENTRY) * Dx->TxEntryCount, VIRTIOSND_POOL_TAG);
  if (!Dx->TxEntries) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  RtlZeroMemory(Dx->TxEntries, sizeof(AEROVIOSND_TX_ENTRY) * Dx->TxEntryCount);

  entryBytes = 8 + Dx->PeriodBytes + 8;
  Dx->TxBufferBytes = entryBytes * Dx->TxEntryCount;

  Dx->TxBufferVa = (PUCHAR)MmAllocateContiguousMemorySpecifyCache(Dx->TxBufferBytes, low, high, skip, MmCached);
  if (!Dx->TxBufferVa) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  Dx->TxBufferPa = MmGetPhysicalAddress(Dx->TxBufferVa);
  RtlZeroMemory(Dx->TxBufferVa, Dx->TxBufferBytes);

  InitializeListHead(&Dx->TxFreeList);
  InitializeListHead(&Dx->TxSubmittedList);

  for (i = 0; i < Dx->TxEntryCount; i++) {
    PAEROVIOSND_TX_ENTRY entry = &Dx->TxEntries[i];
    entry->BufferVa = Dx->TxBufferVa + (entryBytes * i);
    entry->BufferPa.QuadPart = Dx->TxBufferPa.QuadPart + (entryBytes * i);
    entry->PayloadBytes = Dx->PeriodBytes;
    entry->HeadId = 0;
    InsertTailList(&Dx->TxFreeList, &entry->Link);
  }

  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwStart(PAEROVIOSND_DEVICE_EXTENSION Dx, PIRP StartIrp) {
  NTSTATUS status;
  PIO_STACK_LOCATION stack;
  PCM_RESOURCE_LIST translated;
  ULONG i;
  PHYSICAL_ADDRESS low = {0};
  PHYSICAL_ADDRESS high;
  PHYSICAL_ADDRESS skip = {0};
  UCHAR devStatus;

  if (!Dx || !StartIrp) {
    return STATUS_INVALID_PARAMETER;
  }

  high.QuadPart = ~0ull;

  KeInitializeSpinLock(&Dx->Lock);
  KeInitializeMutex(&Dx->ControlMutex, 0);
  Dx->InterruptObject = NULL;
  Dx->Started = FALSE;
  Dx->RefCount = 0;

  Dx->IoPortStart = 0;
  Dx->IoBase = NULL;
  Dx->IoLength = 0;

  Dx->InterruptVector = 0;
  Dx->InterruptIrql = 0;
  Dx->InterruptAffinity = 0;
  Dx->InterruptMode = LevelSensitive;

  Dx->ControlBufferVa = NULL;
  Dx->ControlBufferPa.QuadPart = 0;
  Dx->ControlBufferBytes = 0;

  Dx->TxEntries = NULL;
  Dx->TxEntryCount = 0;
  Dx->TxBufferVa = NULL;
  Dx->TxBufferPa.QuadPart = 0;
  Dx->TxBufferBytes = 0;
  InitializeListHead(&Dx->TxFreeList);
  InitializeListHead(&Dx->TxSubmittedList);

  Dx->BufferBytes = VIRTIOSND_DEFAULT_BUFFER_BYTES;
  Dx->PeriodBytes = VIRTIOSND_DEFAULT_PERIOD_BYTES;
  Dx->PcmState = VirtIoSndPcmIdle;

  stack = IoGetCurrentIrpStackLocation(StartIrp);
  translated = stack->Parameters.StartDevice.AllocatedResourcesTranslated;
  if (!translated || translated->Count < 1) {
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }

  for (i = 0; i < translated->List[0].PartialResourceList.Count; i++) {
    PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = &translated->List[0].PartialResourceList.PartialDescriptors[i];
    if (desc->Type == CmResourceTypePort && Dx->IoLength == 0) {
      Dx->IoPortStart = (ULONG)desc->u.Port.Start.QuadPart;
      Dx->IoLength = desc->u.Port.Length;
      Dx->IoBase = (PUCHAR)(ULONG_PTR)desc->u.Port.Start.QuadPart;
    } else if (desc->Type == CmResourceTypeInterrupt && Dx->InterruptVector == 0) {
      Dx->InterruptVector = desc->u.Interrupt.Vector;
      Dx->InterruptIrql = (KIRQL)desc->u.Interrupt.Level;
      Dx->InterruptAffinity = (KAFFINITY)desc->u.Interrupt.Affinity;
      Dx->InterruptMode = (desc->Flags & CM_RESOURCE_INTERRUPT_LATCHED) ? Latched : LevelSensitive;
    }
  }

  if (Dx->IoLength == 0 || Dx->IoBase == NULL) {
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }
  if (Dx->InterruptVector == 0) {
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }
  if (Dx->InterruptAffinity == 0) {
    Dx->InterruptAffinity = (KAFFINITY)~0ull;
  }

  KeInitializeDpc(&Dx->InterruptDpc, VirtIoSndInterruptDpc, Dx);

  status = IoConnectInterrupt(&Dx->InterruptObject,
                              VirtIoSndInterruptIsr,
                              Dx,
                              NULL,
                              Dx->InterruptVector,
                              Dx->InterruptIrql,
                              Dx->InterruptIrql,
                              Dx->InterruptMode,
                              TRUE,
                              Dx->InterruptAffinity,
                              FALSE);
  if (!NT_SUCCESS(status)) {
    Dx->InterruptObject = NULL;
    return status;
  }

  VirtioPciInitialize(&Dx->Vdev, Dx->IoBase, Dx->IoLength, FALSE);

  VirtioPciReset(&Dx->Vdev);
  VirtioPciAddStatus(&Dx->Vdev, VIRTIO_STATUS_ACKNOWLEDGE);
  VirtioPciAddStatus(&Dx->Vdev, VIRTIO_STATUS_DRIVER);

  Dx->HostFeatures = VirtioPciReadHostFeatures(&Dx->Vdev);
  Dx->NegotiatedFeatures = Dx->HostFeatures & VIRTIO_F_ANY_LAYOUT;
  VirtioPciWriteGuestFeatures(&Dx->Vdev, Dx->NegotiatedFeatures);

  VirtioPciAddStatus(&Dx->Vdev, VIRTIO_STATUS_FEATURES_OK);
  devStatus = VirtioPciGetStatus(&Dx->Vdev);
  if ((devStatus & VIRTIO_STATUS_FEATURES_OK) == 0) {
    VirtioPciAddStatus(&Dx->Vdev, VIRTIO_STATUS_FAILED);
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }

  status = VirtioQueueCreate(&Dx->Vdev, &Dx->ControlVq, VIRTIOSND_QUEUE_CONTROL);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  status = VirtioQueueCreate(&Dx->Vdev, &Dx->TxVq, VIRTIOSND_QUEUE_TX);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  Dx->ControlBufferBytes = 512;
  Dx->ControlBufferVa = (PUCHAR)MmAllocateContiguousMemorySpecifyCache(Dx->ControlBufferBytes, low, high, skip, MmCached);
  if (!Dx->ControlBufferVa) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  Dx->ControlBufferPa = MmGetPhysicalAddress(Dx->ControlBufferVa);
  RtlZeroMemory(Dx->ControlBufferVa, Dx->ControlBufferBytes);

  status = VirtIoSndAllocateTxPool(Dx);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  VirtioPciAddStatus(&Dx->Vdev, VIRTIO_STATUS_DRIVER_OK);

  Dx->Started = TRUE;

  status = VirtIoSndQueryPcmInfo(Dx);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  status = VirtIoSndHwSetPcmParams(Dx, Dx->BufferBytes, Dx->PeriodBytes);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  status = VirtIoSndControlSimple(Dx, VIRTIO_SND_R_PCM_PREPARE, VIRTIOSND_STREAM_ID_PLAYBACK);
  if (!NT_SUCCESS(status)) {
    return status;
  }
  Dx->PcmState = VirtIoSndPcmPrepared;

  VIRTIOSND_TRACE("virtio-snd started host_features=0x%08lx negotiated=0x%08lx\n", Dx->HostFeatures, Dx->NegotiatedFeatures);
  return STATUS_SUCCESS;
}

_Use_decl_annotations_ VOID VirtIoSndHwStop(PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (!Dx) {
    return;
  }

  // Best-effort control-plane teardown.
  if (Dx->Started && KeGetCurrentIrql() == PASSIVE_LEVEL) {
    (void)VirtIoSndHwStopPcm(Dx);
    (void)VirtIoSndHwReleasePcm(Dx);
  }

  Dx->Started = FALSE;

  if (Dx->Vdev.IoBase) {
    VirtioPciReset(&Dx->Vdev);
  }

  if (Dx->InterruptObject) {
    IoDisconnectInterrupt(Dx->InterruptObject);
    Dx->InterruptObject = NULL;
  }

  if (Dx->ControlVq.RingVa) {
    VirtioQueueDelete(&Dx->Vdev, &Dx->ControlVq);
  }
  if (Dx->TxVq.RingVa) {
    VirtioQueueDelete(&Dx->Vdev, &Dx->TxVq);
  }

  VirtIoSndFreeTxPool(Dx);
  VirtIoSndFreeControlBuffer(Dx);
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwSetPcmParams(PAEROVIOSND_DEVICE_EXTENSION Dx, ULONG BufferBytes, ULONG PeriodBytes) {
  UCHAR req[24];
  ULONG resp;
  NTSTATUS status;

  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  if (Dx->PcmState == VirtIoSndPcmRunning) {
    return STATUS_INVALID_DEVICE_STATE;
  }

  RtlZeroMemory(req, sizeof(req));

  *(ULONG*)&req[0] = VIRTIO_SND_R_PCM_SET_PARAMS;
  *(ULONG*)&req[4] = VIRTIOSND_STREAM_ID_PLAYBACK;
  *(ULONG*)&req[8] = BufferBytes;
  *(ULONG*)&req[12] = PeriodBytes;
  *(ULONG*)&req[16] = 0; // features
  req[20] = VIRTIOSND_CHANNELS;
  req[21] = VIRTIO_SND_PCM_FMT_S16;
  req[22] = VIRTIO_SND_PCM_RATE_48000;
  req[23] = 0;

  resp = 0;
  status = VirtIoSndSendControl(Dx, req, sizeof(req), &resp, sizeof(resp));
  if (!NT_SUCCESS(status)) {
    return status;
  }
  if (resp != VIRTIO_SND_S_OK) {
    return STATUS_UNSUCCESSFUL;
  }

  Dx->BufferBytes = BufferBytes;
  Dx->PeriodBytes = PeriodBytes;
  Dx->PcmState = VirtIoSndPcmParamsSet;
  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwPreparePcm(PAEROVIOSND_DEVICE_EXTENSION Dx) {
  NTSTATUS status;

  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  if (Dx->PcmState == VirtIoSndPcmPrepared || Dx->PcmState == VirtIoSndPcmRunning) {
    return STATUS_SUCCESS;
  }

  if (Dx->PcmState == VirtIoSndPcmIdle) {
    status = VirtIoSndHwSetPcmParams(Dx, Dx->BufferBytes, Dx->PeriodBytes);
    if (!NT_SUCCESS(status)) {
      return status;
    }
  }

  if (Dx->PcmState == VirtIoSndPcmParamsSet) {
    status = VirtIoSndControlSimple(Dx, VIRTIO_SND_R_PCM_PREPARE, VIRTIOSND_STREAM_ID_PLAYBACK);
    if (!NT_SUCCESS(status)) {
      return status;
    }
    Dx->PcmState = VirtIoSndPcmPrepared;
  }

  if (Dx->PcmState != VirtIoSndPcmPrepared) {
    return STATUS_INVALID_DEVICE_STATE;
  }

  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwStartPcm(PAEROVIOSND_DEVICE_EXTENSION Dx) {
  NTSTATUS status;
  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  if (Dx->PcmState == VirtIoSndPcmRunning) {
    return STATUS_SUCCESS;
  }

  status = VirtIoSndHwPreparePcm(Dx);
  if (!NT_SUCCESS(status)) {
    return status;
  }

  status = VirtIoSndControlSimple(Dx, VIRTIO_SND_R_PCM_START, VIRTIOSND_STREAM_ID_PLAYBACK);
  if (!NT_SUCCESS(status)) {
    return status;
  }
  Dx->PcmState = VirtIoSndPcmRunning;
  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwStopPcm(PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  if (Dx->PcmState != VirtIoSndPcmRunning) {
    return STATUS_SUCCESS;
  }

  {
    NTSTATUS status = VirtIoSndControlSimple(Dx, VIRTIO_SND_R_PCM_STOP, VIRTIOSND_STREAM_ID_PLAYBACK);
    if (!NT_SUCCESS(status)) {
      return status;
    }
  }

  Dx->PcmState = VirtIoSndPcmPrepared;
  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwReleasePcm(PAEROVIOSND_DEVICE_EXTENSION Dx) {
  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  {
    NTSTATUS status = VirtIoSndControlSimple(Dx, VIRTIO_SND_R_PCM_RELEASE, VIRTIOSND_STREAM_ID_PLAYBACK);
    if (!NT_SUCCESS(status)) {
      return status;
    }
  }

  Dx->PcmState = VirtIoSndPcmIdle;
  return STATUS_SUCCESS;
}

_Use_decl_annotations_ NTSTATUS VirtIoSndHwSubmitTx(PAEROVIOSND_DEVICE_EXTENSION Dx, const VOID* Data, ULONG Bytes) {
  KIRQL oldIrql;
  PAEROVIOSND_TX_ENTRY entry;
  ULONG copy;
  VIRTIO_SG_ENTRY sg[2];
  USHORT headId;
  NTSTATUS status;
  PUCHAR p;

  if (!Dx || !Dx->Started) {
    return STATUS_DEVICE_NOT_READY;
  }

  if (Dx->PcmState != VirtIoSndPcmRunning) {
    return STATUS_INVALID_DEVICE_STATE;
  }

  if (!Data || Bytes == 0) {
    return STATUS_INVALID_PARAMETER;
  }

  KeAcquireSpinLock(&Dx->Lock, &oldIrql);

  if (IsListEmpty(&Dx->TxFreeList) || Dx->TxVq.NumFree < 2) {
    KeReleaseSpinLock(&Dx->Lock, oldIrql);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  entry = CONTAINING_RECORD(RemoveHeadList(&Dx->TxFreeList), AEROVIOSND_TX_ENTRY, Link);

  p = entry->BufferVa;
  *(ULONG*)(p + 0) = VIRTIOSND_STREAM_ID_PLAYBACK;
  *(ULONG*)(p + 4) = 0;

  copy = Bytes;
  // Keep framing valid for S16_LE stereo (4-byte frames).
  copy &= ~(VIRTIOSND_BLOCK_ALIGN - 1u);
  if (copy == 0) {
    InsertHeadList(&Dx->TxFreeList, &entry->Link);
    KeReleaseSpinLock(&Dx->Lock, oldIrql);
    return STATUS_SUCCESS;
  }
  if (copy > entry->PayloadBytes) {
    copy = entry->PayloadBytes;
  }

  RtlCopyMemory(p + 8, Data, copy);
  RtlZeroMemory(p + 8 + entry->PayloadBytes, 8);

  sg[0].Address = entry->BufferPa;
  sg[0].Length = 8 + copy;
  sg[0].Write = FALSE;

  sg[1].Address = entry->BufferPa;
  sg[1].Address.QuadPart += 8 + entry->PayloadBytes;
  sg[1].Length = 8;
  sg[1].Write = TRUE;

  status = VirtioQueueAddBuffer(&Dx->TxVq, sg, 2, entry, &headId);
  if (!NT_SUCCESS(status)) {
    InsertHeadList(&Dx->TxFreeList, &entry->Link);
    KeReleaseSpinLock(&Dx->Lock, oldIrql);
    return status;
  }

  entry->HeadId = headId;
  InsertTailList(&Dx->TxSubmittedList, &entry->Link);

  VirtioQueueNotify(&Dx->Vdev, &Dx->TxVq);
  KeReleaseSpinLock(&Dx->Lock, oldIrql);

  return STATUS_SUCCESS;
}

// --------------------------------------------------------------------------
// Miniport lifetime integration
// --------------------------------------------------------------------------

VOID VirtIoSndMiniportAddRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  VirtIoSndHwAddRef(Dx);
}

VOID VirtIoSndMiniportReleaseRef(_Inout_ PAEROVIOSND_DEVICE_EXTENSION Dx) {
  VirtIoSndHwReleaseRef(Dx);
}
