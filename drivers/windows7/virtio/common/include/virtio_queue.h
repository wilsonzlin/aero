#pragma once

#include <ntddk.h>

#include "virtio_pci_modern_miniport.h"

/*
 * Split virtqueue implementation ("vring") per virtio specification.
 *
 * The queue memory is allocated as a single physically-contiguous region and
 * shared with the device via the virtio-pci modern common_cfg queue address
 * registers (queue_desc/queue_avail/queue_used).
 *
 * This implementation does not negotiate or require:
 *  - VIRTIO_RING_F_INDIRECT_DESC
 *  - VIRTIO_RING_F_EVENT_IDX
 */

/* Split ring alignment requirements (virtio 1.0). */
#define VIRTIO_VRING_DESC_ALIGN 16u
#define VIRTIO_VRING_USED_ALIGN 4u

#define VRING_DESC_F_NEXT  0x0001
#define VRING_DESC_F_WRITE 0x0002
#define VRING_DESC_F_INDIRECT 0x0004

#pragma pack(push, 1)
typedef struct _VRING_DESC {
  ULONGLONG Addr;
  ULONG Len;
  USHORT Flags;
  USHORT Next;
} VRING_DESC;

typedef struct _VRING_AVAIL {
  USHORT Flags;
  USHORT Idx;
  USHORT Ring[1]; // variable-sized
} VRING_AVAIL;

typedef struct _VRING_USED_ELEM {
  ULONG Id;
  ULONG Len;
} VRING_USED_ELEM;

typedef struct _VRING_USED {
  USHORT Flags;
  USHORT Idx;
  VRING_USED_ELEM Ring[1]; // variable-sized
} VRING_USED;
#pragma pack(pop)

typedef struct _VIRTIO_SG_ENTRY {
  PHYSICAL_ADDRESS Address;
  ULONG Length;
  BOOLEAN Write;
} VIRTIO_SG_ENTRY;

typedef struct _VIRTIO_QUEUE {
  USHORT QueueIndex;
  USHORT QueueSize;

  volatile UINT16* NotifyAddr;

  PVOID RingVa;
  PHYSICAL_ADDRESS RingPa;
  ULONG RingBytes;

  VRING_DESC* Desc;
  VRING_AVAIL* Avail;
  VRING_USED* Used;
  ULONG UsedOffset;

  // Driver-side indices
  USHORT FreeHead;
  USHORT NumFree;
  USHORT LastUsedIdx;

  // Per-head context, indexed by descriptor id returned in used ring.
  PVOID* Context;
} VIRTIO_QUEUE;

_Must_inspect_result_ NTSTATUS VirtioQueueCreate(_Inout_ VIRTIO_PCI_DEVICE* Device, _Out_ VIRTIO_QUEUE* Queue,
                                                  _In_ USHORT QueueIndex);
VOID VirtioQueueDelete(_Inout_ VIRTIO_PCI_DEVICE* Device, _Inout_ VIRTIO_QUEUE* Queue);

VOID VirtioQueueResetState(_Inout_ VIRTIO_QUEUE* Queue);

_Must_inspect_result_ NTSTATUS VirtioQueueAddBuffer(_Inout_ VIRTIO_QUEUE* Queue, _In_reads_(SgCount) const VIRTIO_SG_ENTRY* Sg,
                                                    _In_ USHORT SgCount, _In_opt_ PVOID Context, _Out_ USHORT* HeadId);

_Must_inspect_result_ NTSTATUS VirtioQueueAddIndirectTable(_Inout_ VIRTIO_QUEUE* Queue, _In_ PHYSICAL_ADDRESS IndirectTablePa,
                                                           _In_ USHORT IndirectDescCount, _In_opt_ PVOID Context,
                                                           _Out_ USHORT* HeadId);

BOOLEAN VirtioQueuePopUsed(_Inout_ VIRTIO_QUEUE* Queue, _Out_ USHORT* HeadId, _Out_ ULONG* Len, _Out_opt_ PVOID* Context);

VOID VirtioQueueNotify(_Inout_ VIRTIO_PCI_DEVICE* Device, _In_ const VIRTIO_QUEUE* Queue);
