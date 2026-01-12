#pragma once

/*
 * virtio-core: reusable Virtio 1.0 PCI "modern" discovery + BAR mapping for
 * Windows 7 KMDF drivers.
 */

#include <ntddk.h>
#include <wdmguid.h>

#ifndef VIRTIO_CORE_USE_WDF
#define VIRTIO_CORE_USE_WDF 1
#endif

#if VIRTIO_CORE_USE_WDF
#include <wdf.h>
#endif

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

/*
 * Aero virtio-pci modern fixed MMIO layout enforcement.
 *
 * By default, virtio-core is permissive and accepts any valid virtio-pci modern
 * capability placement (e.g. QEMU's multi-BAR layout). Set
 * VIRTIO_CORE_ENFORCE_AERO_MMIO_LAYOUT=1 to require the Aero contract v1 fixed
 * BAR0 layout (see docs/windows7-virtio-driver-contract.md §1.4).
 */
#ifndef VIRTIO_CORE_ENFORCE_AERO_MMIO_LAYOUT
#define VIRTIO_CORE_ENFORCE_AERO_MMIO_LAYOUT 0
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
#if VIRTIO_CORE_USE_WDF
    WDFDEVICE WdfDevice;
#else
    PDEVICE_OBJECT DeviceObject;
    PDEVICE_OBJECT LowerDeviceObject;
#endif

    PCI_BUS_INTERFACE_STANDARD PciInterface;
    BOOLEAN PciInterfaceAcquired;

    /*
     * PCI identity fields (cached from config space during VirtioPciModernInit).
     *
     * These are used to enforce the AERO-W7-VIRTIO contract major version
     * (Revision ID) and to allow per-driver device ID checks before BAR mapping.
     */
    USHORT PciVendorId;
    USHORT PciDeviceId;
    UCHAR PciRevisionId;
    USHORT PciSubsystemVendorId;
    USHORT PciSubsystemId;

    VIRTIO_PCI_CAPS Caps;

    VIRTIO_PCI_BAR Bars[VIRTIO_PCI_MAX_BARS];

    /* Per-capability MMIO pointers (valid after VirtioPciModernMapBars). */
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
     */
    volatile UINT16 **QueueNotifyAddrCache;
    USHORT QueueNotifyAddrCacheCount;

    /*
     * The virtio_pci_common_cfg register block contains selector registers
     * (device_feature_select/driver_feature_select/queue_select) that act as global
     * selectors for the rest of the fields in the capability. Any multi-step access
     * that uses a selector must be serialized to avoid corrupting device state when
     * multiple threads (queues, DPCs, power callbacks, etc.) touch common_cfg
     * concurrently.
     */
#if VIRTIO_CORE_USE_WDF
    WDFSPINLOCK CommonCfgLock;
#else
    KSPIN_LOCK CommonCfgLock;
    KIRQL CommonCfgLockIrql;
#endif

#if DBG
    PKTHREAD CommonCfgLockOwner;
#endif
} VIRTIO_PCI_MODERN_DEVICE, *PVIRTIO_PCI_MODERN_DEVICE;

/*
 * Aero Windows 7 virtio contract (AERO-W7-VIRTIO) v1.0 gatekeeping.
 *
 * Contract v1 is identified by PCI Revision ID 0x01 and uses a fixed BAR0
 * MMIO layout. See: docs/windows7-virtio-driver-contract.md
 */
