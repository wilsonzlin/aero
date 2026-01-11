#include "aerovnet.h"

#include <wdmguid.h>

static NDIS_HANDLE g_NdisDriverHandle = NULL;

static const NDIS_OID g_SupportedOids[] = {
    OID_GEN_SUPPORTED_LIST,
    OID_GEN_HARDWARE_STATUS,
    OID_GEN_MEDIA_SUPPORTED,
    OID_GEN_MEDIA_IN_USE,
    OID_GEN_PHYSICAL_MEDIUM,
    OID_GEN_MAXIMUM_FRAME_SIZE,
    OID_GEN_MAXIMUM_LOOKAHEAD,
    OID_GEN_CURRENT_LOOKAHEAD,
    OID_GEN_MAXIMUM_TOTAL_SIZE,
    OID_GEN_LINK_SPEED,
    OID_GEN_TRANSMIT_BLOCK_SIZE,
    OID_GEN_RECEIVE_BLOCK_SIZE,
    OID_GEN_VENDOR_ID,
    OID_GEN_VENDOR_DESCRIPTION,
    OID_GEN_DRIVER_VERSION,
    OID_GEN_VENDOR_DRIVER_VERSION,
    OID_GEN_MAC_OPTIONS,
    OID_GEN_MEDIA_CONNECT_STATUS,
    OID_GEN_CURRENT_PACKET_FILTER,
    OID_GEN_MAXIMUM_SEND_PACKETS,
    OID_GEN_XMIT_OK,
    OID_GEN_RCV_OK,
    OID_GEN_XMIT_ERROR,
    OID_GEN_RCV_ERROR,
    OID_GEN_RCV_NO_BUFFER,
    OID_GEN_LINK_STATE,
    OID_GEN_STATISTICS,
    OID_802_3_PERMANENT_ADDRESS,
    OID_802_3_CURRENT_ADDRESS,
    OID_802_3_MULTICAST_LIST,
    OID_802_3_MAXIMUM_LIST_SIZE,
};

/* 1 Gbps default link speed. */
static const ULONG64 g_DefaultLinkSpeedBps = 1000000000ull;

/* OID_GEN_DRIVER_VERSION encoding is major in high byte, minor in low byte. */
#define AEROVNET_OID_DRIVER_VERSION ((USHORT)((6u << 8) | 20u))

/* Allocate all shared DMA memory as cached (x86/x64 are cache-coherent). */
#define AEROVNET_DMA_CACHED TRUE

#define AEROVNET_VQ_ALIGN 4096u

#define VIRTIO_MSI_NO_VECTOR ((USHORT)0xFFFFu)

static __forceinline ULONG AerovNetSendCompleteFlagsForCurrentIrql(VOID)
{
    return (KeGetCurrentIrql() == DISPATCH_LEVEL) ? NDIS_SEND_COMPLETE_FLAGS_DISPATCH_LEVEL : 0;
}

static __forceinline ULONG AerovNetReceiveIndicationFlagsForCurrentIrql(VOID)
{
    return (KeGetCurrentIrql() == DISPATCH_LEVEL) ? NDIS_RECEIVE_FLAGS_DISPATCH_LEVEL : 0;
}

static VOID AerovNetFreeTxRequestNoLock(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_TX_REQUEST* TxReq)
{
    TxReq->State = AerovNetTxFree;
    TxReq->Cancelled = FALSE;
    TxReq->Nbl = NULL;
    TxReq->Nb = NULL;
    TxReq->SgList = NULL;
    TxReq->DescHeadId = 0;
    InsertTailList(&Adapter->TxFreeList, &TxReq->Link);
}

static VOID AerovNetCompleteNblSend(_In_ AEROVNET_ADAPTER* Adapter, _Inout_ PNET_BUFFER_LIST Nbl, _In_ NDIS_STATUS Status)
{
    NET_BUFFER_LIST_STATUS(Nbl) = Status;
    NdisMSendNetBufferListsComplete(Adapter->MiniportAdapterHandle, Nbl, AerovNetSendCompleteFlagsForCurrentIrql());
}

static VOID AerovNetTxNblCompleteOneNetBufferLocked(
    _Inout_ AEROVNET_ADAPTER* Adapter,
    _Inout_ PNET_BUFFER_LIST Nbl,
    _In_ NDIS_STATUS TxStatus,
    _Inout_ PNET_BUFFER_LIST* CompleteNblHead,
    _Inout_ PNET_BUFFER_LIST* CompleteNblTail)
{
    LONG pending;
    NDIS_STATUS nblStatus;
    NDIS_STATUS finalStatus;

    UNREFERENCED_PARAMETER(Adapter);

    /* Record the first failure for the NBL. */
    if (TxStatus != NDIS_STATUS_SUCCESS) {
        nblStatus = AEROVNET_NBL_GET_STATUS(Nbl);
        if (nblStatus == NDIS_STATUS_SUCCESS) {
            AEROVNET_NBL_SET_STATUS(Nbl, TxStatus);
        }
    }

    pending = AEROVNET_NBL_GET_PENDING(Nbl);
    pending--;
    AEROVNET_NBL_SET_PENDING(Nbl, pending);

    if (pending == 0) {
        finalStatus = AEROVNET_NBL_GET_STATUS(Nbl);
        AEROVNET_NBL_SET_PENDING(Nbl, 0);
        AEROVNET_NBL_SET_STATUS(Nbl, NDIS_STATUS_SUCCESS);

        NET_BUFFER_LIST_NEXT_NBL(Nbl) = NULL;
        if (*CompleteNblTail) {
            NET_BUFFER_LIST_NEXT_NBL(*CompleteNblTail) = Nbl;
            *CompleteNblTail = Nbl;
        } else {
            *CompleteNblHead = Nbl;
            *CompleteNblTail = Nbl;
        }

        NET_BUFFER_LIST_STATUS(Nbl) = finalStatus;
    }
}

static VOID AerovNetCompleteTxRequest(
    _Inout_ AEROVNET_ADAPTER* Adapter,
    _Inout_ AEROVNET_TX_REQUEST* TxReq,
    _In_ NDIS_STATUS TxStatus,
    _Inout_ PNET_BUFFER_LIST* CompleteNblHead,
    _Inout_ PNET_BUFFER_LIST* CompleteNblTail)
{
    if (!TxReq || !TxReq->Nbl) {
        return;
    }

    AerovNetTxNblCompleteOneNetBufferLocked(Adapter, TxReq->Nbl, TxStatus, CompleteNblHead, CompleteNblTail);
}

static BOOLEAN AerovNetIsBroadcastAddress(_In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* Mac)
{
    ULONG i;
    for (i = 0; i < ETH_LENGTH_OF_ADDRESS; i++) {
        if (Mac[i] != 0xFF) {
            return FALSE;
        }
    }
    return TRUE;
}

static BOOLEAN AerovNetMacEqual(_In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* A, _In_reads_(ETH_LENGTH_OF_ADDRESS) const UCHAR* B)
{
    return (RtlCompareMemory(A, B, ETH_LENGTH_OF_ADDRESS) == ETH_LENGTH_OF_ADDRESS) ? TRUE : FALSE;
}

static BOOLEAN AerovNetAcceptFrame(_In_ const AEROVNET_ADAPTER* Adapter, _In_reads_bytes_(FrameLen) const UCHAR* Frame, _In_ ULONG FrameLen)
{
    const UCHAR* dst;
    ULONG filter;

    if (FrameLen < AEROVNET_MIN_FRAME_SIZE) {
        return FALSE;
    }

    filter = Adapter->PacketFilter;
    if (filter == 0) {
        return FALSE;
    }

    if (filter & NDIS_PACKET_TYPE_PROMISCUOUS) {
        return TRUE;
    }

    dst = Frame;

    if (AerovNetIsBroadcastAddress(dst)) {
        return (filter & NDIS_PACKET_TYPE_BROADCAST) ? TRUE : FALSE;
    }

    if (dst[0] & 0x01) {
        if (filter & NDIS_PACKET_TYPE_ALL_MULTICAST) {
            return TRUE;
        }

        if (filter & NDIS_PACKET_TYPE_MULTICAST) {
            ULONG i;
            for (i = 0; i < Adapter->MulticastListSize; i++) {
                if (AerovNetMacEqual(dst, Adapter->MulticastList[i])) {
                    return TRUE;
                }
            }
        }

        return FALSE;
    }

    /* Unicast. */
    if ((filter & NDIS_PACKET_TYPE_DIRECTED) == 0) {
        return FALSE;
    }

    return AerovNetMacEqual(dst, Adapter->CurrentMac) ? TRUE : FALSE;
}

static VOID AerovNetGenerateFallbackMac(_Out_writes_(ETH_LENGTH_OF_ADDRESS) UCHAR* Mac)
{
    LARGE_INTEGER t;

    KeQuerySystemTime(&t);

    /* Locally administered, unicast. */
    Mac[0] = 0x02;
    Mac[1] = (UCHAR)(t.LowPart & 0xFF);
    Mac[2] = (UCHAR)((t.LowPart >> 8) & 0xFF);
    Mac[3] = (UCHAR)((t.LowPart >> 16) & 0xFF);
    Mac[4] = (UCHAR)((t.LowPart >> 24) & 0xFF);
    Mac[5] = (UCHAR)(t.HighPart & 0xFF);
}

/* -------------------------------------------------------------------------- */
/* PCI / transport helpers (virtio-pci modern)                                 */
/* -------------------------------------------------------------------------- */

static NTSTATUS AerovNetQueryInterfaceCompletion(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Irp);

    if (Context != NULL) {
        KeSetEvent((PKEVENT)Context, IO_NO_INCREMENT, FALSE);
    }

    /* We own the IRP and will free it after the wait. */
    return STATUS_MORE_PROCESSING_REQUIRED;
}

