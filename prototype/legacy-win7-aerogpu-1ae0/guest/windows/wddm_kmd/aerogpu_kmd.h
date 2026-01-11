#pragma once

#include <ntddk.h>
#include <d3dkmddi.h>

#include "../common/aerogpu_protocol.h"

#define AEROGPU_KMD_POOL_TAG '0R3A' // "A3R0"

typedef struct _AEROGPU_ADAPTER {
  PDEVICE_OBJECT PhysicalDeviceObject;

  DXGKRNL_INTERFACE DxgkInterface;
  DXGK_START_INFO StartInfo;

  PUCHAR MmioBase;
  ULONG MmioLength;

  // Software-managed command ring that the host consumes.
  //
  // The ring is physically contiguous so the device model can DMA/poll it.
  PUCHAR RingVa;
  PHYSICAL_ADDRESS RingPa;
  ULONG RingSizeBytes;

  // Cached producer pointer in bytes. The authoritative consumer pointer is
  // provided by the host via AEROGPU_REG_RING_HEAD.
  volatile ULONG RingTailBytes;

  // Fences for basic synchronization.
  volatile ULONGLONG NextFenceValue;
  volatile ULONGLONG CompletedFenceValue;

  // Interrupt plumbing (optional; v1 can poll).
  BOOLEAN InterruptsEnabled;
} AEROGPU_ADAPTER, *PAEROGPU_ADAPTER;

__forceinline ULONG AerogpuMmioRead32(_In_ const AEROGPU_ADAPTER *adapter, _In_ ULONG offset) {
  return READ_REGISTER_ULONG((volatile ULONG *)(adapter->MmioBase + offset));
}

__forceinline VOID AerogpuMmioWrite32(_In_ const AEROGPU_ADAPTER *adapter, _In_ ULONG offset, _In_ ULONG value) {
  WRITE_REGISTER_ULONG((volatile ULONG *)(adapter->MmioBase + offset), value);
}

NTSTATUS AerogpuRingInit(_Inout_ PAEROGPU_ADAPTER adapter, _In_ ULONG ringBytes);
VOID AerogpuRingShutdown(_Inout_ PAEROGPU_ADAPTER adapter);
NTSTATUS AerogpuRingWrite(_Inout_ PAEROGPU_ADAPTER adapter, _In_reads_bytes_(sizeBytes) const VOID *data,
                          _In_ ULONG sizeBytes);