#define VIRTIO_PCI_AERO_CONTRACT_V1_REVISION_ID           0x01u
#define VIRTIO_PCI_AERO_CONTRACT_V1_BAR0_INDEX            0u
#define VIRTIO_PCI_AERO_CONTRACT_V1_BAR0_MIN_LEN          0x4000u
#define VIRTIO_PCI_AERO_CONTRACT_V1_COMMON_OFFSET         0x0000u
#define VIRTIO_PCI_AERO_CONTRACT_V1_COMMON_MIN_LEN        0x0100u
#define VIRTIO_PCI_AERO_CONTRACT_V1_NOTIFY_OFFSET         0x1000u
#define VIRTIO_PCI_AERO_CONTRACT_V1_NOTIFY_MIN_LEN        0x0100u
#define VIRTIO_PCI_AERO_CONTRACT_V1_ISR_OFFSET            0x2000u
#define VIRTIO_PCI_AERO_CONTRACT_V1_ISR_MIN_LEN           0x0020u
#define VIRTIO_PCI_AERO_CONTRACT_V1_DEVICE_OFFSET         0x3000u
#define VIRTIO_PCI_AERO_CONTRACT_V1_DEVICE_MIN_LEN        0x0100u
#define VIRTIO_PCI_AERO_CONTRACT_V1_NOTIFY_OFF_MULTIPLIER 4u

/*
 * Some helpers are specified in terms of a generic "VIRTIO_PCI_DEVICE".
 * In this codebase, that corresponds to the modern PCI transport device.
 */
typedef VIRTIO_PCI_MODERN_DEVICE VIRTIO_PCI_DEVICE;
typedef PVIRTIO_PCI_MODERN_DEVICE PVIRTIO_PCI_DEVICE;