static NTSTATUS AerovNetAcquirePciInterface(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    PDEVICE_OBJECT pdo;
    PDEVICE_OBJECT fdo;
    PDEVICE_OBJECT next;
    KEVENT event;
    PIRP irp;
    IO_STATUS_BLOCK iosb;
    PIO_STACK_LOCATION irpSp;
    NTSTATUS status;

    if (Adapter->PciInterfaceAcquired) {
        return STATUS_SUCCESS;
    }

    RtlZeroMemory(&Adapter->PciInterface, sizeof(Adapter->PciInterface));

    pdo = NULL;
    fdo = NULL;
    next = NULL;
    NdisMGetDeviceProperty(Adapter->MiniportAdapterHandle, &pdo, &fdo, &next, NULL, NULL);

    if (next == NULL) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    KeInitializeEvent(&event, NotificationEvent, FALSE);

    irp = IoAllocateIrp(next->StackSize, FALSE);
    if (irp == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(&iosb, sizeof(iosb));
    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    irp->IoStatus.Information = 0;
    irp->UserIosb = &iosb;
    irp->UserEvent = &event;
    irp->Tail.Overlay.Thread = PsGetCurrentThread();
    irp->RequestorMode = KernelMode;
    irp->Flags = IRP_SYNCHRONOUS_API;

    irpSp = IoGetNextIrpStackLocation(irp);
    irpSp->MajorFunction = IRP_MJ_PNP;
    irpSp->MinorFunction = IRP_MN_QUERY_INTERFACE;
    irpSp->Parameters.QueryInterface.InterfaceType = (LPGUID)&GUID_PCI_BUS_INTERFACE_STANDARD;
    irpSp->Parameters.QueryInterface.Size = sizeof(PCI_BUS_INTERFACE_STANDARD);
    irpSp->Parameters.QueryInterface.Version = PCI_BUS_INTERFACE_STANDARD_VERSION;
    irpSp->Parameters.QueryInterface.Interface = (PINTERFACE)&Adapter->PciInterface;
    irpSp->Parameters.QueryInterface.InterfaceSpecificData = NULL;

    IoSetCompletionRoutine(irp, AerovNetQueryInterfaceCompletion, &event, TRUE, TRUE, TRUE);

    status = IoCallDriver(next, irp);
    if (status == STATUS_PENDING) {
        (VOID)KeWaitForSingleObject(&event, Executive, KernelMode, FALSE, NULL);
    }

    status = irp->IoStatus.Status;
    IoFreeIrp(irp);

    if (!NT_SUCCESS(status)) {
        RtlZeroMemory(&Adapter->PciInterface, sizeof(Adapter->PciInterface));
        return status;
    }

    if (Adapter->PciInterface.InterfaceReference != NULL) {
        Adapter->PciInterface.InterfaceReference(Adapter->PciInterface.Context);
    }

    Adapter->PciInterfaceAcquired = TRUE;
    return STATUS_SUCCESS;
}

static VOID AerovNetReleasePciInterface(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    if (!Adapter->PciInterfaceAcquired) {
        return;
    }

    if (Adapter->PciInterface.InterfaceDereference != NULL) {
        Adapter->PciInterface.InterfaceDereference(Adapter->PciInterface.Context);
    }

    Adapter->PciInterfaceAcquired = FALSE;
    RtlZeroMemory(&Adapter->PciInterface, sizeof(Adapter->PciInterface));
}

static ULONG AerovNetPciReadConfig(
    _In_ const AEROVNET_ADAPTER* Adapter,
    _Out_writes_bytes_(Length) PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length)
{
#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

    if (Adapter->PciInterface.ReadConfig == NULL) {
        return 0;
    }

    return Adapter->PciInterface.ReadConfig(Adapter->PciInterface.Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
}

static NTSTATUS AerovNetReadBarBases(_Inout_ AEROVNET_ADAPTER* Adapter, _Out_writes_(VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT) ULONGLONG* BarBasesOut)
{
    ULONG barRegs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    ULONG bytesRead;
    ULONG i;

    RtlZeroMemory(BarBasesOut, sizeof(ULONGLONG) * VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT);
    RtlZeroMemory(barRegs, sizeof(barRegs));

    bytesRead = AerovNetPciReadConfig(Adapter, barRegs, 0x10, sizeof(barRegs));
    if (bytesRead != sizeof(barRegs)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        ULONG val = barRegs[i];
        if (val == 0) {
            continue;
        }

        if ((val & 0x1u) != 0) {
            /* I/O BAR. Not supported by Aero contract v1. */
            BarBasesOut[i] = (ULONGLONG)(val & ~0x3u);
            continue;
        }

        /* Memory BAR. */
        {
            ULONG memType = (val >> 1) & 0x3u;
            if (memType == 0x2u) {
                /* 64-bit BAR uses this and the next BAR dword. */
                ULONGLONG base;
                ULONG high;

                if (i == (VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT - 1)) {
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                high = barRegs[i + 1];
                base = ((ULONGLONG)high << 32) | (ULONGLONG)(val & ~0xFu);
                BarBasesOut[i] = base;
                /* Skip high dword slot. */
                i++;
            } else {
                BarBasesOut[i] = (ULONGLONG)(val & ~0xFu);
            }
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS AerovNetInitModernTransport(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    UCHAR cfg[256];
    ULONGLONG barBases[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    ULONG bytesRead;
    USHORT vendorId;
    USHORT deviceId;
    UCHAR revisionId;
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseRes;

    if (Adapter->Bar0Va == NULL || Adapter->Bar0Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Adapter->Bar0Length < 0x4000u) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    bytesRead = AerovNetPciReadConfig(Adapter, cfg, 0, sizeof(cfg));
    if (bytesRead != sizeof(cfg)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    vendorId = (USHORT)(cfg[0] | ((USHORT)cfg[1] << 8));
    deviceId = (USHORT)(cfg[2] | ((USHORT)cfg[3] << 8));
    revisionId = cfg[0x08];

    if (vendorId != AEROVNET_PCI_VENDOR_ID || deviceId != AEROVNET_PCI_DEVICE_ID) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (revisionId != AEROVNET_PCI_REVISION_ID_V1) {
        return STATUS_NOT_SUPPORTED;
    }

    RtlZeroMemory(barBases, sizeof(barBases));
    {
        NTSTATUS st = AerovNetReadBarBases(Adapter, barBases);
        if (!NT_SUCCESS(st)) {
            return st;
        }
    }

    /* Ensure the BAR0 base address matches the CM resource BAR0 mapping. */
    if (barBases[0] == 0 || (ULONGLONG)Adapter->Bar0Pa.QuadPart != barBases[0]) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    RtlZeroMemory(&caps, sizeof(caps));
    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), barBases, &caps);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    /* Contract checks: fixed notify multiplier and BAR0-only layout. */
    if (caps.notify_off_multiplier != 4u) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (caps.common_cfg.bar != 0 || caps.notify_cfg.bar != 0 || caps.isr_cfg.bar != 0 || caps.device_cfg.bar != 0) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if ((ULONGLONG)caps.common_cfg.offset + (ULONGLONG)caps.common_cfg.length > (ULONGLONG)Adapter->Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if ((ULONGLONG)caps.notify_cfg.offset + (ULONGLONG)caps.notify_cfg.length > (ULONGLONG)Adapter->Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if ((ULONGLONG)caps.isr_cfg.offset + (ULONGLONG)caps.isr_cfg.length > (ULONGLONG)Adapter->Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }
    if ((ULONGLONG)caps.device_cfg.offset + (ULONGLONG)caps.device_cfg.length > (ULONGLONG)Adapter->Bar0Length) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    Adapter->CommonCfg = (volatile virtio_pci_common_cfg*)(Adapter->Bar0Va + caps.common_cfg.offset);
    Adapter->NotifyBase = (volatile UCHAR*)(Adapter->Bar0Va + caps.notify_cfg.offset);
    Adapter->NotifyOffMultiplier = caps.notify_off_multiplier;
    Adapter->IsrStatus = (volatile UCHAR*)(Adapter->Bar0Va + caps.isr_cfg.offset);
    Adapter->DeviceCfg = (volatile UCHAR*)(Adapter->Bar0Va + caps.device_cfg.offset);

    return STATUS_SUCCESS;
}

static VOID AerovNetVirtioResetDevice(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    ULONG waitedUs;

    if (Adapter->CommonCfg == NULL) {
        return;
    }

    WRITE_REGISTER_UCHAR(&Adapter->CommonCfg->device_status, 0);

    /* Poll for reset completion (bounded). */
    for (waitedUs = 0; waitedUs < 1000000u; waitedUs += 1000u) {
        if (READ_REGISTER_UCHAR(&Adapter->CommonCfg->device_status) == 0) {
            return;
        }
        KeStallExecutionProcessor(1000u);
    }
}

static __forceinline VOID AerovNetVirtioAddStatus(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ UCHAR Bits)
{
    UCHAR st;

    if (Adapter->CommonCfg == NULL) {
        return;
    }

    st = READ_REGISTER_UCHAR(&Adapter->CommonCfg->device_status);
    st |= Bits;
    WRITE_REGISTER_UCHAR(&Adapter->CommonCfg->device_status, st);
}

static __forceinline UCHAR AerovNetVirtioGetStatus(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    if (Adapter->CommonCfg == NULL) {
        return 0;
    }
    return READ_REGISTER_UCHAR(&Adapter->CommonCfg->device_status);
}

static __forceinline VOID AerovNetVirtioFailDevice(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_FAILED);
}

static UINT64 AerovNetVirtioReadDeviceFeatures(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    UINT32 lo;
    UINT32 hi;

    if (Adapter->CommonCfg == NULL) {
        return 0;
    }

    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->device_feature_select, 0);
    lo = READ_REGISTER_ULONG(&Adapter->CommonCfg->device_feature);
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->device_feature_select, 1);
    hi = READ_REGISTER_ULONG(&Adapter->CommonCfg->device_feature);

    return ((UINT64)hi << 32) | (UINT64)lo;
}

static VOID AerovNetVirtioWriteDriverFeatures(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ UINT64 Features)
{
    UINT32 lo;
    UINT32 hi;

    if (Adapter->CommonCfg == NULL) {
        return;
    }

    lo = (UINT32)(Features & 0xFFFFFFFFull);
    hi = (UINT32)((Features >> 32) & 0xFFFFFFFFull);

    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->driver_feature_select, 0);
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->driver_feature, lo);
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->driver_feature_select, 1);
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->driver_feature, hi);
}

static NTSTATUS AerovNetVirtioReadDeviceConfigStable(
    _Inout_ AEROVNET_ADAPTER* Adapter,
    _In_ ULONG Offset,
    _Out_writes_bytes_(Length) PVOID Buffer,
    _In_ ULONG Length)
{
    ULONG attempt;

    if (Adapter->CommonCfg == NULL || Adapter->DeviceCfg == NULL || Buffer == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Use config_generation retry logic (bounded) to avoid torn reads if the
     * device updates config concurrently.
     */
    for (attempt = 0; attempt < 10; attempt++) {
        UCHAR gen1 = READ_REGISTER_UCHAR(&Adapter->CommonCfg->config_generation);
        READ_REGISTER_BUFFER_UCHAR((PUCHAR)(Adapter->DeviceCfg + Offset), (PUCHAR)Buffer, Length);
        UCHAR gen2 = READ_REGISTER_UCHAR(&Adapter->CommonCfg->config_generation);
        if (gen1 == gen2) {
            return STATUS_SUCCESS;
        }
    }

    return STATUS_DEVICE_DATA_ERROR;
}

static NTSTATUS AerovNetReadMacAndLinkState(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    VIRTIO_NET_CONFIG cfg;
    NTSTATUS st;

    RtlZeroMemory(&cfg, sizeof(cfg));

    st = AerovNetVirtioReadDeviceConfigStable(Adapter, 0, &cfg, sizeof(cfg));
    if (!NT_SUCCESS(st)) {
        return st;
    }

    if (cfg.MaxVirtqueuePairs != 1) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (AerovNetIsBroadcastAddress(cfg.Mac) || (cfg.Mac[0] & 0x01) != 0) {
        /* Defensive: ensure we expose a unicast MAC even if device is misconfigured. */
        AerovNetGenerateFallbackMac(cfg.Mac);
    }

    RtlCopyMemory(Adapter->PermanentMac, cfg.Mac, ETH_LENGTH_OF_ADDRESS);
    RtlCopyMemory(Adapter->CurrentMac, cfg.Mac, ETH_LENGTH_OF_ADDRESS);
    Adapter->LinkUp = (cfg.Status & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;

    return STATUS_SUCCESS;
}

static __forceinline VOID AerovNetNotifyQueue(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ const AEROVNET_VQ* Q)
{
    UNREFERENCED_PARAMETER(Adapter);

    if (Q == NULL || Q->NotifyAddr == NULL) {
        return;
    }

    WRITE_REGISTER_USHORT((volatile USHORT*)Q->NotifyAddr, (USHORT)Q->QueueIndex);
}

static NDIS_STATUS AerovNetParseResources(_Inout_ AEROVNET_ADAPTER* Adapter, _In_ PNDIS_RESOURCE_LIST Resources)
{
    ULONG i;
    NDIS_STATUS status;

    Adapter->Bar0Va = NULL;
    Adapter->Bar0Length = 0;
    Adapter->Bar0Pa.QuadPart = 0;

    if (Resources == NULL) {
        return NDIS_STATUS_RESOURCES;
    }

    for (i = 0; i < Resources->Count; i++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = &Resources->PartialDescriptors[i];
        if (desc->Type == CmResourceTypeMemory) {
            /* Contract: BAR0 MMIO is 0x4000 bytes. */
            if (desc->u.Memory.Length < 0x4000u) {
                continue;
            }

            Adapter->Bar0Pa = desc->u.Memory.Start;
            Adapter->Bar0Length = desc->u.Memory.Length;
            break;
        }
    }

    if (Adapter->Bar0Length == 0) {
        return NDIS_STATUS_RESOURCES;
    }

    status = NdisMMapIoSpace((PVOID*)&Adapter->Bar0Va, Adapter->MiniportAdapterHandle, Adapter->Bar0Pa, Adapter->Bar0Length);
    if (status != NDIS_STATUS_SUCCESS) {
        Adapter->Bar0Va = NULL;
        Adapter->Bar0Length = 0;
        Adapter->Bar0Pa.QuadPart = 0;
        return status;
    }

    return NDIS_STATUS_SUCCESS;
}

/* -------------------------------------------------------------------------- */
/* Queue / buffer management                                                  */
/* -------------------------------------------------------------------------- */

static VOID AerovNetFreeRxBuffer(_Inout_ AEROVNET_RX_BUFFER* Rx)
{
    if (Rx->Nbl) {
        NdisFreeNetBufferList(Rx->Nbl);
        Rx->Nbl = NULL;
        Rx->Nb = NULL;
    }

    if (Rx->Mdl) {
        IoFreeMdl(Rx->Mdl);
        Rx->Mdl = NULL;
    }

    if (Rx->BufferVa) {
        MmFreeContiguousMemory(Rx->BufferVa);
        Rx->BufferVa = NULL;
    }
}

static VOID AerovNetFreeTxResources(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    ULONG i;

    if (Adapter->TxRequests) {
        for (i = 0; i < Adapter->TxRequestCount; i++) {
            /* SG lists are owned by NDIS; if any request is still holding one, we cannot safely free it here. */
            Adapter->TxRequests[i].SgList = NULL;
        }

        ExFreePoolWithTag(Adapter->TxRequests, AEROVNET_TAG);
        Adapter->TxRequests = NULL;
    }

    Adapter->TxRequestCount = 0;
    InitializeListHead(&Adapter->TxFreeList);
    InitializeListHead(&Adapter->TxAwaitingSgList);
    InitializeListHead(&Adapter->TxPendingList);
    InitializeListHead(&Adapter->TxSubmittedList);

    if (Adapter->TxHeaderBlockVa) {
        MmFreeContiguousMemory(Adapter->TxHeaderBlockVa);
        Adapter->TxHeaderBlockVa = NULL;
        Adapter->TxHeaderBlockBytes = 0;
        Adapter->TxHeaderBlockPa.QuadPart = 0;
    }
}

static VOID AerovNetFreeRxResources(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    ULONG i;

    if (Adapter->RxBuffers) {
        for (i = 0; i < Adapter->RxBufferCount; i++) {
            AerovNetFreeRxBuffer(&Adapter->RxBuffers[i]);
        }

        ExFreePoolWithTag(Adapter->RxBuffers, AEROVNET_TAG);
        Adapter->RxBuffers = NULL;
    }

    Adapter->RxBufferCount = 0;
    InitializeListHead(&Adapter->RxFreeList);
}

static VOID AerovNetFreeVq(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Q)
{
    if (Adapter == NULL || Q == NULL) {
        return;
    }

    if (Q->RingVa != NULL && Q->RingBytes != 0) {
        NdisMFreeSharedMemory(Adapter->MiniportAdapterHandle, Q->RingBytes, AEROVNET_DMA_CACHED, Q->RingVa, Q->RingPa);
    }
    if (Q->IndirectVa != NULL && Q->IndirectBytes != 0) {
        NdisMFreeSharedMemory(
            Adapter->MiniportAdapterHandle, Q->IndirectBytes, AEROVNET_DMA_CACHED, Q->IndirectVa, Q->IndirectPa);
    }
    if (Q->Vq != NULL) {
        ExFreePoolWithTag(Q->Vq, AEROVNET_TAG);
    }

    RtlZeroMemory(Q, sizeof(*Q));
}

static VOID AerovNetCleanupAdapter(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

    /*
     * Best-effort quiesce.
     *
     * MiniportInitializeEx is responsible for freeing resources on failure;
     * NDIS will not necessarily invoke HaltEx. Reset the device here to stop
     * DMA/interrupts before we tear down shared memory.
     */
    if (Adapter->CommonCfg != NULL) {
        AerovNetVirtioResetDevice(Adapter);
    }

    AerovNetFreeTxResources(Adapter);
    AerovNetFreeRxResources(Adapter);

    AerovNetFreeVq(Adapter, &Adapter->RxQ);
    AerovNetFreeVq(Adapter, &Adapter->TxQ);

    if (Adapter->NblPool) {
        NdisFreeNetBufferListPool(Adapter->NblPool);
        Adapter->NblPool = NULL;
    }

    if (Adapter->DmaHandle) {
        NdisMDeregisterScatterGatherDma(Adapter->DmaHandle);
        Adapter->DmaHandle = NULL;
    }

    if (Adapter->InterruptHandle) {
        NdisMDeregisterInterruptEx(Adapter->InterruptHandle);
        Adapter->InterruptHandle = NULL;
    }

    if (Adapter->Bar0Va) {
        NdisMUnmapIoSpace(Adapter->MiniportAdapterHandle, Adapter->Bar0Va, Adapter->Bar0Length);
        Adapter->Bar0Va = NULL;
        Adapter->Bar0Length = 0;
        Adapter->Bar0Pa.QuadPart = 0;
    }

    AerovNetReleasePciInterface(Adapter);

    NdisFreeSpinLock(&Adapter->Lock);

    ExFreePoolWithTag(Adapter, AEROVNET_TAG);
}

static VOID AerovNetFillRxQueueLocked(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    BOOLEAN added;

    if (Adapter->RxQ.Vq == NULL) {
        return;
    }

    added = FALSE;

    while (!IsListEmpty(&Adapter->RxFreeList)) {
        PLIST_ENTRY entry;
        AEROVNET_RX_BUFFER* rx;
        VIRTQ_SG sg[2];
        UINT16 head;
        NTSTATUS st;

        entry = RemoveHeadList(&Adapter->RxFreeList);
        rx = CONTAINING_RECORD(entry, AEROVNET_RX_BUFFER, Link);

        rx->Indicated = FALSE;

        sg[0].addr = (UINT64)rx->BufferPa.QuadPart;
        sg[0].len = AEROVNET_NET_HDR_LEN;
        sg[0].write = TRUE;

        sg[1].addr = (UINT64)(rx->BufferPa.QuadPart + AEROVNET_NET_HDR_LEN);
        sg[1].len = (UINT32)(rx->BufferBytes - AEROVNET_NET_HDR_LEN);
        sg[1].write = TRUE;

        st = VirtqSplitAddBuffer(Adapter->RxQ.Vq, sg, 2, rx, &head);
        if (!NT_SUCCESS(st)) {
            /* Put it back and stop trying for now. */
            InsertHeadList(&Adapter->RxFreeList, &rx->Link);
            break;
        }

        VirtqSplitPublish(Adapter->RxQ.Vq, head);
        added = TRUE;
    }

    if (added) {
        BOOLEAN kick = VirtqSplitKickPrepare(Adapter->RxQ.Vq);
        if (kick) {
            AerovNetNotifyQueue(Adapter, &Adapter->RxQ);
        }
        VirtqSplitKickCommit(Adapter->RxQ.Vq);
    }
}

static VOID AerovNetFlushTxPendingLocked(
    _Inout_ AEROVNET_ADAPTER* Adapter,
    _Inout_ PLIST_ENTRY CompleteTxReqs,
    _Inout_ PNET_BUFFER_LIST* CompleteNblHead,
    _Inout_ PNET_BUFFER_LIST* CompleteNblTail)
{
    VIRTQ_SG sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
    BOOLEAN submitted;

    if (Adapter->TxQ.Vq == NULL) {
        return;
    }

    submitted = FALSE;

    while (!IsListEmpty(&Adapter->TxPendingList)) {
        AEROVNET_TX_REQUEST* txReq;
        ULONG elemCount;
        ULONG i;
        UINT16 sgCount;
        NTSTATUS st;

        txReq = CONTAINING_RECORD(Adapter->TxPendingList.Flink, AEROVNET_TX_REQUEST, Link);

        if (txReq->Cancelled) {
            RemoveEntryList(&txReq->Link);
            InsertTailList(CompleteTxReqs, &txReq->Link);
            AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_REQUEST_ABORTED, CompleteNblHead, CompleteNblTail);
            continue;
        }

        if (txReq->SgList == NULL) {
            RemoveEntryList(&txReq->Link);
            InsertTailList(CompleteTxReqs, &txReq->Link);
            AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_FAILURE, CompleteNblHead, CompleteNblTail);
            continue;
        }

        elemCount = txReq->SgList->NumberOfElements;
        if (elemCount > AEROVNET_MAX_TX_SG_ELEMENTS) {
            RemoveEntryList(&txReq->Link);
            InsertTailList(CompleteTxReqs, &txReq->Link);
            AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_BUFFER_OVERFLOW, CompleteNblHead, CompleteNblTail);
            continue;
        }

        /* Build virtio-net header: 10 bytes, all fields zero (no offloads). */
        RtlZeroMemory(txReq->HeaderVa, AEROVNET_NET_HDR_LEN);

        sg[0].addr = (UINT64)txReq->HeaderPa.QuadPart;
        sg[0].len = AEROVNET_NET_HDR_LEN;
        sg[0].write = FALSE;

        for (i = 0; i < elemCount; i++) {
            sg[1 + i].addr = (UINT64)txReq->SgList->Elements[i].Address.QuadPart;
            sg[1 + i].len = txReq->SgList->Elements[i].Length;
            sg[1 + i].write = FALSE;
        }

        sgCount = (UINT16)(elemCount + 1);

        st = VirtqSplitAddBuffer(Adapter->TxQ.Vq, sg, sgCount, txReq, &txReq->DescHeadId);
        if (st == STATUS_INSUFFICIENT_RESOURCES) {
            /* Out of descriptors/indirect tables; keep queued. */
            break;
        }
        if (!NT_SUCCESS(st)) {
            RemoveEntryList(&txReq->Link);
            InsertTailList(CompleteTxReqs, &txReq->Link);
            AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_FAILURE, CompleteNblHead, CompleteNblTail);
            continue;
        }

        RemoveEntryList(&txReq->Link);

        VirtqSplitPublish(Adapter->TxQ.Vq, txReq->DescHeadId);
        txReq->State = AerovNetTxSubmitted;
        InsertTailList(&Adapter->TxSubmittedList, &txReq->Link);
        submitted = TRUE;
    }

    if (submitted) {
        BOOLEAN kick = VirtqSplitKickPrepare(Adapter->TxQ.Vq);
        if (kick) {
            AerovNetNotifyQueue(Adapter, &Adapter->TxQ);
        }
        VirtqSplitKickCommit(Adapter->TxQ.Vq);
    }
}

