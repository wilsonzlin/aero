/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "../include/virtio_pci_modern_miniport.h"

/* -------------------------------------------------------------------------- */
/* OS interface for the canonical VirtioPciModernTransport                     */
/* -------------------------------------------------------------------------- */

static __forceinline UINT16 VirtioPciMiniportReadLe16(_In_reads_bytes_(Offset + 2) const UCHAR *Bytes, _In_ UINT16 Offset)
{
    return (UINT16)Bytes[Offset + 0] | ((UINT16)Bytes[Offset + 1] << 8);
}

static __forceinline UINT32 VirtioPciMiniportReadLe32(_In_reads_bytes_(Offset + 4) const UCHAR *Bytes, _In_ UINT16 Offset)
{
    return (UINT32)Bytes[Offset + 0] | ((UINT32)Bytes[Offset + 1] << 8) | ((UINT32)Bytes[Offset + 2] << 16) |
           ((UINT32)Bytes[Offset + 3] << 24);
}

static UINT8 VirtioPciMiniportPciRead8(_In_ void *Context, _In_ UINT16 Offset)
{
    VIRTIO_PCI_DEVICE *Dev;

    Dev = (VIRTIO_PCI_DEVICE *)Context;
    if (Dev == NULL || Offset >= (UINT16)sizeof(Dev->PciCfg)) {
        return 0;
    }

    return (UINT8)Dev->PciCfg[Offset];
}

static UINT16 VirtioPciMiniportPciRead16(_In_ void *Context, _In_ UINT16 Offset)
{
    VIRTIO_PCI_DEVICE *Dev;

    Dev = (VIRTIO_PCI_DEVICE *)Context;
    if (Dev == NULL || (UINT32)Offset + 2u > (UINT32)sizeof(Dev->PciCfg)) {
        return 0;
    }

    return VirtioPciMiniportReadLe16(Dev->PciCfg, Offset);
}

static UINT32 VirtioPciMiniportPciRead32(_In_ void *Context, _In_ UINT16 Offset)
{
    VIRTIO_PCI_DEVICE *Dev;

    Dev = (VIRTIO_PCI_DEVICE *)Context;
    if (Dev == NULL || (UINT32)Offset + 4u > (UINT32)sizeof(Dev->PciCfg)) {
        return 0;
    }

    return VirtioPciMiniportReadLe32(Dev->PciCfg, Offset);
}

