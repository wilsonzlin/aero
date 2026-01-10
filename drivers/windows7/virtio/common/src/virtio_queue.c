#include "../include/virtio_queue.h"

#define VQ_TAG_CTX  'cQvV'

static __forceinline ULONG VirtioAlignUp(_In_ ULONG Value, _In_ ULONG Align) { return (Value + (Align - 1)) & ~(Align - 1); }

static ULONG VirtioQueueRingSizeBytes(_In_ USHORT QueueSize) {
  ULONG DescBytes = sizeof(VRING_DESC) * (ULONG)QueueSize;

  // vring_avail without EVENT_IDX: flags(u16) + idx(u16) + ring[QueueSize](u16 each)
  ULONG AvailBytes = sizeof(USHORT) * 2 + sizeof(USHORT) * (ULONG)QueueSize;

  ULONG UsedOffset = VirtioAlignUp(DescBytes + AvailBytes, VIRTIO_VRING_ALIGN);

  // vring_used without EVENT_IDX: flags(u16) + idx(u16) + ring[QueueSize](vring_used_elem)
  ULONG UsedBytes = sizeof(USHORT) * 2 + sizeof(VRING_USED_ELEM) * (ULONG)QueueSize;

  return UsedOffset + UsedBytes;
}

static VOID VirtioQueueInitLayout(_Inout_ VIRTIO_QUEUE* Queue) {
  ULONG DescBytes = sizeof(VRING_DESC) * (ULONG)Queue->QueueSize;
  ULONG AvailBytes = sizeof(USHORT) * 2 + sizeof(USHORT) * (ULONG)Queue->QueueSize;

  Queue->Desc = (VRING_DESC*)Queue->RingVa;
  Queue->Avail = (VRING_AVAIL*)((PUCHAR)Queue->RingVa + DescBytes);

  Queue->UsedOffset = VirtioAlignUp(DescBytes + AvailBytes, VIRTIO_VRING_ALIGN);
  Queue->Used = (VRING_USED*)((PUCHAR)Queue->RingVa + Queue->UsedOffset);
}

VOID VirtioQueueResetState(_Inout_ VIRTIO_QUEUE* Queue) {
  USHORT I;

  if (!Queue || Queue->QueueSize == 0 || Queue->RingVa == NULL || Queue->Desc == NULL || Queue->Avail == NULL || Queue->Used == NULL) {
    return;
  }

  Queue->FreeHead = 0;
  Queue->NumFree = Queue->QueueSize;
  Queue->LastUsedIdx = 0;

  // Clear rings/descriptors for sanity.
  RtlZeroMemory(Queue->RingVa, Queue->RingBytes);

  for (I = 0; I < Queue->QueueSize; I++) {
    Queue->Desc[I].Next = (USHORT)(I + 1);
  }

  Queue->Desc[Queue->QueueSize - 1].Next = 0xFFFF;

  Queue->Avail->Flags = 0;
  Queue->Avail->Idx = 0;

  Queue->Used->Flags = 0;
  Queue->Used->Idx = 0;

  if (Queue->Context) {
    RtlZeroMemory(Queue->Context, sizeof(PVOID) * Queue->QueueSize);
  }
}