static NDIS_STATUS AerovNetAllocateRxResources(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    ULONG i;
    PHYSICAL_ADDRESS low = {0};
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS skip = {0};

    high.QuadPart = ~0ull;

    InitializeListHead(&Adapter->RxFreeList);
    Adapter->RxBufferCount = Adapter->RxQ.QueueSize;

    Adapter->RxBuffers = (AEROVNET_RX_BUFFER*)ExAllocatePoolWithTag(
        NonPagedPool, sizeof(AEROVNET_RX_BUFFER) * Adapter->RxBufferCount, AEROVNET_TAG);
    if (!Adapter->RxBuffers) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Adapter->RxBuffers, sizeof(AEROVNET_RX_BUFFER) * Adapter->RxBufferCount);

    for (i = 0; i < Adapter->RxBufferCount; i++) {
        AEROVNET_RX_BUFFER* rx = &Adapter->RxBuffers[i];

        rx->BufferBytes = Adapter->RxBufferTotalBytes;
        rx->BufferVa = MmAllocateContiguousMemorySpecifyCache(rx->BufferBytes, low, high, skip, MmCached);
        if (!rx->BufferVa) {
            return NDIS_STATUS_RESOURCES;
        }

        rx->BufferPa = MmGetPhysicalAddress(rx->BufferVa);

        rx->Mdl = IoAllocateMdl(rx->BufferVa, rx->BufferBytes, FALSE, FALSE, NULL);
        if (!rx->Mdl) {
            return NDIS_STATUS_RESOURCES;
        }
        MmBuildMdlForNonPagedPool(rx->Mdl);

        rx->Nbl = NdisAllocateNetBufferAndNetBufferList(Adapter->NblPool, 0, 0, rx->Mdl, AEROVNET_NET_HDR_LEN, 0);
        if (!rx->Nbl) {
            return NDIS_STATUS_RESOURCES;
        }

        rx->Nb = NET_BUFFER_LIST_FIRST_NB(rx->Nbl);
        rx->Indicated = FALSE;

        rx->Nbl->MiniportReserved[0] = rx;

        InsertTailList(&Adapter->RxFreeList, &rx->Link);
    }

    return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetAllocateTxResources(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    ULONG i;
    PHYSICAL_ADDRESS low = {0};
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS skip = {0};

    high.QuadPart = ~0ull;

    InitializeListHead(&Adapter->TxFreeList);
    InitializeListHead(&Adapter->TxAwaitingSgList);
    InitializeListHead(&Adapter->TxPendingList);
    InitializeListHead(&Adapter->TxSubmittedList);

    Adapter->TxRequestCount = Adapter->TxQ.QueueSize;
    Adapter->TxRequests = (AEROVNET_TX_REQUEST*)ExAllocatePoolWithTag(
        NonPagedPool, sizeof(AEROVNET_TX_REQUEST) * Adapter->TxRequestCount, AEROVNET_TAG);
    if (!Adapter->TxRequests) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Adapter->TxRequests, sizeof(AEROVNET_TX_REQUEST) * Adapter->TxRequestCount);

    Adapter->TxHeaderBlockBytes = AEROVNET_NET_HDR_LEN * Adapter->TxRequestCount;
    Adapter->TxHeaderBlockVa = MmAllocateContiguousMemorySpecifyCache(Adapter->TxHeaderBlockBytes, low, high, skip, MmCached);
    if (!Adapter->TxHeaderBlockVa) {
        return NDIS_STATUS_RESOURCES;
    }
    Adapter->TxHeaderBlockPa = MmGetPhysicalAddress(Adapter->TxHeaderBlockVa);
    RtlZeroMemory(Adapter->TxHeaderBlockVa, Adapter->TxHeaderBlockBytes);

    for (i = 0; i < Adapter->TxRequestCount; i++) {
        AEROVNET_TX_REQUEST* tx = &Adapter->TxRequests[i];
        RtlZeroMemory(tx, sizeof(*tx));

        tx->State = AerovNetTxFree;
        tx->Cancelled = FALSE;
        tx->Adapter = Adapter;
        tx->HeaderVa = Adapter->TxHeaderBlockVa + (AEROVNET_NET_HDR_LEN * i);
        tx->HeaderPa.QuadPart = Adapter->TxHeaderBlockPa.QuadPart + (AEROVNET_NET_HDR_LEN * i);
        InsertTailList(&Adapter->TxFreeList, &tx->Link);
    }

    return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetSetupQueue(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ AEROVNET_VQ* Q, _In_ USHORT QueueIndex, _In_ BOOLEAN ForceIndirect)
{
    USHORT qsz;
    USHORT notifyOff;
    size_t vqStateBytes;
    size_t ringBytes;
    ULONG indirectMaxDesc;
    ULONG indirectTables;
    ULONG indirectBytes;
    NTSTATUS st;

    if (Adapter->CommonCfg == NULL || Adapter->NotifyBase == NULL) {
        return NDIS_STATUS_FAILURE;
    }

    RtlZeroMemory(Q, sizeof(*Q));
    Q->QueueIndex = QueueIndex;

    /* Queue selector operations must be serialized. We use the adapter lock. */
    WRITE_REGISTER_USHORT(&Adapter->CommonCfg->queue_select, QueueIndex);
    (VOID)READ_REGISTER_USHORT(&Adapter->CommonCfg->queue_select);

    qsz = READ_REGISTER_USHORT(&Adapter->CommonCfg->queue_size);
    if (qsz != AEROVNET_QUEUE_SIZE) {
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    notifyOff = READ_REGISTER_USHORT(&Adapter->CommonCfg->queue_notify_off);
    if (notifyOff != QueueIndex) {
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    Q->QueueSize = qsz;
    Q->NotifyOff = notifyOff;
    Q->NotifyAddr = (volatile UINT16*)(Adapter->NotifyBase + ((ULONG)notifyOff * Adapter->NotifyOffMultiplier));

    /* Allocate split virtqueue state. */
    vqStateBytes = VirtqSplitStateSize(qsz);
    Q->Vq = (VIRTQ_SPLIT*)ExAllocatePoolWithTag(NonPagedPool, vqStateBytes, AEROVNET_TAG);
    if (Q->Vq == NULL) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Q->Vq, vqStateBytes);

    ringBytes = VirtqSplitRingMemSize(qsz, AEROVNET_VQ_ALIGN, FALSE);
    if (ringBytes == 0 || ringBytes > MAXULONG) {
        return NDIS_STATUS_FAILURE;
    }
    Q->RingBytes = (ULONG)ringBytes;
    NdisMAllocateSharedMemory(Adapter->MiniportAdapterHandle, Q->RingBytes, AEROVNET_DMA_CACHED, &Q->RingVa, &Q->RingPa);
    if (Q->RingVa == NULL) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Q->RingVa, Q->RingBytes);

    /* Indirect descriptor pool: one table per in-flight request. */
    indirectMaxDesc = (QueueIndex == AEROVNET_QUEUE_TX) ? (AEROVNET_MAX_TX_SG_ELEMENTS + 1u) : 2u;
    indirectTables = qsz;
    indirectBytes = sizeof(VIRTQ_DESC) * indirectMaxDesc * indirectTables;
    if (indirectBytes == 0 || indirectBytes > MAXULONG) {
        return NDIS_STATUS_FAILURE;
    }

    Q->IndirectBytes = indirectBytes;
    NdisMAllocateSharedMemory(
        Adapter->MiniportAdapterHandle, Q->IndirectBytes, AEROVNET_DMA_CACHED, &Q->IndirectVa, &Q->IndirectPa);
    if (Q->IndirectVa == NULL) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(Q->IndirectVa, Q->IndirectBytes);

    st = VirtqSplitInit(
        Q->Vq,
        qsz,
        FALSE,
        TRUE,
        Q->RingVa,
        (UINT64)Q->RingPa.QuadPart,
        AEROVNET_VQ_ALIGN,
        Q->IndirectVa,
        (UINT64)Q->IndirectPa.QuadPart,
        (UINT16)indirectTables,
        (UINT16)indirectMaxDesc);
    if (!NT_SUCCESS(st)) {
        return NDIS_STATUS_FAILURE;
    }

    if (ForceIndirect) {
        /* Force indirect even for 2-element SG lists so we can keep the ring full. */
        Q->Vq->indirect_threshold = 1;
    }

    /* Program queue addresses and enable. */
    WRITE_REGISTER_USHORT(&Adapter->CommonCfg->queue_select, QueueIndex);
    (VOID)READ_REGISTER_USHORT(&Adapter->CommonCfg->queue_select);

    WRITE_REGISTER_USHORT(&Adapter->CommonCfg->queue_msix_vector, VIRTIO_MSI_NO_VECTOR);

    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_desc_lo, (ULONG)(Q->Vq->desc_pa & 0xFFFFFFFFull));
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_desc_hi, (ULONG)((Q->Vq->desc_pa >> 32) & 0xFFFFFFFFull));

    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_avail_lo, (ULONG)(Q->Vq->avail_pa & 0xFFFFFFFFull));
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_avail_hi, (ULONG)((Q->Vq->avail_pa >> 32) & 0xFFFFFFFFull));

    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_used_lo, (ULONG)(Q->Vq->used_pa & 0xFFFFFFFFull));
    WRITE_REGISTER_ULONG(&Adapter->CommonCfg->queue_used_hi, (ULONG)((Q->Vq->used_pa >> 32) & 0xFFFFFFFFull));

    WRITE_REGISTER_USHORT(&Adapter->CommonCfg->queue_enable, 1);

    return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetVirtioStart(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    NTSTATUS st;
    UCHAR devStatus;
    UINT64 requiredFeatures;
    UINT64 negotiated;

    st = AerovNetAcquirePciInterface(Adapter);
    if (!NT_SUCCESS(st)) {
        return NDIS_STATUS_FAILURE;
    }

    st = AerovNetInitModernTransport(Adapter);
    if (!NT_SUCCESS(st)) {
        return NDIS_STATUS_FAILURE;
    }

    /* Reset + start negotiation. */
    AerovNetVirtioResetDevice(Adapter);
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_ACKNOWLEDGE);
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_DRIVER);

    Adapter->HostFeatures = AerovNetVirtioReadDeviceFeatures(Adapter);

    requiredFeatures = VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;
    if ((Adapter->HostFeatures & requiredFeatures) != requiredFeatures) {
        AerovNetVirtioFailDevice(Adapter);
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    /*
     * Contract v1: negotiate only the required bits.
     * Do NOT negotiate mergeable RX, offloads, CTRL_VQ, EVENT_IDX, etc.
     */
    negotiated = requiredFeatures;
    Adapter->GuestFeatures = negotiated;

    AerovNetVirtioWriteDriverFeatures(Adapter, negotiated);
    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_FEATURES_OK);

    devStatus = AerovNetVirtioGetStatus(Adapter);
    if ((devStatus & VIRTIO_STATUS_FEATURES_OK) == 0) {
        AerovNetVirtioFailDevice(Adapter);
        return NDIS_STATUS_FAILURE;
    }

    /* Disable MSI-X vectors (INTx required by contract). */
    WRITE_REGISTER_USHORT(&Adapter->CommonCfg->msix_config, VIRTIO_MSI_NO_VECTOR);

    if (READ_REGISTER_USHORT(&Adapter->CommonCfg->num_queues) < AEROVNET_QUEUE_COUNT) {
        AerovNetVirtioFailDevice(Adapter);
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    /* Setup queues: rxq (0), txq (1). */
    {
        NDIS_STATUS status;

        status = AerovNetSetupQueue(Adapter, &Adapter->RxQ, AEROVNET_QUEUE_RX, TRUE);
        if (status != NDIS_STATUS_SUCCESS) {
            return status;
        }

        status = AerovNetSetupQueue(Adapter, &Adapter->TxQ, AEROVNET_QUEUE_TX, FALSE);
        if (status != NDIS_STATUS_SUCCESS) {
            return status;
        }
    }

    /* Allocate packet buffers (contract-fixed). */
    Adapter->Mtu = AEROVNET_MTU;
    Adapter->MaxFrameSize = AEROVNET_MAX_FRAME_SIZE;
    Adapter->RxBufferDataBytes = AEROVNET_RX_PAYLOAD_BYTES;
    Adapter->RxBufferTotalBytes = AEROVNET_RX_BUFFER_BYTES;

    {
        NDIS_STATUS status;
        status = AerovNetAllocateRxResources(Adapter);
        if (status != NDIS_STATUS_SUCCESS) {
            return status;
        }

        status = AerovNetAllocateTxResources(Adapter);
        if (status != NDIS_STATUS_SUCCESS) {
            return status;
        }
    }

    st = AerovNetReadMacAndLinkState(Adapter);
    if (!NT_SUCCESS(st)) {
        return NDIS_STATUS_FAILURE;
    }

    AerovNetVirtioAddStatus(Adapter, VIRTIO_STATUS_DRIVER_OK);
    return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetVirtioStop(_Inout_ AEROVNET_ADAPTER* Adapter)
{
    LIST_ENTRY abortTxReqs;
    PNET_BUFFER_LIST completeHead;
    PNET_BUFFER_LIST completeTail;

    if (!Adapter) {
        return;
    }

    /* Stop the device first to prevent further DMA/interrupts. */
    AerovNetVirtioResetDevice(Adapter);

    /*
     * HaltEx is expected to run at PASSIVE_LEVEL; waiting here avoids freeing
     * memory while an NDIS SG mapping callback might still reference it.
     */
    if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
        (VOID)KeWaitForSingleObject(&Adapter->OutstandingSgEvent, Executive, KernelMode, FALSE, NULL);
    }

    InitializeListHead(&abortTxReqs);
    completeHead = NULL;
    completeTail = NULL;

    /* Move all outstanding TX requests to a local list and complete their NBLs. */
    NdisAcquireSpinLock(&Adapter->Lock);

    while (!IsListEmpty(&Adapter->TxAwaitingSgList)) {
        PLIST_ENTRY e = RemoveHeadList(&Adapter->TxAwaitingSgList);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        InsertTailList(&abortTxReqs, &txReq->Link);
        AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_RESET_IN_PROGRESS, &completeHead, &completeTail);
    }

    while (!IsListEmpty(&Adapter->TxPendingList)) {
        PLIST_ENTRY e = RemoveHeadList(&Adapter->TxPendingList);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        InsertTailList(&abortTxReqs, &txReq->Link);
        AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_RESET_IN_PROGRESS, &completeHead, &completeTail);
    }

    while (!IsListEmpty(&Adapter->TxSubmittedList)) {
        PLIST_ENTRY e = RemoveHeadList(&Adapter->TxSubmittedList);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        InsertTailList(&abortTxReqs, &txReq->Link);
        AerovNetCompleteTxRequest(Adapter, txReq, NDIS_STATUS_RESET_IN_PROGRESS, &completeHead, &completeTail);
    }

    NdisReleaseSpinLock(&Adapter->Lock);

    /* Free per-request SG lists and return requests to the free list. */
    while (!IsListEmpty(&abortTxReqs)) {
        PLIST_ENTRY e = RemoveHeadList(&abortTxReqs);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        PNET_BUFFER nb = txReq->Nb;

        if (txReq->SgList) {
            NdisMFreeNetBufferSGList(Adapter->DmaHandle, txReq->SgList, nb);
            txReq->SgList = NULL;
        }

        NdisAcquireSpinLock(&Adapter->Lock);
        AerovNetFreeTxRequestNoLock(Adapter, txReq);
        NdisReleaseSpinLock(&Adapter->Lock);
    }

    while (completeHead) {
        PNET_BUFFER_LIST nbl = completeHead;
        completeHead = NET_BUFFER_LIST_NEXT_NBL(nbl);
        NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;
        AerovNetCompleteNblSend(Adapter, nbl, NET_BUFFER_LIST_STATUS(nbl));
    }

    AerovNetFreeTxResources(Adapter);
    AerovNetFreeRxResources(Adapter);

    AerovNetFreeVq(Adapter, &Adapter->RxQ);
    AerovNetFreeVq(Adapter, &Adapter->TxQ);

    /* Transport mapping and PCI interface are released in AerovNetCleanupAdapter. */
}

