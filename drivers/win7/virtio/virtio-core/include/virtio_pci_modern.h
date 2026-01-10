#pragma once

#include <ntddk.h>
#include <wdf.h>

//
// Virtio 1.0+ PCI transport ("modern") common configuration.
//
// Note: The virtio_pci_common_cfg register block contains selector registers
// (device_feature_select/driver_feature_select/queue_select) that act as global
// selectors for the rest of the fields in the capability. Any multi-step access
// that uses a selector must be serialized to avoid corrupting device state when
// multiple threads (queues, DPCs, power callbacks, etc.) touch common_cfg
// concurrently.
//

#pragma pack(push, 1)
typedef struct _virtio_pci_common_cfg {
    ULONG device_feature_select; /* read-write */
    ULONG device_feature;        /* read-only  */
    ULONG driver_feature_select; /* read-write */
    ULONG driver_feature;        /* read-write */
    USHORT msix_config;          /* read-write */
    USHORT num_queues;           /* read-only  */
    UCHAR device_status;         /* read-write */
    UCHAR config_generation;     /* read-only  */

    USHORT queue_select;      /* read-write */
    USHORT queue_size;        /* read-only  */
    USHORT queue_msix_vector; /* read-write */
    USHORT queue_enable;      /* read-write */
    USHORT queue_notify_off;  /* read-only  */
    ULONG64 queue_desc;  /* read-write */
    ULONG64 queue_avail; /* read-write */
    ULONG64 queue_used;  /* read-write */
} virtio_pci_common_cfg, *Pvirtio_pci_common_cfg;
#pragma pack(pop)

//
// CommonCfg offsets are defined by the virtio spec. Assert the layout so any
// accidental padding or stray fields are caught at compile time.
//
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature_select) == 0x00);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_feature) == 0x04);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature_select) == 0x08);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, driver_feature) == 0x0C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, msix_config) == 0x10);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, num_queues) == 0x12);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, device_status) == 0x14);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, config_generation) == 0x15);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_select) == 0x16);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_size) == 0x18);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_msix_vector) == 0x1A);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_enable) == 0x1C);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_notify_off) == 0x1E);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_desc) == 0x20);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_avail) == 0x28);
C_ASSERT(FIELD_OFFSET(virtio_pci_common_cfg, queue_used) == 0x30);
C_ASSERT(sizeof(virtio_pci_common_cfg) == 0x38);

typedef struct _VIRTIO_PCI_DEVICE {
    WDFDEVICE WdfDevice;
    volatile virtio_pci_common_cfg* CommonCfg;

    //
    // Serializes selector-based accesses to CommonCfg (feature_select and
    // queue_select sequences). Must be usable at <= DISPATCH_LEVEL.
    //
    WDFSPINLOCK CommonCfgLock;

#if DBG
    PKTHREAD CommonCfgLockOwner;
#endif
} VIRTIO_PCI_DEVICE, *PVIRTIO_PCI_DEVICE;

//
// Initialization
//

// Creates the per-device CommonCfg spinlock.
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciModernInit(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                             _In_ WDFDEVICE WdfDevice,
                             _In_ volatile virtio_pci_common_cfg* CommonCfg);

//
// CommonCfg lock helpers
//

// Acquires the per-device CommonCfg lock.
//
// IRQL: <= DISPATCH_LEVEL. Safe to call from DPC context.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciCommonCfgLock(_Inout_ PVIRTIO_PCI_DEVICE Dev);

// Releases the per-device CommonCfg lock.
//
// IRQL: <= DISPATCH_LEVEL. Safe to call from DPC context.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciCommonCfgUnlock(_Inout_ PVIRTIO_PCI_DEVICE Dev);

//
// Selector-based CommonCfg helpers (internally serialized by CommonCfgLock)
//
// Functions without the "Locked" suffix acquire/release the CommonCfg lock
// internally and must not be called while holding the lock. Callers that need
// to perform a multi-step sequence atomically should use
// VirtioPciCommonCfgLock/Unlock and then call the corresponding *Locked()
// helper(s).

// Reads the full 64-bit device feature bitmap.
//
// IRQL: <= DISPATCH_LEVEL. Serializes feature_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64 VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev);

// Reads the full 64-bit device feature bitmap.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64 VirtioPciReadDeviceFeaturesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev);

// Writes the full 64-bit driver feature bitmap.
//
// IRQL: <= DISPATCH_LEVEL. Serializes feature_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UINT64 Features);

// Writes the full 64-bit driver feature bitmap.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteDriverFeaturesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UINT64 Features);

// Reads queue_size for the given queue.
//
// IRQL: <= DISPATCH_LEVEL. Serializes queue_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueSize(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex);

// Reads queue_size for the given queue.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueSizeLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex);

// Reads queue_notify_off for the given queue.
//
// IRQL: <= DISPATCH_LEVEL. Serializes queue_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueNotifyOffset(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex);

// Reads queue_notify_off for the given queue.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT VirtioPciReadQueueNotifyOffsetLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ USHORT QueueIndex);

// Programs the queue descriptor/avail/used addresses for the given queue.
//
// IRQL: <= DISPATCH_LEVEL. Serializes queue_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueAddresses(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                  _In_ USHORT QueueIndex,
                                  _In_ UINT64 Desc,
                                  _In_ UINT64 Avail,
                                  _In_ UINT64 Used);

// Programs the queue descriptor/avail/used addresses for the given queue.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueAddressesLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                        _In_ USHORT QueueIndex,
                                        _In_ UINT64 Desc,
                                        _In_ UINT64 Avail,
                                        _In_ UINT64 Used);

// Enables/disables the given queue.
//
// IRQL: <= DISPATCH_LEVEL. Serializes queue_select accesses.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueEnable(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                               _In_ USHORT QueueIndex,
                               _In_ BOOLEAN Enable);

// Enables/disables the given queue.
//
// Caller must hold the CommonCfg lock.
// IRQL: <= DISPATCH_LEVEL.
_IRQL_requires_max_(DISPATCH_LEVEL)
void VirtioPciWriteQueueEnableLocked(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                                     _In_ USHORT QueueIndex,
                                     _In_ BOOLEAN Enable);
