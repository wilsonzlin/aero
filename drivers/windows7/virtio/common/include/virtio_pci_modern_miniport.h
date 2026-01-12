/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * virtio-pci modern transport helpers for Windows 7 miniport-style drivers
 * (NDIS / StorPort).
 *
 * This module is intentionally KMDF/WDF-free: callers provide a BAR0 MMIO
 * mapping and a snapshot of PCI config space (typically 256 bytes).
 *
 * Contract: docs/windows7-virtio-driver-contract.md (modern-only, BAR0 MMIO).
 */

#pragma once

#include <ntddk.h>

/*
 * Reuse the canonical Virtio 1.0 definitions + virtio_pci_common_cfg layout
 * from virtio-core so offsets/sizes match the emulator/contract.
 */
#include "../../../../win7/virtio/virtio-core/include/virtio_spec.h"

typedef struct _VIRTIO_PCI_DEVICE {
    /* Caller-provided BAR0 MMIO mapping. */
    PUCHAR Bar0Va;
    ULONG Bar0Length;

    /* Parsed virtio vendor capability windows (BAR-relative). */
    ULONG CommonCfgOffset;
    ULONG CommonCfgLength;
    volatile virtio_pci_common_cfg *CommonCfg;

    ULONG NotifyOffset;
    ULONG NotifyLength;
    volatile UCHAR *NotifyBase;
    ULONG NotifyOffMultiplier;

    ULONG IsrOffset;
    ULONG IsrLength;
    volatile UCHAR *IsrStatus; /* read-to-ack */

    ULONG DeviceCfgOffset;
    ULONG DeviceCfgLength;
    volatile UCHAR *DeviceCfg;

    /*
     * Optional per-queue cached notify addresses.
     *
     * If provided by the caller, QueueNotifyAddrCache must point to an array of
     * QueueNotifyAddrCacheCount entries (typically num_queues). Entries are
     * populated on-demand by VirtioPciNotifyQueue().
     */
    volatile UINT16 **QueueNotifyAddrCache;
    USHORT QueueNotifyAddrCacheCount;

    /*
     * Selector-based common_cfg access must be serialized (contract §1.5.0).
     */
    KSPIN_LOCK CommonCfgLock;
} VIRTIO_PCI_DEVICE;

_Must_inspect_result_
NTSTATUS
VirtioPciModernMiniportInit(_Out_ VIRTIO_PCI_DEVICE *Dev,
                            _In_ PUCHAR Bar0Va,
                            _In_ ULONG Bar0Length,
                            _In_reads_bytes_(PciCfgLength) const UCHAR *PciCfg,
                            _In_ ULONG PciCfgLength);

/*
 * Virtio 1.0 status/reset helpers.
 */
/*
 * Reset the device by writing device_status=0 and waiting for the device to
 * report 0 on read-back (Virtio 1.0 spec).
 *
 * This helper is IRQL-aware:
 * - PASSIVE_LEVEL: sleeps/yields up to 1s total.
 * - > PASSIVE_LEVEL: busy-waits only briefly (to avoid long high-IRQL stalls)
 *   and returns even if the device does not reset.
 *
 * Callers that require a guaranteed reset must follow up with appropriate
 * failure/abort handling (e.g. VirtioPciFailDevice + teardown).
 */
VOID VirtioPciResetDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev);
VOID VirtioPciAddStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Bits);
UCHAR VirtioPciGetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev);
VOID VirtioPciSetStatus(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UCHAR Status);
VOID VirtioPciFailDevice(_Inout_ VIRTIO_PCI_DEVICE *Dev);

/*
 * 64-bit feature negotiation (selector pattern).
 */
UINT64 VirtioPciReadDeviceFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev);
VOID VirtioPciWriteDriverFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ UINT64 Features);

_Must_inspect_result_
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                           _In_ UINT64 Required,
                           _In_ UINT64 Wanted,
                           _Out_ UINT64 *NegotiatedOut);

/*
 * Device-specific config access (config_generation retry loop).
 */
_Must_inspect_result_
NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) VOID *Buffer,
                          _In_ ULONG Length);

/*
 * Queue programming + notify helpers (modern common_cfg + notify capability).
 */
USHORT VirtioPciGetNumQueues(_In_ VIRTIO_PCI_DEVICE *Dev);
USHORT VirtioPciGetQueueSize(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);

_Must_inspect_result_
NTSTATUS
VirtioPciSetupQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                    _In_ USHORT QueueIndex,
                    _In_ UINT64 DescPa,
                    _In_ UINT64 AvailPa,
                    _In_ UINT64 UsedPa);

VOID VirtioPciDisableQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);

_Must_inspect_result_
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16 **NotifyAddrOut);

VOID VirtioPciNotifyQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);

/*
 * Interrupt status (read-to-ack).
 */
UCHAR VirtioPciReadIsr(_In_ const VIRTIO_PCI_DEVICE *Dev);
