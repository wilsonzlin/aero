/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * WDM-only virtio-pci "modern" (Virtio 1.0+) transport helpers.
 *
 * This module is intended to satisfy the transport requirements described in:
 *   docs/windows7-virtio-driver-contract.md
 *
 * Key properties:
 *  - Modern-only (PCI vendor capabilities + MMIO), no legacy I/O-port transport.
 *  - BAR mapping via MmMapIoSpace (MmNonCached).
 *  - INTx-friendly ISR region (read-to-ack).
 *  - Selector register serialization via a per-device spin lock.
 *
 * This header intentionally does not include any KMDF/WDF headers.
 */

#pragma once

#include <ntddk.h>
#include <wdmguid.h>

/*
 * Reuse the existing virtio-core headers for the common_cfg layout and PCI
 * capability structures. These headers have no WDF dependencies.
 */
#include "../../../../win7/virtio/virtio-core/include/virtio_pci_caps.h"
#include "../../../../win7/virtio/virtio-core/include/virtio_spec.h"

/*
 * Compile-time diagnostics switch.
 *
 * Define VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS=1 in the driver's project to
 * enable DbgPrintEx logging from this module.
 */
#ifndef VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS
#define VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS 0
#endif

#if VIRTIO_PCI_MODERN_WDM_ENABLE_DIAGNOSTICS
#define VIRTIO_PCI_MODERN_WDM_PRINT(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "[virtio-pci-modern-wdm] " __VA_ARGS__)
#else
#define VIRTIO_PCI_MODERN_WDM_PRINT(...) ((void)0)
#endif

typedef struct _VIRTIO_PCI_MODERN_WDM_BAR {
    BOOLEAN Present;
    BOOLEAN IsMemory;
    BOOLEAN Is64Bit;
    BOOLEAN IsUpperHalf; /* for 64-bit BARs, the high dword slot */

    ULONGLONG Base; /* bus address base as programmed in config space */

    /* Matched resources (from IRP_MN_START_DEVICE). */
    PHYSICAL_ADDRESS RawStart;
    PHYSICAL_ADDRESS TranslatedStart;
    SIZE_T Length;

    /* Mapped MMIO virtual address (MmMapIoSpace). */
    PVOID Va;
} VIRTIO_PCI_MODERN_WDM_BAR, *PVIRTIO_PCI_MODERN_WDM_BAR;

typedef struct _VIRTIO_PCI_MODERN_WDM_DEVICE {
    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;

    UCHAR PciRevisionId;

    VIRTIO_PCI_CAPS Caps;

    VIRTIO_PCI_MODERN_WDM_BAR Bars[VIRTIO_PCI_MAX_BARS];

    /* Per-capability MMIO pointers (valid after VirtioPciModernWdmMapBars). */
    volatile virtio_pci_common_cfg *CommonCfg;
    volatile UCHAR *NotifyBase;
    ULONG NotifyOffMultiplier;
    SIZE_T NotifyLength;
    volatile UCHAR *IsrStatus;
    volatile UCHAR *DeviceCfg;

    /*
     * Optional per-queue cached notify addresses.
     *
     * If provided by the caller, QueueNotifyAddrCache must point to an array
     * of QueueNotifyAddrCacheCount entries, typically equal to CommonCfg->num_queues.
     * Entries are populated on-demand by VirtioPciNotifyQueue().
     *
     * The cache is invalidated (zeroed) whenever BARs are unmapped.
     */
    volatile UINT16 **QueueNotifyAddrCache;
    USHORT QueueNotifyAddrCacheCount;

    /*
     * The virtio_pci_common_cfg register block contains selector registers
     * (device_feature_select/driver_feature_select/queue_select) that act as
     * global selectors for subsequent MMIO accesses. These sequences must be
     * serialized across threads/cores/DPCs to avoid corrupting device state.
     */
    KSPIN_LOCK CommonCfgLock;

#if DBG
    PKTHREAD CommonCfgLockOwner;
#endif
} VIRTIO_PCI_MODERN_WDM_DEVICE, *PVIRTIO_PCI_MODERN_WDM_DEVICE;

/* Initialization / teardown */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmInit(_In_ PDEVICE_OBJECT LowerDeviceObject, _Out_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernWdmMapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ PCM_RESOURCE_LIST ResourcesRaw,
                          _In_ PCM_RESOURCE_LIST ResourcesTranslated);

/*
 * Unmaps any BARs previously mapped by VirtioPciModernWdmMapBars().
 *
 * This is useful for PnP stop/remove paths where the driver must release
 * translated memory resources (MmUnmapIoSpace).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUnmapBars(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmUninit(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

/* Diagnostics */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpCaps(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE *Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernWdmDumpBars(_In_ const VIRTIO_PCI_MODERN_WDM_DEVICE *Dev);

/* CommonCfg selector serialization helpers (<= DISPATCH_LEVEL). */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgAcquire(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _Out_ PKIRQL OldIrql);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciCommonCfgRelease(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ KIRQL OldIrql);

/* -------------------------------------------------------------------------- */
/* Transport operations                                                        */
/* -------------------------------------------------------------------------- */

/* Resets the device by writing 0 to device_status and polling until it reads 0. */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciResetDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

/* ORs Bits into device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciAddStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UCHAR Bits);

/* Reads device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
UCHAR
VirtioPciGetStatus(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

/* Sets the FAILED bit in device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciFailDevice(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

/* Feature access (64-bit) */
_IRQL_requires_max_(DISPATCH_LEVEL)
UINT64
VirtioPciReadDeviceFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteDriverFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ UINT64 Features);

/*
 * Virtio 1.0 feature negotiation helper.
 *
 * Sequence:
 *  - Reset
 *  - ACKNOWLEDGE + DRIVER
 *  - Read device features
 *  - negotiated = (device & Wanted) | Required
 *  - Always require VIRTIO_F_VERSION_1
 *  - Write negotiated features
 *  - Set FEATURES_OK
 *  - Re-read status to ensure FEATURES_OK was accepted
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                           _In_ UINT64 Required,
                           _In_ UINT64 Wanted,
                           _Out_ UINT64 *NegotiatedOut);

/* Device-specific config access */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) PVOID Buffer,
                          _In_ ULONG Length);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciWriteDeviceConfig(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                           _In_ ULONG Offset,
                           _In_reads_bytes_(Length) const VOID *Buffer,
                           _In_ ULONG Length);

/* Queue helpers */
_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciGetNumQueues(_In_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueSize(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciSetupQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                    _In_ USHORT QueueIndex,
                    _In_ ULONGLONG DescPa,
                    _In_ ULONGLONG AvailPa,
                    _In_ ULONGLONG UsedPa);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDisableQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex);

/* Notify helpers */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16 **NotifyAddrOut);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciNotifyQueue(_Inout_ PVIRTIO_PCI_MODERN_WDM_DEVICE Dev, _In_ USHORT QueueIndex);