static NTSTATUS VirtioPciMiniportMapMmio(_In_ void *Context,
                                        _In_ UINT64 PhysicalAddress,
                                        _In_ UINT32 Length,
                                        _Out_ volatile void **MappedVaOut)
{
    VIRTIO_PCI_DEVICE *Dev;

    UNREFERENCED_PARAMETER(PhysicalAddress);

    Dev = (VIRTIO_PCI_DEVICE *)Context;
    if (Dev == NULL || MappedVaOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *MappedVaOut = NULL;

    if (Dev->Bar0Va == NULL || Dev->Bar0Length == 0) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (Length == 0 || Length > Dev->Bar0Length) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    *MappedVaOut = (volatile void *)Dev->Bar0Va;
    return STATUS_SUCCESS;
}

static void VirtioPciMiniportUnmapMmio(_In_ void *Context, _In_ volatile void *MappedVa, _In_ UINT32 Length)
{
    UNREFERENCED_PARAMETER(Context);
    UNREFERENCED_PARAMETER(MappedVa);
    UNREFERENCED_PARAMETER(Length);
}

static void VirtioPciMiniportStallUs(_In_ void *Context, _In_ UINT32 Microseconds)
{
    UNREFERENCED_PARAMETER(Context);
    KeStallExecutionProcessor(Microseconds);
}

static void *VirtioPciMiniportSpinlockCreate(_In_ void *Context)
{
    VIRTIO_PCI_DEVICE *Dev;

    Dev = (VIRTIO_PCI_DEVICE *)Context;
    if (Dev == NULL) {
        return NULL;
    }

    /* Reuse the lock embedded in the device structure; no allocation. */
    return &Dev->CommonCfgLock;
}

static void VirtioPciMiniportSpinlockDestroy(_In_ void *Context, _In_ void *Lock)
{
    UNREFERENCED_PARAMETER(Context);
    UNREFERENCED_PARAMETER(Lock);
}

static void VirtioPciMiniportSpinlockAcquire(_In_ void *Context,
                                            _In_ void *Lock,
                                            _Out_ VIRTIO_PCI_MODERN_SPINLOCK_STATE *StateOut)
{
    KIRQL oldIrql;

    UNREFERENCED_PARAMETER(Context);

    if (StateOut == NULL) {
        return;
    }

    if (Lock == NULL) {
        *StateOut = 0;
        return;
    }

    KeAcquireSpinLock((KSPIN_LOCK *)Lock, &oldIrql);
    *StateOut = (VIRTIO_PCI_MODERN_SPINLOCK_STATE)oldIrql;
}

static void VirtioPciMiniportSpinlockRelease(_In_ void *Context, _In_ void *Lock, _In_ VIRTIO_PCI_MODERN_SPINLOCK_STATE State)
{
    UNREFERENCED_PARAMETER(Context);

    if (Lock == NULL) {
        return;
    }

    KeReleaseSpinLock((KSPIN_LOCK *)Lock, (KIRQL)State);
}

/* -------------------------------------------------------------------------- */
/* Public miniport API                                                         */
/* -------------------------------------------------------------------------- */

NTSTATUS VirtioPciModernMiniportInit(_Out_ VIRTIO_PCI_DEVICE *Dev,
                                    _In_ PUCHAR Bar0Va,
                                    _In_ ULONG Bar0Length,
                                    _In_ UINT64 Bar0Pa,
                                    _In_reads_bytes_(PciCfgLength) const UCHAR *PciCfg,
                                    _In_ ULONG PciCfgLength)
{
    NTSTATUS status;

    if (Dev == NULL || Bar0Va == NULL || Bar0Length == 0 || Bar0Pa == 0 || PciCfg == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    /* The canonical transport reads the full 256-byte config header. */
    if (PciCfgLength < sizeof(Dev->PciCfg)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlZeroMemory(Dev, sizeof(*Dev));
    Dev->Bar0Va = Bar0Va;
    Dev->Bar0Length = Bar0Length;

    RtlCopyMemory(Dev->PciCfg, PciCfg, sizeof(Dev->PciCfg));

    KeInitializeSpinLock(&Dev->CommonCfgLock);

    RtlZeroMemory(&Dev->Os, sizeof(Dev->Os));
    Dev->Os.Context = Dev;
    Dev->Os.PciRead8 = VirtioPciMiniportPciRead8;
    Dev->Os.PciRead16 = VirtioPciMiniportPciRead16;
    Dev->Os.PciRead32 = VirtioPciMiniportPciRead32;
    Dev->Os.MapMmio = VirtioPciMiniportMapMmio;
    Dev->Os.UnmapMmio = VirtioPciMiniportUnmapMmio;
    Dev->Os.StallUs = VirtioPciMiniportStallUs;
    Dev->Os.MemoryBarrier = NULL;
    Dev->Os.SpinlockCreate = VirtioPciMiniportSpinlockCreate;
    Dev->Os.SpinlockDestroy = VirtioPciMiniportSpinlockDestroy;
    Dev->Os.SpinlockAcquire = VirtioPciMiniportSpinlockAcquire;
    Dev->Os.SpinlockRelease = VirtioPciMiniportSpinlockRelease;
    Dev->Os.Log = NULL;

    status = VirtioPciModernTransportInit(&Dev->Transport, &Dev->Os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, Bar0Pa, (UINT32)Bar0Length);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernTransportUninit(&Dev->Transport);
        return status;
    }

    Dev->CommonCfg = Dev->Transport.CommonCfg;
    Dev->NotifyBase = (volatile UCHAR *)Dev->Transport.NotifyBase;
    Dev->IsrStatus = (volatile UCHAR *)Dev->Transport.IsrStatus;
    Dev->DeviceCfg = (volatile UCHAR *)Dev->Transport.DeviceCfg;

    Dev->NotifyOffMultiplier = (ULONG)Dev->Transport.NotifyOffMultiplier;

    Dev->CommonCfgOffset = (ULONG)((ULONG_PTR)Dev->CommonCfg - (ULONG_PTR)Dev->Bar0Va);
    Dev->NotifyOffset = (ULONG)((ULONG_PTR)Dev->NotifyBase - (ULONG_PTR)Dev->Bar0Va);
    Dev->IsrOffset = (ULONG)((ULONG_PTR)Dev->IsrStatus - (ULONG_PTR)Dev->Bar0Va);
    Dev->DeviceCfgOffset = (ULONG)((ULONG_PTR)Dev->DeviceCfg - (ULONG_PTR)Dev->Bar0Va);

    /* The canonical transport enforces these minimum lengths in STRICT mode. */
    Dev->CommonCfgLength = 0x0100u;
    Dev->NotifyLength = (ULONG)Dev->Transport.NotifyLength;
    Dev->IsrLength = (ULONG)Dev->Transport.IsrLength;
    Dev->DeviceCfgLength = (ULONG)Dev->Transport.DeviceCfgLength;

    return STATUS_SUCCESS;
}

VOID VirtioPciResetDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return;
    }
    VirtioPciModernTransportResetDevice(&Dev->Transport);
}

VOID VirtioPciAddStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Bits)
{
    if (Dev == NULL) {
        return;
    }
    VirtioPciModernTransportAddStatus(&Dev->Transport, (UINT8)Bits);
}

