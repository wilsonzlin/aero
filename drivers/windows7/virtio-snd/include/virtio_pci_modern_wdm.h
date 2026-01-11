#pragma once

/*
 * WDM-only Virtio PCI "modern" (virtio 1.0+) transport helper for virtio-snd.
 *
 * This module implements the subset of virtio-pci modern required by the
 * Aero Windows 7 Virtio Device Contract (AERO-W7-VIRTIO v1).
 *
 * Scope:
 *  - PCI config discovery via PCI_BUS_INTERFACE_STANDARD.ReadConfig
 *  - Vendor capability parsing (COMMON/NOTIFY/ISR/DEVICE)
 *  - BAR0 MMIO mapping (MmMapIoSpace)
 *  - CommonCfg selector serialization (KSPIN_LOCK)
 *  - Feature negotiation (leaves device at FEATURES_OK)
 *  - Queue programming helpers + notify doorbell helpers
 *
 * Out of scope for this module:
 *  - Interrupt connection (INTx/MSI-X)
 *  - virtio-snd protocol messages
 *  - PortCls/miniport integration
 *
 * No WDF/KMDF dependencies are permitted.
 */

#include <ntddk.h>
#include <wdmguid.h>

#include "virtio_spec.h"
#include "virtio_pci_cap_parser.h"

typedef struct _VIRTIOSND_TRANSPORT {
    /*
     * Caller-owned lower device object (the next lower driver in the stack).
     * Used to query PCI_BUS_INTERFACE_STANDARD and read config space.
     */
    PDEVICE_OBJECT LowerDeviceObject;

    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;

    UCHAR PciRevisionId;

    /* BAR0 as programmed in PCI config space (masked base address). */
    ULONGLONG Bar0Base;

    /* Matched CM resources for BAR0 (from IRP_MN_START_DEVICE). */
    PHYSICAL_ADDRESS Bar0RawStart;
    PHYSICAL_ADDRESS Bar0TranslatedStart;
    SIZE_T Bar0Length;

    /* Mapped BAR0 VA (MmMapIoSpace). */
    PVOID Bar0Va;

    /* Parsed modern virtio PCI capabilities (vendor-specific caps). */
    virtio_pci_parsed_caps_t Caps;

    /* MMIO pointers (BAR0 VA + cap offsets). */
    volatile virtio_pci_common_cfg *CommonCfg;
    volatile UCHAR *NotifyBase;
    ULONG NotifyOffMultiplier;
    SIZE_T NotifyLength;
    volatile UCHAR *IsrStatus;
    volatile UCHAR *DeviceCfg;

    /*
     * The virtio_pci_common_cfg selector registers are global state. Any
     * multi-step sequence that touches selector-dependent fields must be
     * serialized (required by the contract).
     */
    KSPIN_LOCK CommonCfgLock;
} VIRTIOSND_TRANSPORT, *PVIRTIOSND_TRANSPORT;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndTransportInit(_Out_ PVIRTIOSND_TRANSPORT Transport,
                       _In_ PDEVICE_OBJECT LowerDeviceObject,
                       _In_ PCM_RESOURCE_LIST ResourcesRaw,
                       _In_ PCM_RESOURCE_LIST ResourcesTranslated);

_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtIoSndTransportUninit(_Inout_ PVIRTIOSND_TRANSPORT Transport);

/*
 * Negotiates required virtio feature bits and leaves the device at FEATURES_OK.
 *
 * Required by contract v1:
 *  - VIRTIO_F_VERSION_1 (bit 32)
 *  - VIRTIO_F_RING_INDIRECT_DESC (bit 28)
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtIoSndTransportNegotiateFeatures(_Inout_ PVIRTIOSND_TRANSPORT Transport, _Out_ UINT64 *NegotiatedOut);

/*
 * Reads the size of a virtqueue (queue_size) for the given index.
 * Returns STATUS_NOT_FOUND if the queue does not exist (size==0).
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportReadQueueSize(_Inout_ PVIRTIOSND_TRANSPORT Transport, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut);

/*
 * Reads the notify offset (queue_notify_off) for the given queue index.
 * Returns STATUS_NOT_FOUND if the queue does not exist.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportReadQueueNotifyOff(_Inout_ PVIRTIOSND_TRANSPORT Transport,
                                     _In_ USHORT QueueIndex,
                                     _Out_ USHORT *NotifyOffOut);

/*
 * Programs a virtqueue:
 *  - selects queue
 *  - reads queue_size and queue_notify_off
 *  - writes queue_desc/avail/used addresses
 *  - enables queue (queue_enable = 1) and verifies readback
 *
 * Returns STATUS_NOT_FOUND if the queue does not exist.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtIoSndTransportSetupQueue(_Inout_ PVIRTIOSND_TRANSPORT Transport,
                             _In_ USHORT QueueIndex,
                             _In_ UINT64 QueueDescPa,
                             _In_ UINT64 QueueAvailPa,
                             _In_ UINT64 QueueUsedPa,
                             _Out_opt_ USHORT *NotifyOffOut);

/*
 * Computes the MMIO notify doorbell address for a queue notify offset.
 *
 * Returns NULL if the transport has not been initialized or if the computed
 * address would fall outside the notify capability region.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
volatile UINT16 *
VirtIoSndTransportComputeNotifyAddr(_In_ const VIRTIOSND_TRANSPORT *Transport, _In_ USHORT QueueNotifyOff);

/*
 * Notifies a queue by writing the queue index (16-bit) to the computed notify
 * doorbell address.
 */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtIoSndTransportNotifyQueue(_In_ const VIRTIOSND_TRANSPORT *Transport,
                              _In_ USHORT QueueIndex,
                              _In_ USHORT QueueNotifyOff);