/* -------------------------------------------------------------------------- */
/* NDIS link indication + interrupt handling                                  */
/* -------------------------------------------------------------------------- */

static VOID AerovNetIndicateLinkState(_In_ AEROVNET_ADAPTER* Adapter)
{
    NDIS_STATUS_INDICATION ind;
    NDIS_LINK_STATE linkState;

    RtlZeroMemory(&ind, sizeof(ind));
    RtlZeroMemory(&linkState, sizeof(linkState));

    linkState.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
    linkState.Header.Revision = NDIS_LINK_STATE_REVISION_1;
    linkState.Header.Size = sizeof(linkState);

    linkState.MediaConnectState = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
    linkState.MediaDuplexState = MediaDuplexStateFull;
    linkState.XmitLinkSpeed = g_DefaultLinkSpeedBps;
    linkState.RcvLinkSpeed = g_DefaultLinkSpeedBps;

    ind.Header.Type = NDIS_OBJECT_TYPE_STATUS_INDICATION;
    ind.Header.Revision = NDIS_STATUS_INDICATION_REVISION_1;
    ind.Header.Size = sizeof(ind);

    ind.SourceHandle = Adapter->MiniportAdapterHandle;
    ind.StatusCode = NDIS_STATUS_LINK_STATE;
    ind.StatusBuffer = &linkState;
    ind.StatusBufferSize = sizeof(linkState);

    NdisMIndicateStatusEx(Adapter->MiniportAdapterHandle, &ind);
}

