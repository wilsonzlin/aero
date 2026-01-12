#include "aerogpu_kmd.h"

#define AEROGPU_DEFAULT_RING_BYTES (256u * 1024u)

static NTSTATUS APIENTRY AerogpuDdiAddDevice(_In_ PDEVICE_OBJECT physicalDeviceObject,
                                             _Outptr_ PVOID *miniportDeviceContext);
static NTSTATUS APIENTRY AerogpuDdiStartDevice(_In_ PVOID miniportDeviceContext, _In_ PDXGK_START_INFO dxgkStartInfo,
                                               _In_ PDXGKRNL_INTERFACE dxgkInterface,
                                               _Out_ PULONG numberOfVideoPresentSources, _Out_ PULONG numberOfChildren);
static NTSTATUS APIENTRY AerogpuDdiStopDevice(_In_ PVOID miniportDeviceContext);
static NTSTATUS APIENTRY AerogpuDdiRemoveDevice(_In_ PVOID miniportDeviceContext);
static VOID APIENTRY AerogpuDdiUnload(_In_ PDRIVER_OBJECT driverObject);

static NTSTATUS APIENTRY AerogpuDdiQueryChildRelations(_In_ CONST PVOID miniportDeviceContext,
                                                       _Inout_ PDXGK_CHILD_DESCRIPTOR childRelations,
                                                       _In_ ULONG childRelationsSize);
static NTSTATUS APIENTRY AerogpuDdiQueryChildStatus(_In_ CONST PVOID miniportDeviceContext,
                                                    _Inout_ PDXGKARG_QUERYCHILDSTATUS queryChildStatus);
static NTSTATUS APIENTRY AerogpuDdiQueryDeviceDescriptor(_In_ CONST PVOID miniportDeviceContext,
                                                         _In_ ULONG childUid,
                                                         _Inout_ PDXGK_DEVICE_DESCRIPTOR deviceDescriptor);
static NTSTATUS APIENTRY AerogpuDdiQueryAdapterInfo(_In_ CONST PVOID miniportDeviceContext,
                                                    _In_ CONST DXGKARG_QUERYADAPTERINFO *queryAdapterInfo);

static NTSTATUS APIENTRY AerogpuDdiEscape(_In_ CONST HANDLE hAdapter, _In_ CONST DXGKARG_ESCAPE *pEscape);

static BOOLEAN APIENTRY AerogpuDdiInterruptRoutine(_In_ PVOID miniportDeviceContext, _In_ ULONG messageNumber);
static VOID APIENTRY AerogpuDdiDpcRoutine(_In_ PVOID miniportDeviceContext);
static NTSTATUS APIENTRY AerogpuDdiControlInterrupt(_In_ PVOID miniportDeviceContext, _In_ BOOLEAN enableInterrupt);

static NTSTATUS APIENTRY AerogpuDdiSetPowerState(_In_ PVOID miniportDeviceContext, _In_ ULONG deviceUid,
                                                 _In_ DEVICE_POWER_STATE devicePowerState, _In_ POWER_ACTION actionType);

static NTSTATUS APIENTRY AerogpuDdiIsSupportedVidPn(_In_ PVOID miniportDeviceContext,
                                                    _Inout_ PDXGKARG_ISSUPPORTEDVIDPN isSupportedVidPn);
static NTSTATUS APIENTRY AerogpuDdiRecommendFunctionalVidPn(_In_ PVOID miniportDeviceContext,
                                                            _In_ CONST DXGKARG_RECOMMENDFUNCTIONALVIDPN *recommendVidPn);
static NTSTATUS APIENTRY AerogpuDdiEnumVidPnCofuncModality(_In_ PVOID miniportDeviceContext,
                                                          _In_ CONST DXGKARG_ENUMVIDPNCOFUNCMODALITY *enumCofuncModality);
static NTSTATUS APIENTRY AerogpuDdiCommitVidPn(_In_ PVOID miniportDeviceContext,
                                               _In_ CONST DXGKARG_COMMITVIDPN *commitVidPn);
static NTSTATUS APIENTRY AerogpuDdiUpdateActiveVidPnPresentPath(
    _In_ PVOID miniportDeviceContext, _In_ CONST DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH *updateActiveVidPnPresentPath);
static NTSTATUS APIENTRY AerogpuDdiSetVidPnSourceVisibility(_In_ PVOID miniportDeviceContext,
                                                            _In_ CONST DXGKARG_SETVIDPNSOURCEVISIBILITY *visibility);