/* Initialization / discovery */
#if VIRTIO_CORE_USE_WDF
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernInit(_In_ WDFDEVICE WdfDevice, _Out_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernMapBars(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                       _In_ WDFCMRESLIST ResourcesRaw,
                       _In_ WDFCMRESLIST ResourcesTranslated);
#else
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernInitWdm(_In_ PDEVICE_OBJECT DeviceObject,
                       _In_ PDEVICE_OBJECT LowerDeviceObject,
                       _Out_ PVIRTIO_PCI_MODERN_DEVICE Dev);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernMapBarsWdm(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                          _In_opt_ PCM_RESOURCE_LIST ResourcesRaw,
                          _In_opt_ PCM_RESOURCE_LIST ResourcesTranslated);
#endif

typedef enum _VIRTIO_PCI_AERO_CONTRACT_V1_LAYOUT_FAILURE {
    VirtioPciAeroContractV1LayoutFailureNone = 0,
    VirtioPciAeroContractV1LayoutFailureCommonCfg,
    VirtioPciAeroContractV1LayoutFailureNotifyCfg,
    VirtioPciAeroContractV1LayoutFailureIsrCfg,
    VirtioPciAeroContractV1LayoutFailureDeviceCfg,
    VirtioPciAeroContractV1LayoutFailureNotifyOffMultiplier,
    VirtioPciAeroContractV1LayoutFailureBar0Length,
} VIRTIO_PCI_AERO_CONTRACT_V1_LAYOUT_FAILURE;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernValidateAeroContractV1RevisionId(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev,
                                                _Out_opt_ UCHAR *RevisionIdOut);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernValidateAeroContractV1FixedLayout(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev,
                                                 _Out_opt_ VIRTIO_PCI_AERO_CONTRACT_V1_LAYOUT_FAILURE *FailureOut);

_IRQL_requires_max_(PASSIVE_LEVEL)
PCSTR
VirtioPciAeroContractV1LayoutFailureToString(_In_ VIRTIO_PCI_AERO_CONTRACT_V1_LAYOUT_FAILURE Failure);

/*
 * Enforces that the device matches the expected modern virtio-pci device IDs for
 * the caller (e.g. 0x1042 for virtio-blk).
 *
 * This MUST be called (by drivers) before mapping BARs / touching MMIO, so
 * unsupported devices are rejected early.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciModernEnforceDeviceIds(_In_ const VIRTIO_PCI_MODERN_DEVICE *Dev,
                                _In_reads_(AllowedDeviceIdCount) const USHORT *AllowedDeviceIds,
                                _In_ ULONG AllowedDeviceIdCount);

/*
 * Transport smoke-test helpers.
 *
 * These intentionally stop at FEATURES_OK (no DRIVER_OK / no virtqueues).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciModernResetDevice(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev);

/*
 * Virtio 1.0 status/reset helpers.
 */

/*
 * Resets the device by writing 0 to device_status and waiting for the device to
 * acknowledge reset (device_status reads back 0).
 *
 * Intended call site: PASSIVE_LEVEL (during init/teardown).
 *
 * Defensive behavior: if invoked at > PASSIVE_LEVEL, this helper will only
 * busy-wait for a small bounded budget and then return even if the device does
 * not complete the reset handshake (to avoid long stalls in DPC/DIRQL
 * contexts).
 *
 * Callers that require a guaranteed reset must follow up with appropriate
 * failure/abort handling (e.g. VirtioPciFailDevice + teardown).
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
VOID
VirtioPciResetDevice(_Inout_ PVIRTIO_PCI_DEVICE Dev);

/* ORs Bits into device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciAddStatus(_Inout_ PVIRTIO_PCI_DEVICE Dev, _In_ UCHAR Bits);

/* Reads device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
UCHAR
VirtioPciGetStatus(_Inout_ PVIRTIO_PCI_DEVICE Dev);

/* Sets the FAILED bit in device_status. */
_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciFailDevice(_Inout_ PVIRTIO_PCI_DEVICE Dev);

/*
 * Virtio 1.0 feature negotiation helper.
 */

/*
 * Negotiates 64-bit feature bits for a modern Virtio device.
 *
 * Sequence:
 *   - Reset
 *   - ACKNOWLEDGE + DRIVER
 *   - Read device features
 *   - negotiated = (device & Wanted) | Required
 *   - Always require VIRTIO_F_VERSION_1
 *   - Write negotiated features
 *   - Set FEATURES_OK
 *   - Re-read status to ensure FEATURES_OK was accepted
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciNegotiateFeatures(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                           _In_ UINT64 Required,
                           _In_ UINT64 Wanted,
                           _Out_ UINT64 *NegotiatedOut);

/*
 * Device-specific config access helpers.
 */

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciReadDeviceConfig(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                          _In_ ULONG Offset,
                          _Out_writes_bytes_(Length) PVOID Buffer,
                          _In_ ULONG Length);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciWriteDeviceConfig(_Inout_ PVIRTIO_PCI_DEVICE Dev,
                           _In_ ULONG Offset,
                           _In_reads_bytes_(Length) const VOID *Buffer,
                           _In_ ULONG Length);

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
VirtioPciReadQueueMsixVector(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciReadQueueMsixVectorLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueMsixVector(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                              _In_ USHORT QueueIndex,
                              _In_ USHORT Vector);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciWriteQueueMsixVectorLocked(_Inout_ PVIRTIO_PCI_MODERN_DEVICE Dev,
                                    _In_ USHORT QueueIndex,
                                    _In_ USHORT Vector);

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

/*
 * Virtqueue configuration + notification helpers (modern PCI transport).
 *
 * IRQL:
 *  - VirtioPciNotifyQueue() may be called at DISPATCH_LEVEL (e.g. from a DPC).
 *  - All helpers use the CommonCfg spin lock for selector serialization and are
 *    safe at <= DISPATCH_LEVEL.
 */

_IRQL_requires_max_(DISPATCH_LEVEL)
USHORT
VirtioPciGetNumQueues(_In_ VIRTIO_PCI_DEVICE *Dev);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueSize(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex, _Out_ USHORT *SizeOut);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciSetupQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                    _In_ USHORT QueueIndex,
                    _In_ ULONGLONG DescPa,
                    _In_ ULONGLONG AvailPa,
                    _In_ ULONGLONG UsedPa);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDisableQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
NTSTATUS
VirtioPciGetQueueNotifyAddress(_Inout_ VIRTIO_PCI_DEVICE *Dev,
                               _In_ USHORT QueueIndex,
                               _Out_ volatile UINT16 **NotifyAddrOut);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciNotifyQueue(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);

_IRQL_requires_max_(DISPATCH_LEVEL)
VOID
VirtioPciDumpQueueState(_Inout_ VIRTIO_PCI_DEVICE *Dev, _In_ USHORT QueueIndex);