static BOOLEAN AerovNetInterruptIsr(_In_ NDIS_HANDLE MiniportInterruptContext, _Out_ PBOOLEAN QueueDefaultInterruptDpc, _Out_ PULONG TargetProcessors)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
    UCHAR isr;

    UNREFERENCED_PARAMETER(TargetProcessors);

    if (!adapter) {
        return FALSE;
    }

    if (adapter->IsrStatus == NULL) {
        return FALSE;
    }

    /* Modern ISR status byte is read-to-ack (required for INTx deassertion). */
    isr = READ_REGISTER_UCHAR(adapter->IsrStatus);
    if (isr == 0) {
        return FALSE;
    }

    if (adapter->State == AerovNetAdapterStopped) {
        *QueueDefaultInterruptDpc = FALSE;
        return TRUE;
    }

    InterlockedOr(&adapter->PendingIsrStatus, (LONG)isr);
    *QueueDefaultInterruptDpc = TRUE;
    return TRUE;
}

static VOID AerovNetInterruptDpc(_In_ NDIS_HANDLE MiniportInterruptContext, _In_ PVOID MiniportDpcContext, _In_ PULONG NdisReserved1, _In_ PULONG NdisReserved2)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportInterruptContext;
    LONG isr;
    LIST_ENTRY completeTxReqs;
    PNET_BUFFER_LIST completeNblHead;
    PNET_BUFFER_LIST completeNblTail;
    PNET_BUFFER_LIST indicateHead;
    PNET_BUFFER_LIST indicateTail;
    ULONG indicateCount;
    BOOLEAN linkChanged;
    BOOLEAN newLinkUp;

    UNREFERENCED_PARAMETER(MiniportDpcContext);
    UNREFERENCED_PARAMETER(NdisReserved1);
    UNREFERENCED_PARAMETER(NdisReserved2);

    if (!adapter) {
        return;
    }

    InitializeListHead(&completeTxReqs);
    completeNblHead = NULL;
    completeNblTail = NULL;
    indicateHead = NULL;
    indicateTail = NULL;
    indicateCount = 0;
    linkChanged = FALSE;
    newLinkUp = adapter->LinkUp;

    isr = InterlockedExchange(&adapter->PendingIsrStatus, 0);

    NdisAcquireSpinLock(&adapter->Lock);

    if (adapter->State == AerovNetAdapterStopped) {
        NdisReleaseSpinLock(&adapter->Lock);
        return;
    }

    /* TX completions. */
    if (adapter->TxQ.Vq != NULL) {
        for (;;) {
            void* cookie = NULL;
            UINT32 len = 0;
            NTSTATUS st = VirtqSplitGetUsed(adapter->TxQ.Vq, &cookie, &len);
            if (st == STATUS_NOT_FOUND) {
                break;
            }
            if (!NT_SUCCESS(st)) {
                adapter->StatTxErrors++;
                break;
            }

            UNREFERENCED_PARAMETER(len);

            {
                AEROVNET_TX_REQUEST* txReq = (AEROVNET_TX_REQUEST*)cookie;
                if (txReq == NULL) {
                    adapter->StatTxErrors++;
                    continue;
                }

                adapter->StatTxPackets++;
                adapter->StatTxBytes += NET_BUFFER_DATA_LENGTH(txReq->Nb);

                if (txReq->State == AerovNetTxSubmitted) {
                    RemoveEntryList(&txReq->Link);
                }
                InsertTailList(&completeTxReqs, &txReq->Link);

                AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_SUCCESS, &completeNblHead, &completeNblTail);
            }
        }
    }

    /* Submit any TX requests that were waiting on descriptors. */
    if (adapter->State == AerovNetAdapterRunning) {
        AerovNetFlushTxPendingLocked(adapter, &completeTxReqs, &completeNblHead, &completeNblTail);
    }

    /* RX completions. */
    if (adapter->RxQ.Vq != NULL) {
        for (;;) {
            void* cookie = NULL;
            UINT32 len = 0;
            NTSTATUS st = VirtqSplitGetUsed(adapter->RxQ.Vq, &cookie, &len);
            if (st == STATUS_NOT_FOUND) {
                break;
            }
            if (!NT_SUCCESS(st)) {
                adapter->StatRxErrors++;
                break;
            }

            {
                AEROVNET_RX_BUFFER* rx = (AEROVNET_RX_BUFFER*)cookie;
                ULONG payloadLen;

                if (rx == NULL) {
                    adapter->StatRxErrors++;
                    continue;
                }

                if (len < AEROVNET_NET_HDR_LEN || len > rx->BufferBytes) {
                    adapter->StatRxErrors++;
                    InsertTailList(&adapter->RxFreeList, &rx->Link);
                    continue;
                }

                payloadLen = len - AEROVNET_NET_HDR_LEN;

                /* Contract drop rules (but always recycle). */
                if (payloadLen < AEROVNET_MIN_FRAME_SIZE || payloadLen > AEROVNET_MAX_FRAME_SIZE) {
                    adapter->StatRxErrors++;
                    InsertTailList(&adapter->RxFreeList, &rx->Link);
                    continue;
                }

                if (adapter->State != AerovNetAdapterRunning) {
                    InsertTailList(&adapter->RxFreeList, &rx->Link);
                    continue;
                }

                if (!AerovNetAcceptFrame(adapter, rx->BufferVa + AEROVNET_NET_HDR_LEN, payloadLen)) {
                    InsertTailList(&adapter->RxFreeList, &rx->Link);
                    continue;
                }

                rx->Indicated = TRUE;

                NET_BUFFER_DATA_OFFSET(rx->Nb) = AEROVNET_NET_HDR_LEN;
                NET_BUFFER_DATA_LENGTH(rx->Nb) = payloadLen;
                NET_BUFFER_LIST_STATUS(rx->Nbl) = NDIS_STATUS_SUCCESS;
                NET_BUFFER_LIST_NEXT_NBL(rx->Nbl) = NULL;

                if (indicateTail) {
                    NET_BUFFER_LIST_NEXT_NBL(indicateTail) = rx->Nbl;
                    indicateTail = rx->Nbl;
                } else {
                    indicateHead = rx->Nbl;
                    indicateTail = rx->Nbl;
                }

                indicateCount++;
                adapter->StatRxPackets++;
                adapter->StatRxBytes += payloadLen;
            }
        }
    }

    /* Refill RX queue with any buffers we dropped. */
    if (adapter->State == AerovNetAdapterRunning) {
        AerovNetFillRxQueueLocked(adapter);
    }

    /* Link state change handling (config interrupt). */
    if ((isr & VIRTIO_PCI_ISR_CONFIG_INTERRUPT) != 0 && adapter->DeviceCfg != NULL) {
        USHORT linkStatus = READ_REGISTER_USHORT((volatile USHORT*)((PUCHAR)adapter->DeviceCfg + 6));
        newLinkUp = (linkStatus & VIRTIO_NET_S_LINK_UP) ? TRUE : FALSE;
        if (newLinkUp != adapter->LinkUp) {
            adapter->LinkUp = newLinkUp;
            linkChanged = TRUE;
        }
    }

    NdisReleaseSpinLock(&adapter->Lock);

    /* Free SG lists and return TX requests to free list. */
    while (!IsListEmpty(&completeTxReqs)) {
        PLIST_ENTRY entry = RemoveHeadList(&completeTxReqs);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(entry, AEROVNET_TX_REQUEST, Link);

        if (txReq->SgList) {
            NdisMFreeNetBufferSGList(adapter->DmaHandle, txReq->SgList, txReq->Nb);
            txReq->SgList = NULL;
        }

        NdisAcquireSpinLock(&adapter->Lock);
        AerovNetFreeTxRequestNoLock(adapter, txReq);
        NdisReleaseSpinLock(&adapter->Lock);
    }

    /* Complete any NBLs which have no remaining NET_BUFFERs pending. */
    while (completeNblHead) {
        PNET_BUFFER_LIST nbl = completeNblHead;
        completeNblHead = NET_BUFFER_LIST_NEXT_NBL(nbl);
        NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;

        AerovNetCompleteNblSend(adapter, nbl, NET_BUFFER_LIST_STATUS(nbl));
    }

    /* Indicate receives. */
    if (indicateHead) {
        NdisMIndicateReceiveNetBufferLists(
            adapter->MiniportAdapterHandle,
            indicateHead,
            NDIS_DEFAULT_PORT_NUMBER,
            indicateCount,
            AerovNetReceiveIndicationFlagsForCurrentIrql());
    }

    if (linkChanged) {
        AerovNetIndicateLinkState(adapter);
    }
}