static NTSTATUS APIENTRY AerogpuDdiSetVidPnSourceAddress(_In_ PVOID miniportDeviceContext,
                                                         _In_ CONST DXGKARG_SETVIDPNSOURCEADDRESS *setSourceAddress);

NTSTATUS APIENTRY DriverEntry(_In_ PDRIVER_OBJECT driverObject, _In_ PUNICODE_STRING registryPath) {
  DXGK_INITIALIZATION_DATA initData;
  RtlZeroMemory(&initData, sizeof(initData));

  initData.Version = DXGKDDI_INTERFACE_VERSION_WDDM1_1;

  initData.DxgkDdiAddDevice = AerogpuDdiAddDevice;
  initData.DxgkDdiStartDevice = AerogpuDdiStartDevice;
  initData.DxgkDdiStopDevice = AerogpuDdiStopDevice;
  initData.DxgkDdiRemoveDevice = AerogpuDdiRemoveDevice;
  initData.DxgkDdiUnload = AerogpuDdiUnload;

  initData.DxgkDdiQueryChildRelations = AerogpuDdiQueryChildRelations;
  initData.DxgkDdiQueryChildStatus = AerogpuDdiQueryChildStatus;
  initData.DxgkDdiQueryDeviceDescriptor = AerogpuDdiQueryDeviceDescriptor;

  initData.DxgkDdiQueryAdapterInfo = AerogpuDdiQueryAdapterInfo;
  initData.DxgkDdiEscape = AerogpuDdiEscape;

  initData.DxgkDdiInterruptRoutine = AerogpuDdiInterruptRoutine;
  initData.DxgkDdiDpcRoutine = AerogpuDdiDpcRoutine;
  initData.DxgkDdiControlInterrupt = AerogpuDdiControlInterrupt;

  initData.DxgkDdiSetPowerState = AerogpuDdiSetPowerState;

  initData.DxgkDdiIsSupportedVidPn = AerogpuDdiIsSupportedVidPn;
  initData.DxgkDdiRecommendFunctionalVidPn = AerogpuDdiRecommendFunctionalVidPn;
  initData.DxgkDdiEnumVidPnCofuncModality = AerogpuDdiEnumVidPnCofuncModality;
  initData.DxgkDdiCommitVidPn = AerogpuDdiCommitVidPn;
  initData.DxgkDdiUpdateActiveVidPnPresentPath = AerogpuDdiUpdateActiveVidPnPresentPath;
  initData.DxgkDdiSetVidPnSourceVisibility = AerogpuDdiSetVidPnSourceVisibility;
  initData.DxgkDdiSetVidPnSourceAddress = AerogpuDdiSetVidPnSourceAddress;

  return DxgkInitialize(driverObject, registryPath, &initData);
}

NTSTATUS AerogpuRingInit(_Inout_ PAEROGPU_ADAPTER adapter, _In_ ULONG ringBytes) {
  PHYSICAL_ADDRESS low;
  PHYSICAL_ADDRESS high;
  PHYSICAL_ADDRESS boundary;

  low.QuadPart = 0;
  high.QuadPart = ~0ull;
  boundary.QuadPart = 0;

  adapter->RingVa = (PUCHAR)MmAllocateContiguousMemorySpecifyCache(ringBytes, low, high, boundary, MmCached);
  if (adapter->RingVa == NULL) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  adapter->RingPa = MmGetPhysicalAddress(adapter->RingVa);
  adapter->RingSizeBytes = ringBytes;
  adapter->RingTailBytes = 0;
  adapter->NextFenceValue = 1;
  adapter->CompletedFenceValue = 0;

  RtlZeroMemory(adapter->RingVa, ringBytes);

  AerogpuMmioWrite32(adapter, AEROGPU_REG_RING_GPA_LO, (ULONG)(adapter->RingPa.QuadPart & 0xFFFFFFFFu));
  AerogpuMmioWrite32(adapter, AEROGPU_REG_RING_GPA_HI, (ULONG)((adapter->RingPa.QuadPart >> 32) & 0xFFFFFFFFu));
  AerogpuMmioWrite32(adapter, AEROGPU_REG_RING_SIZE, ringBytes);
  AerogpuMmioWrite32(adapter, AEROGPU_REG_RING_TAIL, 0);

  return STATUS_SUCCESS;
}