NTSTATUS VirtioQueueCreate(_In_ const VIRTIO_PCI_DEVICE* Device, _Out_ VIRTIO_QUEUE* Queue, _In_ USHORT QueueIndex) {
  PHYSICAL_ADDRESS Low = {0};
  PHYSICAL_ADDRESS High;
  PHYSICAL_ADDRESS Skip = {0};

  RtlZeroMemory(Queue, sizeof(*Queue));
  Queue->QueueIndex = QueueIndex;

  High.QuadPart = ~0ull;

  VirtioPciSelectQueue(Device, QueueIndex);
  Queue->QueueSize = VirtioPciReadQueueSize(Device);
  if (Queue->QueueSize == 0) {
    return STATUS_NOT_SUPPORTED;
  }

  Queue->RingBytes = VirtioQueueRingSizeBytes(Queue->QueueSize);
  Queue->RingVa = MmAllocateContiguousMemorySpecifyCache(Queue->RingBytes, Low, High, Skip, MmCached);
  if (!Queue->RingVa) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  Queue->RingPa = MmGetPhysicalAddress(Queue->RingVa);
  if ((Queue->RingPa.QuadPart & (VIRTIO_VRING_ALIGN - 1)) != 0) {
    MmFreeContiguousMemory(Queue->RingVa);
    Queue->RingVa = NULL;
    return STATUS_DATATYPE_MISALIGNMENT;
  }

  Queue->Context =
      (PVOID*)ExAllocatePoolWithTag(NonPagedPool, sizeof(PVOID) * Queue->QueueSize, VQ_TAG_CTX);
  if (!Queue->Context) {
    MmFreeContiguousMemory(Queue->RingVa);
    Queue->RingVa = NULL;
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  VirtioQueueInitLayout(Queue);
  VirtioQueueResetState(Queue);

  // Share the queue with the device. PFN is 4k pages.
  VirtioPciSelectQueue(Device, QueueIndex);
  VirtioPciWriteQueuePfn(Device, (ULONG)(Queue->RingPa.QuadPart >> PAGE_SHIFT));

  return STATUS_SUCCESS;
}

VOID VirtioQueueDelete(_In_ const VIRTIO_PCI_DEVICE* Device, _Inout_ VIRTIO_QUEUE* Queue) {
  UNREFERENCED_PARAMETER(Device);

  if (Queue->RingVa) {
    // Per spec, writing 0 unconfigures the queue. The caller is expected to
    // have reset the device already; this is a best-effort cleanup.
    VirtioPciSelectQueue(Device, Queue->QueueIndex);
    VirtioPciWriteQueuePfn(Device, 0);

    MmFreeContiguousMemory(Queue->RingVa);
    Queue->RingVa = NULL;
  }

  if (Queue->Context) {
    ExFreePoolWithTag(Queue->Context, VQ_TAG_CTX);
    Queue->Context = NULL;
  }

  RtlZeroMemory(Queue, sizeof(*Queue));
}

static VOID VirtioQueueFreeDescChain(_Inout_ VIRTIO_QUEUE* Queue, _In_ USHORT Head) {
  USHORT Cur = Head;
  USHORT Next;

  for (;;) {
    USHORT Flags = Queue->Desc[Cur].Flags;
    Next = Queue->Desc[Cur].Next;

    Queue->Desc[Cur].Flags = 0;
    Queue->Desc[Cur].Len = 0;
    Queue->Desc[Cur].Addr = 0;

    Queue->Desc[Cur].Next = Queue->FreeHead;
    Queue->FreeHead = Cur;
    Queue->NumFree++;

    if ((Flags & VRING_DESC_F_NEXT) == 0) {
      break;
    }

    Cur = Next;
  }
}

NTSTATUS VirtioQueueAddBuffer(_Inout_ VIRTIO_QUEUE* Queue, _In_reads_(SgCount) const VIRTIO_SG_ENTRY* Sg, _In_ USHORT SgCount,
                              _In_opt_ PVOID Context, _Out_ USHORT* HeadId) {
  USHORT Head;
  USHORT Prev;
  USHORT Cur;
  USHORT Flags;
  USHORT I;

  if (!Queue || !Sg || SgCount == 0 || !HeadId) {
    return STATUS_INVALID_PARAMETER;
  }

  if (Queue->NumFree < SgCount) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  // Allocate and populate descriptors from the free list, linking as we go.
  Head = Queue->FreeHead;
  Prev = 0xFFFF;

  for (I = 0; I < SgCount; I++) {
    Cur = Queue->FreeHead;
    Queue->FreeHead = Queue->Desc[Cur].Next;
    Queue->NumFree--;

    Flags = 0;

    if (Sg[I].Write) {
      Flags |= VRING_DESC_F_WRITE;
    }

    Queue->Desc[Cur].Addr = (ULONGLONG)Sg[I].Address.QuadPart;
    Queue->Desc[Cur].Len = Sg[I].Length;
    Queue->Desc[Cur].Flags = Flags;

    if (Prev != 0xFFFF) {
      Queue->Desc[Prev].Flags |= VRING_DESC_F_NEXT;
      Queue->Desc[Prev].Next = Cur;
    }

    Prev = Cur;
  }

  Queue->Context[Head] = Context;

  // Publish to avail ring.
  Queue->Avail->Ring[Queue->Avail->Idx % Queue->QueueSize] = Head;
  KeMemoryBarrier();
  Queue->Avail->Idx++;

  *HeadId = Head;

  return STATUS_SUCCESS;
}

NTSTATUS VirtioQueueAddIndirectTable(_Inout_ VIRTIO_QUEUE* Queue, _In_ PHYSICAL_ADDRESS IndirectTablePa, _In_ USHORT IndirectDescCount,
                                     _In_opt_ PVOID Context, _Out_ USHORT* HeadId) {
  USHORT Head;
  ULONG TableBytes;

  if (!Queue || IndirectDescCount == 0 || !HeadId) {
    return STATUS_INVALID_PARAMETER;
  }

  TableBytes = (ULONG)IndirectDescCount * sizeof(VRING_DESC);
  if (IndirectDescCount != (USHORT)(TableBytes / sizeof(VRING_DESC))) {
    return STATUS_INVALID_PARAMETER;
  }

  if (Queue->NumFree < 1) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  Head = Queue->FreeHead;
  Queue->FreeHead = Queue->Desc[Head].Next;
  Queue->NumFree--;

  Queue->Desc[Head].Addr = (ULONGLONG)IndirectTablePa.QuadPart;
  Queue->Desc[Head].Len = TableBytes;
  Queue->Desc[Head].Flags = VRING_DESC_F_INDIRECT;
  Queue->Desc[Head].Next = 0;

  Queue->Context[Head] = Context;

  Queue->Avail->Ring[Queue->Avail->Idx % Queue->QueueSize] = Head;
  KeMemoryBarrier();
  Queue->Avail->Idx++;

  *HeadId = Head;
  return STATUS_SUCCESS;
}

BOOLEAN VirtioQueuePopUsed(_Inout_ VIRTIO_QUEUE* Queue, _Out_ USHORT* HeadId, _Out_ ULONG* Len, _Out_opt_ PVOID* Context) {
  USHORT UsedIdx;
  VRING_USED_ELEM Elem;
  USHORT RingPos;

  if (!Queue || !HeadId || !Len) {
    return FALSE;
  }

  UsedIdx = Queue->Used->Idx;
  KeMemoryBarrier();

  if (Queue->LastUsedIdx == UsedIdx) {
    return FALSE;
  }

  RingPos = Queue->LastUsedIdx % Queue->QueueSize;
  Elem = Queue->Used->Ring[RingPos];

  Queue->LastUsedIdx++;

  if (Elem.Id >= Queue->QueueSize) {
    return FALSE;
  }

  *HeadId = (USHORT)Elem.Id;
  *Len = Elem.Len;

  if (Context) {
    *Context = Queue->Context[*HeadId];
  }

  Queue->Context[*HeadId] = NULL;

  // Free descriptors back to the free list.
  VirtioQueueFreeDescChain(Queue, *HeadId);
  return TRUE;
}

VOID VirtioQueueNotify(_In_ const VIRTIO_PCI_DEVICE* Device, _In_ const VIRTIO_QUEUE* Queue) { VirtioPciNotifyQueue(Device, Queue->QueueIndex); }