static VOID AerovNetProcessSgList(
    _In_ PDEVICE_OBJECT DeviceObject,
    _In_opt_ PVOID Reserved,
    _In_ PSCATTER_GATHER_LIST ScatterGatherList,
    _In_ PVOID Context)
{
    AEROVNET_TX_REQUEST* txReq;
    AEROVNET_ADAPTER* adapter;
    VIRTQ_SG sg[AEROVNET_MAX_TX_SG_ELEMENTS + 1];
    ULONG elemCount;
    ULONG i;
    UINT16 sgCount;
    NTSTATUS st;
    PNET_BUFFER nbForFree;
    BOOLEAN completeNow;
    PNET_BUFFER_LIST completeHead;
    PNET_BUFFER_LIST completeTail;

    UNREFERENCED_PARAMETER(DeviceObject);
    UNREFERENCED_PARAMETER(Reserved);

    txReq = (AEROVNET_TX_REQUEST*)Context;
    if (!txReq || !ScatterGatherList) {
        return;
    }

    adapter = txReq->Adapter;
    if (!adapter) {
        return;
    }

    elemCount = ScatterGatherList->NumberOfElements;
    sgCount = (UINT16)(elemCount + 1);

    completeNow = FALSE;
    completeHead = NULL;
    completeTail = NULL;
    nbForFree = txReq->Nb;

    NdisAcquireSpinLock(&adapter->Lock);

    /* The request was in-flight in the "awaiting SG" list. Remove it regardless. */
    if (txReq->State == AerovNetTxAwaitingSg) {
        RemoveEntryList(&txReq->Link);
    }

    txReq->SgList = ScatterGatherList;

    if (txReq->Cancelled) {
        AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_REQUEST_ABORTED, &completeHead, &completeTail);
        completeNow = TRUE;
    } else if (adapter->State == AerovNetAdapterStopped) {
        AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_RESET_IN_PROGRESS, &completeHead, &completeTail);
        completeNow = TRUE;
    } else if (elemCount > AEROVNET_MAX_TX_SG_ELEMENTS) {
        AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_BUFFER_OVERFLOW, &completeHead, &completeTail);
        completeNow = TRUE;
    } else if (adapter->State != AerovNetAdapterRunning) {
        /* Paused: queue for later retry on restart. */
        txReq->State = AerovNetTxPendingSubmit;
        InsertTailList(&adapter->TxPendingList, &txReq->Link);
    } else if (adapter->TxQ.Vq == NULL) {
        AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_FAILURE, &completeHead, &completeTail);
        completeNow = TRUE;
    } else {
        /* Build virtio-net header: 10 bytes, all fields zero (no offloads). */
        RtlZeroMemory(txReq->HeaderVa, AEROVNET_NET_HDR_LEN);

        sg[0].addr = (UINT64)txReq->HeaderPa.QuadPart;
        sg[0].len = AEROVNET_NET_HDR_LEN;
        sg[0].write = FALSE;

        for (i = 0; i < elemCount; i++) {
            sg[1 + i].addr = (UINT64)ScatterGatherList->Elements[i].Address.QuadPart;
            sg[1 + i].len = ScatterGatherList->Elements[i].Length;
            sg[1 + i].write = FALSE;
        }

        st = VirtqSplitAddBuffer(adapter->TxQ.Vq, sg, sgCount, txReq, &txReq->DescHeadId);
        if (st == STATUS_INSUFFICIENT_RESOURCES) {
            /* No descriptors/indirect tables yet; queue it for later retry (DPC will flush). */
            txReq->State = AerovNetTxPendingSubmit;
            InsertTailList(&adapter->TxPendingList, &txReq->Link);
        } else if (!NT_SUCCESS(st)) {
            AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_FAILURE, &completeHead, &completeTail);
            completeNow = TRUE;
        } else {
            VirtqSplitPublish(adapter->TxQ.Vq, txReq->DescHeadId);
            txReq->State = AerovNetTxSubmitted;
            InsertTailList(&adapter->TxSubmittedList, &txReq->Link);

            {
                BOOLEAN kick = VirtqSplitKickPrepare(adapter->TxQ.Vq);
                if (kick) {
                    AerovNetNotifyQueue(adapter, &adapter->TxQ);
                }
                VirtqSplitKickCommit(adapter->TxQ.Vq);
            }
        }
    }

    NdisReleaseSpinLock(&adapter->Lock);

    if (completeNow) {
        /* Free the SG list immediately; the device never saw the descriptors. */
        if (ScatterGatherList) {
            NdisMFreeNetBufferSGList(adapter->DmaHandle, ScatterGatherList, nbForFree);
        }

        NdisAcquireSpinLock(&adapter->Lock);
        AerovNetFreeTxRequestNoLock(adapter, txReq);
        NdisReleaseSpinLock(&adapter->Lock);

        while (completeHead) {
            PNET_BUFFER_LIST nbl = completeHead;
            completeHead = NET_BUFFER_LIST_NEXT_NBL(nbl);
            NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;
            AerovNetCompleteNblSend(adapter, nbl, NET_BUFFER_LIST_STATUS(nbl));
        }
    }

    /* Signal HaltEx once all SG mapping callbacks have finished. */
    if (InterlockedDecrement(&adapter->OutstandingSgMappings) == 0) {
        KeSetEvent(&adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
    }
}

/* -------------------------------------------------------------------------- */
/* OID handling (ported from legacy driver)                                   */
/* -------------------------------------------------------------------------- */