UCHAR VirtioPciGetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return 0;
    }
    return (UCHAR)VirtioPciModernTransportGetStatus(&Dev->Transport);
}

VOID VirtioPciSetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Status)
{
    if (Dev == NULL) {
        return;
    }
    VirtioPciModernTransportSetStatus(&Dev->Transport, (UINT8)Status);
}

VOID VirtioPciFailDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    VirtioPciAddStatus(Dev, VIRTIO_STATUS_FAILED);
}

UINT64 VirtioPciReadDeviceFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return 0;
    }
    return VirtioPciModernTransportReadDeviceFeatures(&Dev->Transport);
}

VOID VirtioPciWriteDriverFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UINT64 Features)
{
    if (Dev == NULL) {
        return;
    }
    VirtioPciModernTransportWriteDriverFeatures(&Dev->Transport, Features);
}

NTSTATUS VirtioPciNegotiateFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                                   _In_ UINT64 Required,
                                   _In_ UINT64 Wanted,
                                   _Out_ UINT64 *NegotiatedOut)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportNegotiateFeatures(&Dev->Transport, Required, Wanted, NegotiatedOut);
}

NTSTATUS VirtioPciReadDeviceConfig(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                                  _In_ ULONG Offset,
                                  _Out_writes_bytes_(Length) VOID *Buffer,
                                  _In_ ULONG Length)
{
    if (Dev == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    return VirtioPciModernTransportReadDeviceConfig(&Dev->Transport, (UINT32)Offset, Buffer, (UINT32)Length);
}

USHORT VirtioPciGetNumQueues(_In_ VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return 0;
    }

    return VirtioPciModernTransportGetNumQueues(&Dev->Transport);
}

USHORT VirtioPciGetQueueSize(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    UINT16 size;

    if (Dev == NULL) {
        return 0;
    }

    size = 0;
    if (!NT_SUCCESS(VirtioPciModernTransportGetQueueSize(&Dev->Transport, (UINT16)QueueIndex, &size))) {
        return 0;
    }

    return (USHORT)size;
}

NTSTATUS VirtioPciSetupQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                            _In_ USHORT QueueIndex,
                            _In_ UINT64 DescPa,
                            _In_ UINT64 AvailPa,
                            _In_ UINT64 UsedPa)
{
    if (Dev == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    return VirtioPciModernTransportSetupQueue(&Dev->Transport, (UINT16)QueueIndex, DescPa, AvailPa, UsedPa);
}

VOID VirtioPciDisableQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    if (Dev == NULL) {
        return;
    }

    VirtioPciModernTransportDisableQueue(&Dev->Transport, (UINT16)QueueIndex);
}

NTSTATUS VirtioPciGetQueueNotifyAddress(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                                       _In_ USHORT QueueIndex,
                                       _Out_ volatile UINT16 **NotifyAddrOut)
{
    NTSTATUS status;
    UINT16 notifyOff;
    UINT64 offset;

    if (NotifyAddrOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    *NotifyAddrOut = NULL;

    if (Dev == NULL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = VirtioPciModernTransportGetQueueNotifyOff(&Dev->Transport, (UINT16)QueueIndex, &notifyOff);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    offset = (UINT64)notifyOff * (UINT64)Dev->NotifyOffMultiplier;
    if (offset + sizeof(UINT16) > (UINT64)Dev->NotifyLength) {
        return STATUS_IO_DEVICE_ERROR;
    }

    *NotifyAddrOut = (volatile UINT16 *)((volatile UCHAR *)Dev->NotifyBase + (ULONG_PTR)offset);
    return STATUS_SUCCESS;
}

VOID VirtioPciNotifyQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex)
{
    volatile UINT16 *notifyAddr;

    if (Dev == NULL) {
        return;
    }

    notifyAddr = NULL;
    if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
        notifyAddr = Dev->QueueNotifyAddrCache[QueueIndex];
    }

    if (notifyAddr == NULL) {
        if (!NT_SUCCESS(VirtioPciGetQueueNotifyAddress(Dev, QueueIndex, &notifyAddr)) || notifyAddr == NULL) {
            return;
        }

        if (Dev->QueueNotifyAddrCache != NULL && QueueIndex < Dev->QueueNotifyAddrCacheCount) {
            Dev->QueueNotifyAddrCache[QueueIndex] = notifyAddr;
        }
    }

    /* Publish ring writes before notifying. */
    KeMemoryBarrier();
    WRITE_REGISTER_USHORT((volatile USHORT *)notifyAddr, QueueIndex);
    KeMemoryBarrier();
}

UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE *Dev)
{
    if (Dev == NULL) {
        return 0;
    }

    return (UCHAR)VirtioPciModernTransportReadIsrStatus((VIRTIO_PCI_MODERN_TRANSPORT *)&Dev->Transport);
}
