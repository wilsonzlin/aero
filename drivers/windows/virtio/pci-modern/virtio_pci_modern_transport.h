#ifndef VIRTIO_PCI_MODERN_TRANSPORT_H_
#define VIRTIO_PCI_MODERN_TRANSPORT_H_

/*
 * WDF-free virtio-pci "modern" transport for Aero Windows 7 virtio drivers.
 *
 * This module implements discovery via PCI vendor capabilities and MMIO access
 * to CommonCfg/Notify/ISR/DeviceCfg regions.
 *
 * In STRICT mode, this module enforces the AERO-W7-VIRTIO v1 transport contract
 * (docs/):
 *   - PCI Vendor ID == 0x1AF4 (virtio vendor)
 *   - PCI Device ID in the modern-only ID space (>= 0x1040)
 *   - PCI Revision ID == 0x01
 *   - PCI Subsystem Vendor ID == 0x1AF4
 *   - PCI Interrupt Pin == 1 (INTA#)
 *   - BAR0 is 64-bit MMIO (no legacy I/O port BAR0)
 *   - BAR0 base address in PCI config space matches the caller-supplied BAR0 PA
 *   - COMMON/NOTIFY/ISR/DEVICE vendor caps present and reference BAR0
 *   - Fixed BAR0 offsets: 0x0000 / 0x1000 / 0x2000 / 0x3000
 *   - notify_off_multiplier == 4
 *   - Feature negotiation always requires VIRTIO_F_VERSION_1 and never
 *     negotiates VIRTIO_F_RING_EVENT_IDX.
 *   - devices MUST offer VIRTIO_F_RING_INDIRECT_DESC
 *
 * In COMPAT mode, the transport still requires the BAR0-only MMIO transport
 * shape (virtio vendor capabilities + BAR0 mapping) but relaxes some PCI identity
 * checks to support bring-up/testing with transitional/QEMU builds:
 *
 * QEMU compatibility:
 *   - Some QEMU configurations expose virtio devices with transitional PCI IDs
 *     (0x1000..0x103f) and/or report Revision ID 0x00 by default.
 *   - Drivers can opt into accepting transitional device IDs by defining:
 *       - AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID=1
 *   - Revision ID enforcement can be disabled by defining:
 *       - AERO_VIRTIO_PCI_ENFORCE_REVISION_ID=0
 */

#include "../common/virtio_osdep.h"

/* virtio_pci_common_cfg layout + virtio status bits */
#include "../../../win7/virtio/virtio-core/include/virtio_spec.h"

#ifndef AERO_VIRTIO_PCI_ENFORCE_REVISION_ID
#define AERO_VIRTIO_PCI_ENFORCE_REVISION_ID 1
#endif

#ifndef AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID
#define AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID 0
#endif

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Contract v1 strict-mode BAR0 size requirement.
 *
 * In STRICT mode the transport enforces the fixed BAR0 layout described by
 * docs/windows7-virtio-driver-contract.md, which requires a 0x4000-byte BAR0.
 */
#define VIRTIO_PCI_MODERN_TRANSPORT_BAR0_REQUIRED_LEN 0x4000u

/* Virtio spec sentinel for "no MSI-X vector assigned". */
#ifndef VIRTIO_PCI_MSI_NO_VECTOR
#define VIRTIO_PCI_MSI_NO_VECTOR ((UINT16)0xFFFFu)
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
	VIRTIO_PCI_MODERN_INIT_ERR_VENDOR_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_DEVICE_ID_NOT_MODERN,
	VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION,
	VIRTIO_PCI_MODERN_INIT_ERR_SUBSYSTEM_VENDOR_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_INTERRUPT_PIN_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_BAR0_ADDRESS_MISMATCH,
	VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO,
	VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO,
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
const char *VirtioPciModernTransportCapParseResultStr(UINT32 CapParseResult);

typedef struct _VIRTIO_PCI_MODERN_TRANSPORT {
	const VIRTIO_PCI_MODERN_OS_INTERFACE *Os;
	void *OsContext;
	VIRTIO_PCI_MODERN_TRANSPORT_MODE Mode;

	/* Diagnostics for init failures. */
	VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR InitError;
	UINT32 CapParseResult; /* virtio_pci_cap_parse_result_t (kept opaque here) */

	/* PCI identity (cached from config space). */
	UINT16 PciVendorId;
	UINT16 PciDeviceId;
	UINT8 PciRevisionId;
	UINT16 PciSubsystemVendorId;
	UINT16 PciSubsystemDeviceId;
	UINT8 PciInterruptPin;

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

	/*
	 * STRICT-mode safety latch: set once we observe queue_notify_off != queue index.
	 *
	 * In strict contract mode the transport uses the fast notify path that assumes
	 * queue_notify_off(q) == q. If the device violates this (or the MMIO mapping
	 * is inconsistent), treat the device as unsupported.
	 */
	BOOLEAN StrictNotifyOffMismatch;
} VIRTIO_PCI_MODERN_TRANSPORT;

NTSTATUS VirtioPciModernTransportInit(VIRTIO_PCI_MODERN_TRANSPORT *Transport, const VIRTIO_PCI_MODERN_OS_INTERFACE *Os,
				     VIRTIO_PCI_MODERN_TRANSPORT_MODE Mode, UINT64 Bar0Pa, UINT32 Bar0Length);

VOID VirtioPciModernTransportUninit(VIRTIO_PCI_MODERN_TRANSPORT *Transport);

/* Virtio status helpers */
/*
 * Reset the device by writing device_status=0 and waiting for the device to
 * acknowledge reset (device_status reads back 0).
 *
 * This helper is IRQL-aware in kernel-mode builds:
 * - PASSIVE_LEVEL: may sleep/yield while waiting (bounded ~1s).
 * - > PASSIVE_LEVEL: busy-waits only briefly (bounded) and returns even if the
 *   reset handshake does not complete.
 */
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
NTSTATUS VirtioPciModernTransportGetQueueNotifyOff(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex, UINT16 *NotifyOffOut);
NTSTATUS VirtioPciModernTransportSetupQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex, UINT64 DescPa,
					    UINT64 AvailPa, UINT64 UsedPa);
VOID VirtioPciModernTransportDisableQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex);
NTSTATUS VirtioPciModernTransportNotifyQueue(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex);

/*
 * MSI-X helpers.
 *
 * These helpers perform a read-back check after programming the device's
 * virtio_pci_common_cfg MSI-X vector fields:
 * - If Vector != VIRTIO_PCI_MSI_NO_VECTOR and the device reads back
 *   VIRTIO_PCI_MSI_NO_VECTOR or a different value, they fail with
 *   STATUS_IO_DEVICE_ERROR.
 * - If Vector == VIRTIO_PCI_MSI_NO_VECTOR, they accept a
 *   VIRTIO_PCI_MSI_NO_VECTOR read-back (disable).
 */
NTSTATUS VirtioPciModernTransportSetConfigMsixVector(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 Vector);
NTSTATUS VirtioPciModernTransportSetQueueMsixVector(VIRTIO_PCI_MODERN_TRANSPORT *Transport, UINT16 QueueIndex, UINT16 Vector);

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
