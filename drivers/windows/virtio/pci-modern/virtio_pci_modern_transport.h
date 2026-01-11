#ifndef VIRTIO_PCI_MODERN_TRANSPORT_H_
#define VIRTIO_PCI_MODERN_TRANSPORT_H_

/*
 * WDF-free virtio-pci "modern" transport for Aero Windows 7 virtio drivers.
 *
 * This module implements discovery via PCI vendor capabilities and MMIO access
 * to CommonCfg/Notify/ISR/DeviceCfg regions.
 *
 * It hard-enforces the AERO-W7-VIRTIO v1 transport contract (docs/):
 *   - PCI Revision ID == 0x01
 *   - BAR0 is MMIO (no legacy I/O port BAR0)
 *   - COMMON/NOTIFY/ISR/DEVICE vendor caps present and reference BAR0
 *   - Fixed BAR0 offsets: 0x0000 / 0x1000 / 0x2000 / 0x3000
 *   - notify_off_multiplier == 4
 *   - Feature negotiation always requires VIRTIO_F_VERSION_1 and never
 *     negotiates VIRTIO_F_RING_EVENT_IDX.
 */

#include "../common/virtio_osdep.h"

/* virtio_pci_common_cfg layout + virtio status bits */
#include "../../../win7/virtio/virtio-core/include/virtio_spec.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef UINT_PTR VIRTIO_PCI_MODERN_SPINLOCK_STATE;

typedef struct _VIRTIO_PCI_MODERN_OS_INTERFACE {
	void *Context;

	/* PCI config space access */
	UINT8 (*PciRead8)(void *Context, UINT16 Offset);
	UINT16 (*PciRead16)(void *Context, UINT16 Offset);
	UINT32 (*PciRead32)(void *Context, UINT16 Offset);

	/*
	 * Map a physical MMIO range (typically BAR0) and return a virtual pointer.
	 *
	 * The returned mapping must support volatile loads/stores.
	 */
	NTSTATUS (*MapMmio)(void *Context, UINT64 PhysicalAddress, UINT32 Length, volatile void **MappedVaOut);
	void (*UnmapMmio)(void *Context, volatile void *MappedVa, UINT32 Length);

	/* Busy-wait delay used for reset polling. */
	void (*StallUs)(void *Context, UINT32 Microseconds);

	/* Full memory barrier (SMP). Optional; falls back to VIRTIO_MB(). */
	void (*MemoryBarrier)(void *Context);

	/*
	 * Spinlock used to serialize selector-based CommonCfg accesses
	 * (device_feature_select/driver_feature_select/queue_select).
	 */
	void *(*SpinlockCreate)(void *Context);
	void (*SpinlockDestroy)(void *Context, void *Lock);
	void (*SpinlockAcquire)(void *Context, void *Lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE *StateOut);
	void (*SpinlockRelease)(void *Context, void *Lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE State);

	/* Optional diagnostics callback (formatting is caller-defined). */
	void (*Log)(void *Context, const char *Message);
} VIRTIO_PCI_MODERN_OS_INTERFACE;

typedef enum _VIRTIO_PCI_MODERN_TRANSPORT_MODE {
	VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT = 0,
	VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT = 1,
} VIRTIO_PCI_MODERN_TRANSPORT_MODE;

typedef enum _VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR {
	VIRTIO_PCI_MODERN_INIT_OK = 0,
	VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT,
	VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION,
	VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO,
	VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL,
	VIRTIO_PCI_MODERN_INIT_ERR_PCI_NO_CAP_LIST_STATUS,
	VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_PTR_UNALIGNED,
	VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID,
	VIRTIO_PCI_MODERN_INIT_ERR_CAP_PARSE_FAILED,
	VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_NOTIFY_MULTIPLIER_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_MAP_MMIO_FAILED,
	VIRTIO_PCI_MODERN_INIT_ERR_LOCK_CREATE_FAILED,
} VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR;

const char *VirtioPciModernTransportInitErrorStr(VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR Error);

typedef struct _VIRTIO_PCI_MODERN_TRANSPORT {
	const VIRTIO_PCI_MODERN_OS_INTERFACE *Os;
	void *OsContext;
	VIRTIO_PCI_MODERN_TRANSPORT_MODE Mode;

	/* Diagnostics for init failures. */
	VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR InitError;
	UINT32 CapParseResult; /* virtio_pci_cap_parse_result_t (kept opaque here) */

	UINT64 Bar0Pa;
	UINT32 Bar0Length;
	UINT32 Bar0MappedLength;
	volatile UINT8 *Bar0Va;

	volatile virtio_pci_common_cfg *CommonCfg;
	volatile UINT8 *NotifyBase;
	UINT32 NotifyOffMultiplier;
	UINT32 NotifyLength;
	volatile UINT8 *IsrStatus;
	UINT32 IsrLength;
	volatile UINT8 *DeviceCfg;
	UINT32 DeviceCfgLength;

	void *CommonCfgLock;
} VIRTIO_PCI_MODERN_TRANSPORT;

NTSTATUS VirtioPciModernTransportInit(VIRTIO_PCI_MODERN_TRANSPORT *Transport, const VIRTIO_PCI_MODERN_OS_INTERFACE *Os,
				     VIRTIO_PCI_MODERN_TRANSPORT_MODE Mode, UINT64 Bar0Pa, UINT32 Bar0Length);

VOID VirtioPciModernTransportUninit(VIRTIO_PCI_MODERN_TRANSPORT *Transport);

/* Virtio status helpers */
VOID VirtioPciModernTransportResetDevice(VIRTIO_PCI_MODERN_TRANSPORT *Transport);
UINT8 VirtioPciModernTransportGetStatus(VIRTIO_PCI_MODERN_TRANSPORT *Transport);
VOID VirtioPciModernTransportSetStatus(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT8 Status);
VOID VirtioPciModernTransportAddStatus(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT8 StatusBits);

/* Feature negotiation */
UINT64 VirtioPciModernTransportReadDeviceFeatures(VIRTIO_PCI_MODERN_TRANSPORT *Transport);
VOID VirtioPciModernTransportWriteDriverFeatures(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT64 Features);
NTSTATUS VirtioPciModernTransportNegotiateFeatures(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT64 Required, UINT64 Wanted,
						  UINT64 *NegotiatedOut);

/* Queue programming + notify */
UINT16 VirtioPciModernTransportGetNumQueues(VIRTIO_PCI_MODERN_TRANSPORT *Transport);
NTSTATUS VirtioPciModernTransportGetQueueSize(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex, UINT16 *SizeOut);
NTSTATUS VirtioPciModernTransportSetupQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex, UINT64 DescPa,
					    UINT64 AvailPa, UINT64 UsedPa);
VOID VirtioPciModernTransportDisableQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex);
NTSTATUS VirtioPciModernTransportNotifyQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex);

/* INTx helper (read-to-ack) */
UINT8 VirtioPciModernTransportReadIsrStatus(VIRTIO_PCI_MODERN_TRANSPORT *Transport);

/* Device-specific config access */
NTSTATUS VirtioPciModernTransportReadDeviceConfig(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT32 Offset, VOID *Buffer,
						  UINT32 Length);
NTSTATUS VirtioPciModernTransportWriteDeviceConfig(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT32 Offset, const VOID *Buffer,
						   UINT32 Length);

#ifdef __cplusplus
}
#endif

#endif /* VIRTIO_PCI_MODERN_TRANSPORT_H_ */