VOID AerogpuRingShutdown(_Inout_ PAEROGPU_ADAPTER adapter) {
  if (adapter->RingVa != NULL) {
    MmFreeContiguousMemory(adapter->RingVa);
    adapter->RingVa = NULL;
  }
  adapter->RingSizeBytes = 0;
  adapter->RingTailBytes = 0;
}

static ULONG AerogpuRingFreeBytes(_Inout_ PAEROGPU_ADAPTER adapter) {
  ULONG head = AerogpuMmioRead32(adapter, AEROGPU_REG_RING_HEAD) % adapter->RingSizeBytes;
  ULONG tail = adapter->RingTailBytes % adapter->RingSizeBytes;

  ULONG used = (tail >= head) ? (tail - head) : (tail + adapter->RingSizeBytes - head);

  // Keep 1 byte empty to avoid head==tail ambiguity.
  if (used >= adapter->RingSizeBytes) {
    return 0;
  }
  return adapter->RingSizeBytes - used - 1;
}

NTSTATUS AerogpuRingWrite(_Inout_ PAEROGPU_ADAPTER adapter, _In_reads_bytes_(sizeBytes) const VOID *data,
                          _In_ ULONG sizeBytes) {
  if (adapter->RingVa == NULL || adapter->RingSizeBytes == 0) {
    return STATUS_DEVICE_NOT_READY;
  }

  // Commands must be 8-byte aligned in v1.
  ULONG alignedSize = (sizeBytes + 7u) & ~7u;
  if (alignedSize >= adapter->RingSizeBytes) {
    return STATUS_INVALID_BUFFER_SIZE;
  }

  // Busy-wait for space. v1 keeps this simple; a future version can use a
  // kernel event driven by interrupts/fences.
  for (ULONG spin = 0; spin < 1000000u; spin++) {
    if (AerogpuRingFreeBytes(adapter) >= alignedSize) {
      break;
    }
    KeStallExecutionProcessor(1);
  }
  if (AerogpuRingFreeBytes(adapter) < alignedSize) {
    return STATUS_DEVICE_BUSY;
  }

  ULONG tail = adapter->RingTailBytes % adapter->RingSizeBytes;
  ULONG firstCopy = adapter->RingSizeBytes - tail;
  if (firstCopy > alignedSize) {
    firstCopy = alignedSize;
  }

  RtlCopyMemory(adapter->RingVa + tail, data, firstCopy);
  if (alignedSize > firstCopy) {
    RtlCopyMemory(adapter->RingVa, ((const UCHAR *)data) + firstCopy, alignedSize - firstCopy);
  } else if (alignedSize > sizeBytes) {
    // Pad to 8-byte alignment.
    RtlZeroMemory(adapter->RingVa + tail + sizeBytes, alignedSize - sizeBytes);
  }

  adapter->RingTailBytes = (tail + alignedSize) % adapter->RingSizeBytes;
  KeMemoryBarrier();
  AerogpuMmioWrite32(adapter, AEROGPU_REG_RING_TAIL, adapter->RingTailBytes);

  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiAddDevice(_In_ PDEVICE_OBJECT physicalDeviceObject,
                                             _Outptr_ PVOID *miniportDeviceContext) {
  PAEROGPU_ADAPTER adapter =
      (PAEROGPU_ADAPTER)ExAllocatePoolWithTag(NonPagedPool, sizeof(AEROGPU_ADAPTER), AEROGPU_KMD_POOL_TAG);
  if (adapter == NULL) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  RtlZeroMemory(adapter, sizeof(*adapter));
  adapter->PhysicalDeviceObject = physicalDeviceObject;
  *miniportDeviceContext = adapter;
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiStartDevice(_In_ PVOID miniportDeviceContext, _In_ PDXGK_START_INFO dxgkStartInfo,
                                               _In_ PDXGKRNL_INTERFACE dxgkInterface,
                                               _Out_ PULONG numberOfVideoPresentSources, _Out_ PULONG numberOfChildren) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;

  adapter->StartInfo = *dxgkStartInfo;
  adapter->DxgkInterface = *dxgkInterface;

  // Map BAR0 MMIO.
  adapter->MmioBase = NULL;
  adapter->MmioLength = 0;

  if (dxgkStartInfo->TranslatedResourceList == NULL) {
    return STATUS_INVALID_PARAMETER;
  }

  static BOOLEAN AerogpuExtractMemoryResource(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR *desc,
                                              _Out_ PHYSICAL_ADDRESS *startOut,
                                              _Out_ ULONG *lengthOut) {
    USHORT large;
    ULONGLONG lenBytes;

    if (startOut) {
      startOut->QuadPart = 0;
    }
    if (lengthOut) {
      *lengthOut = 0;
    }

    if (desc == NULL || startOut == NULL || lengthOut == NULL) {
      return FALSE;
    }

    lenBytes = 0;

    if (desc->Type == CmResourceTypeMemory) {
      *startOut = desc->u.Memory.Start;
      *lengthOut = desc->u.Memory.Length;
      return TRUE;
    }

    if (desc->Type == CmResourceTypeMemoryLarge) {
      large = desc->Flags & (CM_RESOURCE_MEMORY_LARGE_40 | CM_RESOURCE_MEMORY_LARGE_48 | CM_RESOURCE_MEMORY_LARGE_64);
      switch (large) {
      case CM_RESOURCE_MEMORY_LARGE_40:
        *startOut = desc->u.Memory40.Start;
        lenBytes = ((ULONGLONG)desc->u.Memory40.Length40) << 8;
        break;
      case CM_RESOURCE_MEMORY_LARGE_48:
        *startOut = desc->u.Memory48.Start;
        lenBytes = ((ULONGLONG)desc->u.Memory48.Length48) << 16;
        break;
      case CM_RESOURCE_MEMORY_LARGE_64:
        *startOut = desc->u.Memory64.Start;
        lenBytes = ((ULONGLONG)desc->u.Memory64.Length64) << 32;
        break;
      default:
        return FALSE;
      }

      if (lenBytes > 0xFFFFFFFFull) {
        return FALSE;
      }

      *lengthOut = (ULONG)lenBytes;
      return TRUE;
    }

    return FALSE;
  }

  PCM_FULL_RESOURCE_DESCRIPTOR fullRes = &dxgkStartInfo->TranslatedResourceList->List[0];
  PCM_PARTIAL_RESOURCE_LIST partialRes = &fullRes->PartialResourceList;
  for (ULONG i = 0; i < partialRes->Count; i++) {
    PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = &partialRes->PartialDescriptors[i];
    PHYSICAL_ADDRESS start;
    ULONG length;
    if (!AerogpuExtractMemoryResource(desc, &start, &length)) {
      continue;
    }

    adapter->MmioLength = length;
    adapter->MmioBase = (PUCHAR)MmMapIoSpace(start, adapter->MmioLength, MmNonCached);
    break;
  }

  if (adapter->MmioBase == NULL) {
    return STATUS_DEVICE_CONFIGURATION_ERROR;
  }

  // Basic device sanity check (best-effort).
  ULONG devId = AerogpuMmioRead32(adapter, AEROGPU_REG_DEVICE_ID);
  ULONG ver = AerogpuMmioRead32(adapter, AEROGPU_REG_VERSION);
  UNREFERENCED_PARAMETER(devId);
  UNREFERENCED_PARAMETER(ver);

  NTSTATUS status = AerogpuRingInit(adapter, AEROGPU_DEFAULT_RING_BYTES);
  if (!NT_SUCCESS(status)) {
    MmUnmapIoSpace(adapter->MmioBase, adapter->MmioLength);
    adapter->MmioBase = NULL;
    adapter->MmioLength = 0;
    return status;
  }

  *numberOfVideoPresentSources = 1;
  *numberOfChildren = 1;
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiStopDevice(_In_ PVOID miniportDeviceContext) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;

  AerogpuRingShutdown(adapter);

  if (adapter->MmioBase != NULL) {
    MmUnmapIoSpace(adapter->MmioBase, adapter->MmioLength);
    adapter->MmioBase = NULL;
    adapter->MmioLength = 0;
  }

  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiRemoveDevice(_In_ PVOID miniportDeviceContext) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;
  ExFreePoolWithTag(adapter, AEROGPU_KMD_POOL_TAG);
  return STATUS_SUCCESS;
}

static VOID APIENTRY AerogpuDdiUnload(_In_ PDRIVER_OBJECT driverObject) { UNREFERENCED_PARAMETER(driverObject); }

static NTSTATUS APIENTRY AerogpuDdiQueryChildRelations(_In_ CONST PVOID miniportDeviceContext,
                                                       _Inout_ PDXGK_CHILD_DESCRIPTOR childRelations,
                                                       _In_ ULONG childRelationsSize) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);

  if (childRelationsSize < sizeof(DXGK_CHILD_DESCRIPTOR)) {
    return STATUS_BUFFER_TOO_SMALL;
  }

  RtlZeroMemory(childRelations, childRelationsSize);
  childRelations[0].ChildDeviceType = DXGK_CHILD_DEVICE_TYPE_MONITOR;
  childRelations[0].ChildCapabilities.Type.VideoOutput.HpdAwareness = HpdAwarenessAlwaysConnected;
  childRelations[0].ChildUid = 0;
  childRelations[0].AcpiUid = 0;

  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiQueryChildStatus(_In_ CONST PVOID miniportDeviceContext,
                                                    _Inout_ PDXGKARG_QUERYCHILDSTATUS queryChildStatus) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);

  if (queryChildStatus->ChildUid != 0) {
    return STATUS_INVALID_PARAMETER;
  }

  queryChildStatus->ChildStatus.Type = StatusConnection;
  queryChildStatus->ChildStatus.HotPlug.Connected = TRUE;
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiQueryDeviceDescriptor(_In_ CONST PVOID miniportDeviceContext, _In_ ULONG childUid,
                                                         _Inout_ PDXGK_DEVICE_DESCRIPTOR deviceDescriptor) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);

  if (childUid != 0) {
    return STATUS_INVALID_PARAMETER;
  }

  RtlZeroMemory(deviceDescriptor, sizeof(*deviceDescriptor));
  deviceDescriptor->DeviceId = 0;
  deviceDescriptor->VendorId = 0;
  deviceDescriptor->SubSysId = 0;
  deviceDescriptor->RevisionId = 0;

  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiQueryAdapterInfo(_In_ CONST PVOID miniportDeviceContext,
                                                    _In_ CONST DXGKARG_QUERYADAPTERINFO *queryAdapterInfo) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);

  // Minimal v1: acknowledge but do not advertise additional caps yet.
  if (queryAdapterInfo->pOutputData != NULL && queryAdapterInfo->OutputDataSize != 0) {
    RtlZeroMemory(queryAdapterInfo->pOutputData, queryAdapterInfo->OutputDataSize);
  }
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiEscape(_In_ CONST HANDLE hAdapter, _In_ CONST DXGKARG_ESCAPE *pEscape) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)hAdapter;

  if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_packet_t) || pEscape->pPrivateDriverData == NULL) {
    return STATUS_INVALID_PARAMETER;
  }

  aerogpu_escape_packet_t *packet = (aerogpu_escape_packet_t *)pEscape->pPrivateDriverData;
  if (packet->magic != AEROGPU_ESCAPE_MAGIC || packet->version != AEROGPU_ESCAPE_VERSION) {
    return STATUS_INVALID_PARAMETER;
  }
  if (packet->size_bytes > pEscape->PrivateDriverDataSize) {
    return STATUS_INVALID_PARAMETER;
  }

  switch ((aerogpu_escape_op_t)packet->op) {
  case AEROGPU_ESCAPE_SUBMIT: {
    if (packet->size_bytes < sizeof(aerogpu_escape_packet_t) + sizeof(aerogpu_escape_submit_t)) {
      return STATUS_INVALID_PARAMETER;
    }

    aerogpu_escape_submit_t *submit = (aerogpu_escape_submit_t *)(packet + 1);
    ULONG payloadOffset = sizeof(aerogpu_escape_packet_t) + sizeof(aerogpu_escape_submit_t);
    if (payloadOffset + submit->stream_bytes != packet->size_bytes) {
      return STATUS_INVALID_PARAMETER;
    }
    const VOID *stream = ((const UCHAR *)packet) + payloadOffset;

    NTSTATUS status = AerogpuRingWrite(adapter, stream, submit->stream_bytes);
    if (!NT_SUCCESS(status)) {
      return status;
    }

    // If the UMD didn't supply a fence, assign one and push it to the ring so
    // the host has a point to signal completion from.
    if (submit->fence_value == 0) {
      ULONGLONG fence = adapter->NextFenceValue++;
      aerogpu_cmd_header_t hdr;
      aerogpu_cmd_fence_signal_t sig;
      hdr.opcode = AEROGPU_CMD_FENCE_SIGNAL;
      hdr.size_bytes = sizeof(hdr) + sizeof(sig);
      sig.fence_value = fence;

      UCHAR buf[sizeof(hdr) + sizeof(sig)];
      RtlCopyMemory(buf, &hdr, sizeof(hdr));
      RtlCopyMemory(buf + sizeof(hdr), &sig, sizeof(sig));

      status = AerogpuRingWrite(adapter, buf, sizeof(buf));
      if (!NT_SUCCESS(status)) {
        return status;
      }

      submit->fence_value = fence;
    }

    return STATUS_SUCCESS;
  }
  case AEROGPU_ESCAPE_QUERY_CAPS:
    return STATUS_NOT_SUPPORTED;
  default:
    return STATUS_NOT_SUPPORTED;
  }
}

