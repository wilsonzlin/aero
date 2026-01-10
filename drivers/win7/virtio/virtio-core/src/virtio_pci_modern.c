#include "virtio_pci_modern.h"

static __forceinline void VirtioPciSelectQueueLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                                     _In_ USHORT QueueIndex)
{
#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    WRITE_REGISTER_USHORT((volatile USHORT*)&Dev->CommonCfg->queue_select, QueueIndex);
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciModernInit(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                             _In_ WDFDEVICE WdfDevice,
                             _In_ volatile virtio_pci_common_cfg* CommonCfg)
{
    WDF_OBJECT_ATTRIBUTES attributes;
    NTSTATUS status;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(WdfDevice != NULL);
    NT_ASSERT(CommonCfg != NULL);
    NT_ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    Dev->WdfDevice = WdfDevice;
    Dev->CommonCfg = CommonCfg;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.ParentObject = WdfDevice;

    status = WdfSpinLockCreate(&attributes, &Dev->CommonCfgLock);
    if (!NT_SUCCESS(status)) {
        Dev->CommonCfgLock = NULL;
        return status;
    }

#if DBG
    Dev->CommonCfgLockOwner = NULL;
#endif

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciCommonCfgLock(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
#if DBG
    PKTHREAD currentThread;
#endif

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfgLock != NULL);
    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

#if DBG
    currentThread = KeGetCurrentThread();
    NT_ASSERT(Dev->CommonCfgLockOwner != currentThread);

    WdfSpinLockAcquire(Dev->CommonCfgLock);

    NT_ASSERT(Dev->CommonCfgLockOwner == NULL);
    Dev->CommonCfgLockOwner = currentThread;
#else
    WdfSpinLockAcquire(Dev->CommonCfgLock);
#endif
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciCommonCfgUnlock(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfgLock != NULL);
    NT_ASSERT(KeGetCurrentIrql() <= DISPATCH_LEVEL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
    Dev->CommonCfgLockOwner = NULL;
#endif

    WdfSpinLockRelease(Dev->CommonCfgLock);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64 VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
    UINT64 features;

    VirtioPciCommonCfgLock(Dev);
    features = VirtioPciReadDeviceFeaturesLocked(Dev);
    VirtioPciCommonCfgUnlock(Dev);
    return features;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64 VirtioPciReadDeviceFeaturesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev)
{
    ULONG lo = 0;
    ULONG hi = 0;

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->device_feature_select, 0);
    lo = READ_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->device_feature);

    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->device_feature_select, 1);
    hi = READ_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->device_feature);

    return ((UINT64)hi << 32) | lo;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueSize(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex)
{
    USHORT size;

    VirtioPciCommonCfgLock(Dev);
    size = VirtioPciReadQueueSizeLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);
    return size;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueSizeLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif
    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT*)&Dev->CommonCfg->queue_size);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UINT64 Features)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteDriverFeaturesLocked(Dev, Features);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteDriverFeaturesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UINT64 Features)
{
    const ULONG lo = (ULONG)(Features & 0xFFFFFFFFull);
    const ULONG hi = (ULONG)(Features >> 32);

    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->driver_feature_select, 0);
    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->driver_feature, lo);

    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->driver_feature_select, 1);
    WRITE_REGISTER_ULONG((volatile ULONG*)&Dev->CommonCfg->driver_feature, hi);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueNotifyOffset(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex)
{
    USHORT notifyOff;

    VirtioPciCommonCfgLock(Dev);
    notifyOff = VirtioPciReadQueueNotifyOffsetLocked(Dev, QueueIndex);
    VirtioPciCommonCfgUnlock(Dev);
    return notifyOff;
}

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueNotifyOffsetLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    return READ_REGISTER_USHORT((volatile USHORT*)&Dev->CommonCfg->queue_notify_off);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueAddresses(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                  _In_ USHORT QueueIndex,
                                  _In_ UINT64 Desc,
                                  _In_ UINT64 Avail,
                                  _In_ UINT64 Used)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteQueueAddressesLocked(Dev, QueueIndex, Desc, Avail, Used);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueAddressesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                        _In_ USHORT QueueIndex,
                                        _In_ UINT64 Desc,
                                        _In_ UINT64 Avail,
                                        _In_ UINT64 Used)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);

    WRITE_REGISTER_ULONG64((volatile ULONG64*)&Dev->CommonCfg->queue_desc, (ULONG64)Desc);
    WRITE_REGISTER_ULONG64((volatile ULONG64*)&Dev->CommonCfg->queue_avail, (ULONG64)Avail);
    WRITE_REGISTER_ULONG64((volatile ULONG64*)&Dev->CommonCfg->queue_used, (ULONG64)Used);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueEnable(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                               _In_ USHORT QueueIndex,
                               _In_ BOOLEAN Enable)
{
    VirtioPciCommonCfgLock(Dev);
    VirtioPciWriteQueueEnableLocked(Dev, QueueIndex, Enable);
    VirtioPciCommonCfgUnlock(Dev);
}

_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueEnableLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                     _In_ USHORT QueueIndex,
                                     _In_ BOOLEAN Enable)
{
    NT_ASSERT(Dev != NULL);
    NT_ASSERT(Dev->CommonCfg != NULL);

#if DBG
    NT_ASSERT(Dev->CommonCfgLockOwner == KeGetCurrentThread());
#endif

    VirtioPciSelectQueueLocked(Dev, QueueIndex);
    WRITE_REGISTER_USHORT((volatile USHORT*)&Dev->CommonCfg->queue_enable, Enable ? 1 : 0);
}