static NDIS_STATUS AerovNetOidQuery(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNDIS_OID_REQUEST OidRequest)
{
    NDIS_OID oid = OidRequest->DATA.QUERY_INFORMATION.Oid;
    PVOID outBuffer = OidRequest->DATA.QUERY_INFORMATION.InformationBuffer;
    ULONG outLen = OidRequest->DATA.QUERY_INFORMATION.InformationBufferLength;
    ULONG bytesWritten = 0;
    ULONG bytesNeeded = 0;

    switch (oid) {
    case OID_GEN_SUPPORTED_LIST: {
        bytesNeeded = sizeof(g_SupportedOids);
        if (outLen < bytesNeeded) {
            break;
        }
        RtlCopyMemory(outBuffer, g_SupportedOids, sizeof(g_SupportedOids));
        bytesWritten = sizeof(g_SupportedOids);
        break;
    }

    case OID_GEN_HARDWARE_STATUS: {
        NDIS_HARDWARE_STATUS hw = NdisHardwareStatusReady;
        bytesNeeded = sizeof(hw);
        if (outLen < bytesNeeded) {
            break;
        }
        *(NDIS_HARDWARE_STATUS*)outBuffer = hw;
        bytesWritten = sizeof(hw);
        break;
    }

    case OID_GEN_MEDIA_SUPPORTED:
    case OID_GEN_MEDIA_IN_USE: {
        NDIS_MEDIUM m = NdisMedium802_3;
        bytesNeeded = sizeof(m);
        if (outLen < bytesNeeded) {
            break;
        }
        *(NDIS_MEDIUM*)outBuffer = m;
        bytesWritten = sizeof(m);
        break;
    }

    case OID_GEN_PHYSICAL_MEDIUM: {
        NDIS_PHYSICAL_MEDIUM p = NdisPhysicalMedium802_3;
        bytesNeeded = sizeof(p);
        if (outLen < bytesNeeded) {
            break;
        }
        *(NDIS_PHYSICAL_MEDIUM*)outBuffer = p;
        bytesWritten = sizeof(p);
        break;
    }

    case OID_GEN_MAXIMUM_FRAME_SIZE: {
        ULONG v = Adapter->Mtu;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_MAXIMUM_LOOKAHEAD:
    case OID_GEN_CURRENT_LOOKAHEAD: {
        ULONG v = Adapter->Mtu;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_MAXIMUM_TOTAL_SIZE: {
        ULONG v = Adapter->MaxFrameSize;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_LINK_SPEED: {
        ULONG speed100Bps = (ULONG)(g_DefaultLinkSpeedBps / 100ull);
        bytesNeeded = sizeof(speed100Bps);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = speed100Bps;
        bytesWritten = sizeof(speed100Bps);
        break;
    }

    case OID_GEN_TRANSMIT_BLOCK_SIZE:
    case OID_GEN_RECEIVE_BLOCK_SIZE: {
        ULONG v = 1;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_VENDOR_ID: {
        ULONG vid = ((ULONG)Adapter->PermanentMac[0]) | ((ULONG)Adapter->PermanentMac[1] << 8) | ((ULONG)Adapter->PermanentMac[2] << 16);
        bytesNeeded = sizeof(vid);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = vid;
        bytesWritten = sizeof(vid);
        break;
    }

    case OID_GEN_VENDOR_DESCRIPTION: {
        static const char desc[] = "Aero virtio-net (modern)";
        bytesNeeded = sizeof(desc);
        if (outLen < bytesNeeded) {
            break;
        }
        RtlCopyMemory(outBuffer, desc, sizeof(desc));
        bytesWritten = sizeof(desc);
        break;
    }

    case OID_GEN_DRIVER_VERSION: {
        USHORT v = AEROVNET_OID_DRIVER_VERSION;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(USHORT*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_VENDOR_DRIVER_VERSION: {
        ULONG v = 1;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_MAC_OPTIONS: {
        ULONG v = NDIS_MAC_OPTION_COPY_LOOKAHEAD_DATA | NDIS_MAC_OPTION_NO_LOOPBACK;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_MEDIA_CONNECT_STATUS: {
        NDIS_MEDIA_CONNECT_STATE s = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
        bytesNeeded = sizeof(s);
        if (outLen < bytesNeeded) {
            break;
        }
        *(NDIS_MEDIA_CONNECT_STATE*)outBuffer = s;
        bytesWritten = sizeof(s);
        break;
    }

    case OID_GEN_CURRENT_PACKET_FILTER: {
        ULONG v = Adapter->PacketFilter;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_MAXIMUM_SEND_PACKETS: {
        ULONG v = 1;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_XMIT_OK: {
        ULONGLONG v = Adapter->StatTxPackets;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONGLONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_RCV_OK: {
        ULONGLONG v = Adapter->StatRxPackets;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONGLONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_XMIT_ERROR: {
        ULONGLONG v = Adapter->StatTxErrors;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONGLONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_RCV_ERROR: {
        ULONGLONG v = Adapter->StatRxErrors;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONGLONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_RCV_NO_BUFFER: {
        ULONGLONG v = Adapter->StatRxNoBuffers;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONGLONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    case OID_GEN_LINK_STATE: {
        NDIS_LINK_STATE link;
        bytesNeeded = sizeof(link);
        if (outLen < bytesNeeded) {
            break;
        }

        RtlZeroMemory(&link, sizeof(link));
        link.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
        link.Header.Revision = NDIS_LINK_STATE_REVISION_1;
        link.Header.Size = sizeof(link);
        link.MediaConnectState = Adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
        link.MediaDuplexState = MediaDuplexStateFull;
        link.XmitLinkSpeed = g_DefaultLinkSpeedBps;
        link.RcvLinkSpeed = g_DefaultLinkSpeedBps;
        *(NDIS_LINK_STATE*)outBuffer = link;
        bytesWritten = sizeof(link);
        break;
    }

    case OID_GEN_STATISTICS: {
        NDIS_STATISTICS_INFO info;
        bytesNeeded = sizeof(info);
        if (outLen < bytesNeeded) {
            break;
        }

        RtlZeroMemory(&info, sizeof(info));
        info.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
        info.Header.Revision = NDIS_STATISTICS_INFO_REVISION_1;
        info.Header.Size = sizeof(info);

        info.SupportedStatistics = NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_XMIT |
                                   NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_XMIT;
        info.ifInUcastPkts = Adapter->StatRxPackets;
        info.ifOutUcastPkts = Adapter->StatTxPackets;
        info.ifInUcastOctets = Adapter->StatRxBytes;
        info.ifOutUcastOctets = Adapter->StatTxBytes;

        *(NDIS_STATISTICS_INFO*)outBuffer = info;
        bytesWritten = sizeof(info);
        break;
    }

    case OID_802_3_PERMANENT_ADDRESS: {
        bytesNeeded = ETH_LENGTH_OF_ADDRESS;
        if (outLen < bytesNeeded) {
            break;
        }
        RtlCopyMemory(outBuffer, Adapter->PermanentMac, ETH_LENGTH_OF_ADDRESS);
        bytesWritten = ETH_LENGTH_OF_ADDRESS;
        break;
    }

    case OID_802_3_CURRENT_ADDRESS: {
        bytesNeeded = ETH_LENGTH_OF_ADDRESS;
        if (outLen < bytesNeeded) {
            break;
        }
        RtlCopyMemory(outBuffer, Adapter->CurrentMac, ETH_LENGTH_OF_ADDRESS);
        bytesWritten = ETH_LENGTH_OF_ADDRESS;
        break;
    }

    case OID_802_3_MULTICAST_LIST: {
        bytesNeeded = Adapter->MulticastListSize * ETH_LENGTH_OF_ADDRESS;
        if (outLen < bytesNeeded) {
            break;
        }
        RtlCopyMemory(outBuffer, Adapter->MulticastList, bytesNeeded);
        bytesWritten = bytesNeeded;
        break;
    }

    case OID_802_3_MAXIMUM_LIST_SIZE: {
        ULONG v = NDIS_MAX_MULTICAST_LIST;
        bytesNeeded = sizeof(v);
        if (outLen < bytesNeeded) {
            break;
        }
        *(ULONG*)outBuffer = v;
        bytesWritten = sizeof(v);
        break;
    }

    default:
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    OidRequest->DATA.QUERY_INFORMATION.BytesWritten = bytesWritten;
    OidRequest->DATA.QUERY_INFORMATION.BytesNeeded = bytesNeeded;

    return (bytesWritten != 0) ? NDIS_STATUS_SUCCESS : NDIS_STATUS_BUFFER_TOO_SHORT;
}

static NDIS_STATUS AerovNetOidSet(_Inout_ AEROVNET_ADAPTER* Adapter, _Inout_ PNDIS_OID_REQUEST OidRequest)
{
    NDIS_OID oid = OidRequest->DATA.SET_INFORMATION.Oid;
    PVOID inBuffer = OidRequest->DATA.SET_INFORMATION.InformationBuffer;
    ULONG inLen = OidRequest->DATA.SET_INFORMATION.InformationBufferLength;
    ULONG bytesRead = 0;
    ULONG bytesNeeded = 0;

    switch (oid) {
    case OID_GEN_CURRENT_PACKET_FILTER: {
        ULONG filter;
        bytesNeeded = sizeof(filter);
        if (inLen < bytesNeeded) {
            break;
        }
        filter = *(ULONG*)inBuffer;

        Adapter->PacketFilter = filter;
        bytesRead = sizeof(filter);
        break;
    }

    case OID_802_3_MULTICAST_LIST: {
        ULONG count;
        if (inLen % ETH_LENGTH_OF_ADDRESS != 0) {
            return NDIS_STATUS_INVALID_LENGTH;
        }
        count = inLen / ETH_LENGTH_OF_ADDRESS;
        if (count > NDIS_MAX_MULTICAST_LIST) {
            return NDIS_STATUS_MULTICAST_FULL;
        }
        RtlCopyMemory(Adapter->MulticastList, inBuffer, inLen);
        Adapter->MulticastListSize = count;
        bytesRead = inLen;
        break;
    }

    default:
        return NDIS_STATUS_NOT_SUPPORTED;
    }

    OidRequest->DATA.SET_INFORMATION.BytesRead = bytesRead;
    OidRequest->DATA.SET_INFORMATION.BytesNeeded = bytesNeeded;

    return (bytesRead != 0) ? NDIS_STATUS_SUCCESS : NDIS_STATUS_BUFFER_TOO_SHORT;
}

static NDIS_STATUS AerovNetMiniportOidRequest(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_OID_REQUEST OidRequest)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
    NDIS_STATUS status;

    if (!adapter || !OidRequest) {
        return NDIS_STATUS_FAILURE;
    }

    NdisAcquireSpinLock(&adapter->Lock);
    if (adapter->State == AerovNetAdapterStopped) {
        NdisReleaseSpinLock(&adapter->Lock);
        return NDIS_STATUS_RESET_IN_PROGRESS;
    }
    NdisReleaseSpinLock(&adapter->Lock);

    switch (OidRequest->RequestType) {
    case NdisRequestQueryInformation:
    case NdisRequestQueryStatistics:
        status = AerovNetOidQuery(adapter, OidRequest);
        break;
    case NdisRequestSetInformation:
        status = AerovNetOidSet(adapter, OidRequest);
        break;
    default:
        status = NDIS_STATUS_NOT_SUPPORTED;
        break;
    }

    return status;
}

/* -------------------------------------------------------------------------- */
/* NDIS send/receive paths                                                    */
/* -------------------------------------------------------------------------- */

static VOID AerovNetMiniportSendNetBufferLists(
    _In_ NDIS_HANDLE MiniportAdapterContext,
    _In_ PNET_BUFFER_LIST NetBufferLists,
    _In_ NDIS_PORT_NUMBER PortNumber,
    _In_ ULONG SendFlags)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
    PNET_BUFFER_LIST nbl;
    PNET_BUFFER_LIST completeHead;
    PNET_BUFFER_LIST completeTail;

    UNREFERENCED_PARAMETER(PortNumber);
    UNREFERENCED_PARAMETER(SendFlags);

    if (!adapter) {
        return;
    }

    completeHead = NULL;
    completeTail = NULL;

    nbl = NetBufferLists;
    while (nbl) {
        PNET_BUFFER_LIST nextNbl;
        PNET_BUFFER nb;
        LONG nbCount;

        nextNbl = NET_BUFFER_LIST_NEXT_NBL(nbl);
        NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;

        nbCount = 0;
        for (nb = NET_BUFFER_LIST_FIRST_NB(nbl); nb; nb = NET_BUFFER_NEXT_NB(nb)) {
            nbCount++;
        }

        if (nbCount == 0) {
            NET_BUFFER_LIST_STATUS(nbl) = NDIS_STATUS_SUCCESS;
            if (completeTail) {
                NET_BUFFER_LIST_NEXT_NBL(completeTail) = nbl;
                completeTail = nbl;
            } else {
                completeHead = nbl;
                completeTail = nbl;
            }

            nbl = nextNbl;
            continue;
        }

        AEROVNET_NBL_SET_PENDING(nbl, nbCount);
        AEROVNET_NBL_SET_STATUS(nbl, NDIS_STATUS_SUCCESS);

        for (nb = NET_BUFFER_LIST_FIRST_NB(nbl); nb; nb = NET_BUFFER_NEXT_NB(nb)) {
            AEROVNET_TX_REQUEST* txReq;
            NDIS_STATUS sgStatus;

            txReq = NULL;

            NdisAcquireSpinLock(&adapter->Lock);

            if (adapter->State != AerovNetAdapterRunning) {
                AerovNetTxNblCompleteOneNetBufferLocked(adapter, nbl, NDIS_STATUS_RESET_IN_PROGRESS, &completeHead, &completeTail);
                NdisReleaseSpinLock(&adapter->Lock);
                continue;
            }

            if (IsListEmpty(&adapter->TxFreeList)) {
                AerovNetTxNblCompleteOneNetBufferLocked(adapter, nbl, NDIS_STATUS_RESOURCES, &completeHead, &completeTail);
                NdisReleaseSpinLock(&adapter->Lock);
                continue;
            }

            {
                PLIST_ENTRY entry = RemoveHeadList(&adapter->TxFreeList);
                txReq = CONTAINING_RECORD(entry, AEROVNET_TX_REQUEST, Link);
            }

            txReq->State = AerovNetTxAwaitingSg;
            txReq->Cancelled = FALSE;
            txReq->Adapter = adapter;
            txReq->Nbl = nbl;
            txReq->Nb = nb;
            txReq->SgList = NULL;
            InsertTailList(&adapter->TxAwaitingSgList, &txReq->Link);

            if (InterlockedIncrement(&adapter->OutstandingSgMappings) == 1) {
                KeClearEvent(&adapter->OutstandingSgEvent);
            }

            NdisReleaseSpinLock(&adapter->Lock);

            sgStatus = NdisMAllocateNetBufferSGList(adapter->DmaHandle, nb, txReq, 0);
            if (sgStatus != NDIS_STATUS_SUCCESS && sgStatus != NDIS_STATUS_PENDING) {
                /* SG allocation failed synchronously; undo the TxReq. */
                if (InterlockedDecrement(&adapter->OutstandingSgMappings) == 0) {
                    KeSetEvent(&adapter->OutstandingSgEvent, IO_NO_INCREMENT, FALSE);
                }

                NdisAcquireSpinLock(&adapter->Lock);
                RemoveEntryList(&txReq->Link);
                AerovNetCompleteTxRequest(adapter, txReq, sgStatus, &completeHead, &completeTail);
                AerovNetFreeTxRequestNoLock(adapter, txReq);
                NdisReleaseSpinLock(&adapter->Lock);
            }
        }

        nbl = nextNbl;
    }

    while (completeHead) {
        PNET_BUFFER_LIST done = completeHead;
        completeHead = NET_BUFFER_LIST_NEXT_NBL(done);
        NET_BUFFER_LIST_NEXT_NBL(done) = NULL;
        AerovNetCompleteNblSend(adapter, done, NET_BUFFER_LIST_STATUS(done));
    }
}

static VOID AerovNetMiniportReturnNetBufferLists(
    _In_ NDIS_HANDLE MiniportAdapterContext,
    _In_ PNET_BUFFER_LIST NetBufferLists,
    _In_ ULONG ReturnFlags)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
    PNET_BUFFER_LIST nbl;

    UNREFERENCED_PARAMETER(ReturnFlags);

    if (!adapter) {
        return;
    }

    NdisAcquireSpinLock(&adapter->Lock);

    for (nbl = NetBufferLists; nbl; nbl = NET_BUFFER_LIST_NEXT_NBL(nbl)) {
        AEROVNET_RX_BUFFER* rx = (AEROVNET_RX_BUFFER*)nbl->MiniportReserved[0];
        if (!rx) {
            continue;
        }

        rx->Indicated = FALSE;
        NET_BUFFER_DATA_OFFSET(rx->Nb) = AEROVNET_NET_HDR_LEN;
        NET_BUFFER_DATA_LENGTH(rx->Nb) = 0;

        InsertTailList(&adapter->RxFreeList, &rx->Link);
    }

    if (adapter->State == AerovNetAdapterRunning) {
        AerovNetFillRxQueueLocked(adapter);
    }

    NdisReleaseSpinLock(&adapter->Lock);
}

static VOID AerovNetMiniportCancelSend(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PVOID CancelId)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
    PLIST_ENTRY entry;
    LIST_ENTRY cancelledReqs;
    PNET_BUFFER_LIST completeHead;
    PNET_BUFFER_LIST completeTail;

    if (!adapter) {
        return;
    }

    InitializeListHead(&cancelledReqs);
    completeHead = NULL;
    completeTail = NULL;

    NdisAcquireSpinLock(&adapter->Lock);

    /* Mark any requests still awaiting SG mapping as cancelled. */
    for (entry = adapter->TxAwaitingSgList.Flink; entry != &adapter->TxAwaitingSgList; entry = entry->Flink) {
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(entry, AEROVNET_TX_REQUEST, Link);
        if (NET_BUFFER_LIST_CANCEL_ID(txReq->Nbl) == CancelId) {
            txReq->Cancelled = TRUE;
        }
    }

    /* Cancel requests queued pending submission (SG mapping already complete). */
    entry = adapter->TxPendingList.Flink;
    while (entry != &adapter->TxPendingList) {
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(entry, AEROVNET_TX_REQUEST, Link);
        entry = entry->Flink;

        if (NET_BUFFER_LIST_CANCEL_ID(txReq->Nbl) == CancelId) {
            RemoveEntryList(&txReq->Link);
            InsertTailList(&cancelledReqs, &txReq->Link);
            AerovNetCompleteTxRequest(adapter, txReq, NDIS_STATUS_REQUEST_ABORTED, &completeHead, &completeTail);
        }
    }

    NdisReleaseSpinLock(&adapter->Lock);

    while (!IsListEmpty(&cancelledReqs)) {
        PLIST_ENTRY e = RemoveHeadList(&cancelledReqs);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        PNET_BUFFER nb = txReq->Nb;

        if (txReq->SgList) {
            NdisMFreeNetBufferSGList(adapter->DmaHandle, txReq->SgList, nb);
            txReq->SgList = NULL;
        }

        NdisAcquireSpinLock(&adapter->Lock);
        AerovNetFreeTxRequestNoLock(adapter, txReq);
        NdisReleaseSpinLock(&adapter->Lock);
    }

    while (completeHead) {
        PNET_BUFFER_LIST nbl = completeHead;
        completeHead = NET_BUFFER_LIST_NEXT_NBL(nbl);
        NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;
        AerovNetCompleteNblSend(adapter, nbl, NET_BUFFER_LIST_STATUS(nbl));
    }
}

static VOID AerovNetMiniportDevicePnPEventNotify(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNET_DEVICE_PNP_EVENT NetDevicePnPEvent)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

    if (!adapter || !NetDevicePnPEvent) {
        return;
    }

    if (NetDevicePnPEvent->DevicePnPEvent == NdisDevicePnPEventSurpriseRemoved) {
        NdisAcquireSpinLock(&adapter->Lock);
        adapter->SurpriseRemoved = TRUE;
        adapter->State = AerovNetAdapterStopped;
        NdisReleaseSpinLock(&adapter->Lock);

        /* Quiesce the device. Full cleanup happens in HaltEx (PASSIVE_LEVEL). */
        AerovNetVirtioResetDevice(adapter);
    }
}

static NDIS_STATUS AerovNetMiniportPause(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_MINIPORT_PAUSE_PARAMETERS PauseParameters)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

    UNREFERENCED_PARAMETER(PauseParameters);

    if (!adapter) {
        return NDIS_STATUS_FAILURE;
    }

    NdisAcquireSpinLock(&adapter->Lock);
    adapter->State = AerovNetAdapterPaused;
    NdisReleaseSpinLock(&adapter->Lock);

    return NDIS_STATUS_SUCCESS;
}

static NDIS_STATUS AerovNetMiniportRestart(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ PNDIS_MINIPORT_RESTART_PARAMETERS RestartParameters)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;
    LIST_ENTRY completeTxReqs;
    PNET_BUFFER_LIST completeHead;
    PNET_BUFFER_LIST completeTail;

    UNREFERENCED_PARAMETER(RestartParameters);

    if (!adapter) {
        return NDIS_STATUS_FAILURE;
    }

    InitializeListHead(&completeTxReqs);
    completeHead = NULL;
    completeTail = NULL;

    NdisAcquireSpinLock(&adapter->Lock);
    adapter->State = AerovNetAdapterRunning;
    AerovNetFillRxQueueLocked(adapter);
    AerovNetFlushTxPendingLocked(adapter, &completeTxReqs, &completeHead, &completeTail);
    NdisReleaseSpinLock(&adapter->Lock);

    while (!IsListEmpty(&completeTxReqs)) {
        PLIST_ENTRY e = RemoveHeadList(&completeTxReqs);
        AEROVNET_TX_REQUEST* txReq = CONTAINING_RECORD(e, AEROVNET_TX_REQUEST, Link);
        PNET_BUFFER nb = txReq->Nb;

        if (txReq->SgList) {
            NdisMFreeNetBufferSGList(adapter->DmaHandle, txReq->SgList, nb);
            txReq->SgList = NULL;
        }

        NdisAcquireSpinLock(&adapter->Lock);
        AerovNetFreeTxRequestNoLock(adapter, txReq);
        NdisReleaseSpinLock(&adapter->Lock);
    }

    while (completeHead) {
        PNET_BUFFER_LIST nbl = completeHead;
        completeHead = NET_BUFFER_LIST_NEXT_NBL(nbl);
        NET_BUFFER_LIST_NEXT_NBL(nbl) = NULL;
        AerovNetCompleteNblSend(adapter, nbl, NET_BUFFER_LIST_STATUS(nbl));
    }

    return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetMiniportHaltEx(_In_ NDIS_HANDLE MiniportAdapterContext, _In_ NDIS_HALT_ACTION HaltAction)
{
    AEROVNET_ADAPTER* adapter = (AEROVNET_ADAPTER*)MiniportAdapterContext;

    UNREFERENCED_PARAMETER(HaltAction);

    if (!adapter) {
        return;
    }

    NdisAcquireSpinLock(&adapter->Lock);
    adapter->State = AerovNetAdapterStopped;
    NdisReleaseSpinLock(&adapter->Lock);

    AerovNetVirtioStop(adapter);
    AerovNetCleanupAdapter(adapter);
}

static NDIS_STATUS AerovNetMiniportInitializeEx(
    _In_ NDIS_HANDLE MiniportAdapterHandle,
    _In_ NDIS_HANDLE MiniportDriverContext,
    _In_ PNDIS_MINIPORT_INIT_PARAMETERS MiniportInitParameters)
{
    NDIS_STATUS status;
    AEROVNET_ADAPTER* adapter;
    NDIS_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES reg;
    NDIS_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES gen;
    NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS intr;
    NDIS_SG_DMA_DESCRIPTION dmaDesc;
    NDIS_NET_BUFFER_LIST_POOL_PARAMETERS poolParams;

    UNREFERENCED_PARAMETER(MiniportDriverContext);

    adapter = (AEROVNET_ADAPTER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*adapter), AEROVNET_TAG);
    if (!adapter) {
        return NDIS_STATUS_RESOURCES;
    }
    RtlZeroMemory(adapter, sizeof(*adapter));

    adapter->MiniportAdapterHandle = MiniportAdapterHandle;
    adapter->State = AerovNetAdapterStopped;
    adapter->PacketFilter = NDIS_PACKET_TYPE_DIRECTED | NDIS_PACKET_TYPE_BROADCAST | NDIS_PACKET_TYPE_MULTICAST;
    adapter->MulticastListSize = 0;
    adapter->PendingIsrStatus = 0;
    adapter->OutstandingSgMappings = 0;
    adapter->PciInterfaceAcquired = FALSE;

    NdisAllocateSpinLock(&adapter->Lock);
    KeInitializeEvent(&adapter->OutstandingSgEvent, NotificationEvent, TRUE);

    InitializeListHead(&adapter->RxFreeList);
    InitializeListHead(&adapter->TxFreeList);
    InitializeListHead(&adapter->TxAwaitingSgList);
    InitializeListHead(&adapter->TxPendingList);
    InitializeListHead(&adapter->TxSubmittedList);

    /* Registration attributes. */
    RtlZeroMemory(&reg, sizeof(reg));
    reg.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES;
    reg.Header.Revision = NDIS_MINIPORT_ADAPTER_REGISTRATION_ATTRIBUTES_REVISION_1;
    reg.Header.Size = sizeof(reg);
    reg.MiniportAdapterContext = adapter;
    reg.AttributeFlags = NDIS_MINIPORT_ATTRIBUTES_HARDWARE_DEVICE | NDIS_MINIPORT_ATTRIBUTES_BUS_MASTER;
    reg.CheckForHangTimeInSeconds = 0;
    reg.InterfaceType = NdisInterfacePci;

    status = NdisMSetMiniportAttributes(MiniportAdapterHandle, (PNDIS_MINIPORT_ADAPTER_ATTRIBUTES)&reg);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    status = AerovNetParseResources(adapter, MiniportInitParameters->AllocatedResources);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    /* Interrupt registration (legacy INTx). */
    RtlZeroMemory(&intr, sizeof(intr));
    intr.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_INTERRUPT;
    intr.Header.Revision = NDIS_MINIPORT_INTERRUPT_CHARACTERISTICS_REVISION_1;
    intr.Header.Size = sizeof(intr);
    intr.InterruptHandler = AerovNetInterruptIsr;
    intr.InterruptDpcHandler = AerovNetInterruptDpc;

    status = NdisMRegisterInterruptEx(MiniportAdapterHandle, adapter, &intr, &adapter->InterruptHandle);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    /* Scatter-gather DMA. */
    RtlZeroMemory(&dmaDesc, sizeof(dmaDesc));
    dmaDesc.Header.Type = NDIS_OBJECT_TYPE_SG_DMA_DESCRIPTION;
    dmaDesc.Header.Revision = NDIS_SG_DMA_DESCRIPTION_REVISION_1;
    dmaDesc.Header.Size = sizeof(dmaDesc);
    dmaDesc.Flags = NDIS_SG_DMA_64_BIT_ADDRESS;
    dmaDesc.MaximumPhysicalMapping = 0xFFFFFFFF;
    dmaDesc.ProcessSGListHandler = AerovNetProcessSgList;

    status = NdisMRegisterScatterGatherDma(MiniportAdapterHandle, &dmaDesc, &adapter->DmaHandle);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    /* Receive NBL pool. */
    RtlZeroMemory(&poolParams, sizeof(poolParams));
    poolParams.Header.Type = NDIS_OBJECT_TYPE_DEFAULT;
    poolParams.Header.Revision = NDIS_NET_BUFFER_LIST_POOL_PARAMETERS_REVISION_1;
    poolParams.Header.Size = sizeof(poolParams);
    poolParams.ProtocolId = NDIS_PROTOCOL_ID_DEFAULT;
    poolParams.fAllocateNetBuffer = TRUE;

    adapter->NblPool = NdisAllocateNetBufferListPool(MiniportAdapterHandle, &poolParams);
    if (!adapter->NblPool) {
        AerovNetCleanupAdapter(adapter);
        return NDIS_STATUS_RESOURCES;
    }

    status = AerovNetVirtioStart(adapter);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    /* General attributes. */
    RtlZeroMemory(&gen, sizeof(gen));
    gen.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES;
    gen.Header.Revision = NDIS_MINIPORT_ADAPTER_GENERAL_ATTRIBUTES_REVISION_2;
    gen.Header.Size = sizeof(gen);
    gen.MediaType = NdisMedium802_3;
    gen.PhysicalMediumType = NdisPhysicalMedium802_3;
    gen.MtuSize = adapter->Mtu;
    gen.MaxXmitLinkSpeed = g_DefaultLinkSpeedBps;
    gen.MaxRcvLinkSpeed = g_DefaultLinkSpeedBps;
    gen.XmitLinkSpeed = g_DefaultLinkSpeedBps;
    gen.RcvLinkSpeed = g_DefaultLinkSpeedBps;
    gen.MediaConnectState = adapter->LinkUp ? MediaConnectStateConnected : MediaConnectStateDisconnected;
    gen.MediaDuplexState = MediaDuplexStateFull;
    gen.LookaheadSize = adapter->Mtu;
    gen.MacAddressLength = ETH_LENGTH_OF_ADDRESS;
    gen.PermanentMacAddress = adapter->PermanentMac;
    gen.CurrentMacAddress = adapter->CurrentMac;
    gen.SupportedPacketFilters = NDIS_PACKET_TYPE_DIRECTED | NDIS_PACKET_TYPE_MULTICAST | NDIS_PACKET_TYPE_ALL_MULTICAST |
                                 NDIS_PACKET_TYPE_BROADCAST | NDIS_PACKET_TYPE_PROMISCUOUS;
    gen.MaxMulticastListSize = NDIS_MAX_MULTICAST_LIST;
    gen.MacOptions = NDIS_MAC_OPTION_COPY_LOOKAHEAD_DATA | NDIS_MAC_OPTION_NO_LOOPBACK;
    gen.SupportedStatistics = NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_FRAMES_XMIT |
                              NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_RCV | NDIS_STATISTICS_FLAGS_VALID_DIRECTED_BYTES_XMIT;
    gen.SupportedOidList = (PVOID)g_SupportedOids;
    gen.SupportedOidListLength = sizeof(g_SupportedOids);

    status = NdisMSetMiniportAttributes(MiniportAdapterHandle, (PNDIS_MINIPORT_ADAPTER_ATTRIBUTES)&gen);
    if (status != NDIS_STATUS_SUCCESS) {
        AerovNetCleanupAdapter(adapter);
        return status;
    }

    NdisAcquireSpinLock(&adapter->Lock);
    adapter->State = AerovNetAdapterRunning;
    AerovNetFillRxQueueLocked(adapter);
    NdisReleaseSpinLock(&adapter->Lock);

    AerovNetIndicateLinkState(adapter);

    return NDIS_STATUS_SUCCESS;
}

static VOID AerovNetDriverUnload(_In_ PDRIVER_OBJECT DriverObject)
{
    UNREFERENCED_PARAMETER(DriverObject);

    if (g_NdisDriverHandle) {
        NdisMDeregisterMiniportDriver(g_NdisDriverHandle);
        g_NdisDriverHandle = NULL;
    }
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    NDIS_STATUS status;
    NDIS_MINIPORT_DRIVER_CHARACTERISTICS ch;

    RtlZeroMemory(&ch, sizeof(ch));
    ch.Header.Type = NDIS_OBJECT_TYPE_MINIPORT_DRIVER_CHARACTERISTICS;
    ch.Header.Revision = NDIS_MINIPORT_DRIVER_CHARACTERISTICS_REVISION_2;
    ch.Header.Size = sizeof(ch);

    ch.MajorNdisVersion = 6;
    ch.MinorNdisVersion = 20;
    ch.MajorDriverVersion = 1;
    ch.MinorDriverVersion = 0;
    ch.InitializeHandlerEx = AerovNetMiniportInitializeEx;
    ch.HaltHandlerEx = AerovNetMiniportHaltEx;
    ch.PauseHandler = AerovNetMiniportPause;
    ch.RestartHandler = AerovNetMiniportRestart;
    ch.OidRequestHandler = AerovNetMiniportOidRequest;
    ch.SendNetBufferListsHandler = AerovNetMiniportSendNetBufferLists;
    ch.ReturnNetBufferListsHandler = AerovNetMiniportReturnNetBufferLists;
    ch.CancelSendHandler = AerovNetMiniportCancelSend;
    ch.DevicePnPEventNotifyHandler = AerovNetMiniportDevicePnPEventNotify;

    status = NdisMRegisterMiniportDriver(DriverObject, RegistryPath, NULL, &ch, &g_NdisDriverHandle);
    if (status != NDIS_STATUS_SUCCESS) {
        g_NdisDriverHandle = NULL;
        return status;
    }

    DriverObject->DriverUnload = AerovNetDriverUnload;
    return STATUS_SUCCESS;
}