static BOOLEAN APIENTRY AerogpuDdiInterruptRoutine(_In_ PVOID miniportDeviceContext, _In_ ULONG messageNumber) {
  UNREFERENCED_PARAMETER(messageNumber);
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;

  ULONG irq = AerogpuMmioRead32(adapter, AEROGPU_REG_IRQ_STATUS);
  if (irq == 0) {
    return FALSE;
  }

  // Ack and defer real work.
  AerogpuMmioWrite32(adapter, AEROGPU_REG_IRQ_ACK, irq);
  adapter->DxgkInterface.DxgkCbQueueDpc(adapter->DxgkInterface.DeviceHandle);
  return TRUE;
}

static VOID APIENTRY AerogpuDdiDpcRoutine(_In_ PVOID miniportDeviceContext) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;

  ULONGLONG completed =
      ((ULONGLONG)AerogpuMmioRead32(adapter, AEROGPU_REG_FENCE_COMPLETED_LO)) |
      (((ULONGLONG)AerogpuMmioRead32(adapter, AEROGPU_REG_FENCE_COMPLETED_HI)) << 32);

  adapter->CompletedFenceValue = completed;

  // v1 does not integrate with the dxgkrnl scheduler yet; we only wake any
  // waiters that rely on escape-driven completion.
  adapter->DxgkInterface.DxgkCbNotifyDpc(adapter->DxgkInterface.DeviceHandle);
}

