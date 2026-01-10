#pragma once

/*
 * virtio-core: reusable Virtio 1.0 PCI "modern" discovery + BAR mapping for
 * Windows 7 KMDF drivers.
 */

#include <ntddk.h>
#include <wdf.h>
#include <wdmguid.h>

#include "virtio_pci_caps.h"
#include "virtio_spec.h"

/*
 * Compile-time diagnostics switch.
 *
 * Set VIRTIO_CORE_ENABLE_DIAGNOSTICS=1 in the driver's project to enable
 * DbgPrintEx logging from this library.
 */
#ifndef VIRTIO_CORE_ENABLE_DIAGNOSTICS
#define VIRTIO_CORE_ENABLE_DIAGNOSTICS 0
#endif

#if VIRTIO_CORE_ENABLE_DIAGNOSTICS
#define VIRTIO_CORE_PRINT(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "[virtio-core] " __VA_ARGS__)
#else
#define VIRTIO_CORE_PRINT(...) ((void)0)
#endif

typedef struct _VIRTIO_PCI_BAR {
    BOOLEAN Present;
    BOOLEAN IsMemory;
    BOOLEAN Is64Bit;
    BOOLEAN IsUpperHalf; /* for 64-bit BARs, the high dword slot */

    ULONGLONG Base; /* bus address base as programmed in config space */

    /* Matched resources (from EvtDevicePrepareHardware). */
    PHYSICAL_ADDRESS RawStart;
    PHYSICAL_ADDRESS TranslatedStart;
    SIZE_T Length;

    /* Mapped MMIO virtual address (MmMapIoSpace). */
    PVOID Va;
} VIRTIO_PCI_BAR, *PVIRTIO_PCI_BAR;

typedef struct _VIRTIO_PCI_MODERN_DEVICE {
    WDFDEVICE WdfDevice;

    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;

    VIRTIO_PCI_CAPS Caps;

    VIRTIO_PCI_BAR Bars[VIRTIO_PCI_MAX_BARS];

    /* Per-capability MMIO pointers (valid after VirtioPciModernMapBars). */
    volatile virtio_pci_common_cfg *CommonCfg;
    volatile UCHAR *NotifyBase;
    ULONG NotifyOffMultiplier;
    volatile UCHAR *IsrStatus;
    volatile UCHAR *DeviceCfg;

    /*
     * The virtio_pci_common_cfg register block contains selector registers
     * (device_feature_select/driver_feature_select/queue_select) that act as global
     * selectors for the rest of the fields in the capability. Any multi-step access
     * that uses a selector must be serialized to avoid corrupting device state when
     * multiple threads (queues, DPCs, power callbacks, etc.) touch common_cfg
     * concurrently.
     */
    WDFSPINLOCK CommonCfgLock;

#if DBG
    PKTHREAD CommonCfgLockOwner;
#endif
} VIRTIO_PCI_MODERN_DEVICE, *PVIRTIO_PCI_MODERN_DEVICE;

/* Initialization / discovery */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernInit(_In_ WDFDEVICE WdfDevice, _Out_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernMapBars(
    _Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated);

/*
 * Transport smoke-test helpers.
 *
 * These intentionally stop at FEATURES_OK (no DRIVER_OK / no virtqueues).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernResetDevice(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                           _In_ UINT64 RequestedFeatures,
                           _Out_opt_ UINT64 *NegotiatedFeatures);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernUninit(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

/* Diagnostics */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernDumpCaps(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernDumpBars(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev);

/*
 * CommonCfg lock helpers.
 *
 * IRQL: <= DISPATCH_LEVEL. Safe to call from DPC context.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgLock(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgUnlock(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

/*
 * Selector-based CommonCfg helpers (internally serialized by CommonCfgLock).
 *
 * Functions without the "Locked" suffix acquire/release the CommonCfg lock
 * internally and must not be called while holding the lock. Callers that need
 * to perform a multi-step sequence atomically should use
 * VirtioPciCommonCfgLock/Unlock and then call the corresponding *Locked()
 * helper(s).
 *
 * IRQL: <= DISPATCH_LEVEL.
 */

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ UINT64 Features);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeaturesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ UINT64 Features);

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueSize(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueSizeLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueNotifyOffset(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueNotifyOffsetLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueAddresses(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                             _In_ USHORT QueueIndex,
                             _In_ UINT64 Desc,
                             _In_ UINT64 Avail,
                             _In_ UINT64 Used);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueAddressesLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                   _In_ USHORT QueueIndex,
                                   _In_ UINT64 Desc,
                                   _In_ UINT64 Avail,
                                   _In_ UINT64 Used);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueEnable(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                          _In_ USHORT QueueIndex,
                          _In_ BOOLEAN Enable);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueEnableLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                _In_ USHORT QueueIndex,
                                _In_ BOOLEAN Enable);