static NTSTATUS APIENTRY AerogpuDdiControlInterrupt(_In_ PVOID miniportDeviceContext, _In_ BOOLEAN enableInterrupt) {
  PAEROGPU_ADAPTER adapter = (PAEROGPU_ADAPTER)miniportDeviceContext;
  adapter->InterruptsEnabled = enableInterrupt;
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiSetPowerState(_In_ PVOID miniportDeviceContext, _In_ ULONG deviceUid,
                                                 _In_ DEVICE_POWER_STATE devicePowerState, _In_ POWER_ACTION actionType) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(deviceUid);
  UNREFERENCED_PARAMETER(devicePowerState);
  UNREFERENCED_PARAMETER(actionType);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiIsSupportedVidPn(_In_ PVOID miniportDeviceContext,
                                                    _Inout_ PDXGKARG_ISSUPPORTEDVIDPN isSupportedVidPn) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  isSupportedVidPn->IsVidPnSupported = TRUE;
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiRecommendFunctionalVidPn(_In_ PVOID miniportDeviceContext,
                                                            _In_ CONST DXGKARG_RECOMMENDFUNCTIONALVIDPN *recommendVidPn) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(recommendVidPn);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiEnumVidPnCofuncModality(_In_ PVOID miniportDeviceContext,
                                                          _In_ CONST DXGKARG_ENUMVIDPNCOFUNCMODALITY *enumCofuncModality) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(enumCofuncModality);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiCommitVidPn(_In_ PVOID miniportDeviceContext,
                                               _In_ CONST DXGKARG_COMMITVIDPN *commitVidPn) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(commitVidPn);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiUpdateActiveVidPnPresentPath(
    _In_ PVOID miniportDeviceContext, _In_ CONST DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH *updateActiveVidPnPresentPath) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(updateActiveVidPnPresentPath);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiSetVidPnSourceVisibility(_In_ PVOID miniportDeviceContext,
                                                            _In_ CONST DXGKARG_SETVIDPNSOURCEVISIBILITY *visibility) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(visibility);
  return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AerogpuDdiSetVidPnSourceAddress(_In_ PVOID miniportDeviceContext,
                                                         _In_ CONST DXGKARG_SETVIDPNSOURCEADDRESS *setSourceAddress) {
  UNREFERENCED_PARAMETER(miniportDeviceContext);
  UNREFERENCED_PARAMETER(setSourceAddress);
  return STATUS_SUCCESS;
}
