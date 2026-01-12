#include "virtio_pci_modern_transport.h"

#include <stddef.h>

#include "../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.h"

#ifndef AERO_VIRTIO_PCI_ENFORCE_REVISION_ID
#define AERO_VIRTIO_PCI_ENFORCE_REVISION_ID 1
#endif

#ifndef AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID
#define AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID 0
#endif

enum {
	AERO_W7_VIRTIO_PCI_VENDOR_ID = 0x1AF4,
	AERO_W7_VIRTIO_PCI_DEVICE_MODERN_BASE = 0x1040,
	AERO_W7_VIRTIO_PCI_DEVICE_TRANSITIONAL_BASE = 0x1000,

	AERO_W7_VIRTIO_PCI_REVISION = 0x01,

	AERO_W7_VIRTIO_BAR0_REQUIRED_LEN = VIRTIO_PCI_MODERN_TRANSPORT_BAR0_REQUIRED_LEN,

	AERO_W7_VIRTIO_COMMON_OFF = 0x0000,
	AERO_W7_VIRTIO_COMMON_MIN_LEN = 0x0100,

	AERO_W7_VIRTIO_NOTIFY_OFF = 0x1000,
	AERO_W7_VIRTIO_NOTIFY_MIN_LEN = 0x0100,

	AERO_W7_VIRTIO_ISR_OFF = 0x2000,
	AERO_W7_VIRTIO_ISR_MIN_LEN = 0x0020,

	AERO_W7_VIRTIO_DEVICE_OFF = 0x3000,
	AERO_W7_VIRTIO_DEVICE_MIN_LEN = 0x0100,

	AERO_W7_VIRTIO_NOTIFY_MULTIPLIER = 4,

	/* Bounded reset poll (virtio status reset handshake). */
	VIRTIO_PCI_RESET_TIMEOUT_US = 1000000u,
	VIRTIO_PCI_RESET_POLL_DELAY_US = 1000u,
	/*
	 * When reset is requested at elevated IRQL, cap the total busy-wait budget.
	 * Long stalls in DPC/DIRQL contexts can severely impact system responsiveness.
	 */
	VIRTIO_PCI_RESET_HIGH_IRQL_TIMEOUT_US = 10000u,
	VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US = 100u,
	VIRTIO_PCI_CONFIG_MAX_READ_RETRIES = 10u,

	/* Standard PCI config offsets */
	PCI_CFG_VENDOR_OFF = 0x00,
	PCI_CFG_DEVICE_OFF = 0x02,
	PCI_CFG_STATUS_OFF = 0x06,
	PCI_CFG_REVISION_OFF = 0x08,
	PCI_CFG_SUBSYSTEM_VENDOR_OFF = 0x2C,
	PCI_CFG_SUBSYSTEM_DEVICE_OFF = 0x2E,
	PCI_CFG_BAR0_OFF = 0x10,
	PCI_CFG_CAP_PTR_OFF = 0x34,
	PCI_CFG_INTERRUPT_PIN_OFF = 0x3D,
};

static __inline VOID VirtioPciModernLog(VIRTIO_PCI_MODERN_TRANSPORT *t, const char *msg)
{
	if (t != NULL && t->Os != NULL && t->Os->Log != NULL) {
		t->Os->Log(t->OsContext, msg);
	}
}

static __inline VOID VirtioPciModernMb(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	if (t != NULL && t->Os != NULL && t->Os->MemoryBarrier != NULL) {
		t->Os->MemoryBarrier(t->OsContext);
	} else {
		VIRTIO_MB();
	}
}

static __inline VOID VirtioPciModernLock(VIRTIO_PCI_MODERN_TRANSPORT *t, VIRTIO_PCI_MODERN_SPINLOCK_STATE *state)
{
	if (t == NULL || t->Os == NULL || t->Os->SpinlockAcquire == NULL || t->CommonCfgLock == NULL) {
		*state = 0;
		return;
	}
	t->Os->SpinlockAcquire(t->OsContext, t->CommonCfgLock, state);
}

static __inline VOID VirtioPciModernUnlock(VIRTIO_PCI_MODERN_TRANSPORT *t, VIRTIO_PCI_MODERN_SPINLOCK_STATE state)
{
	if (t == NULL || t->Os == NULL || t->Os->SpinlockRelease == NULL || t->CommonCfgLock == NULL) {
		return;
	}
	t->Os->SpinlockRelease(t->OsContext, t->CommonCfgLock, state);
}

static __inline UINT16 VirtioPciModernReadLe16(const UINT8 *p)
{
	return (UINT16)p[0] | ((UINT16)p[1] << 8);
}

static __inline VOID VirtioPciModernWriteLe32(UINT8 *p, UINT32 v)
{
	p[0] = (UINT8)(v & 0xffu);
	p[1] = (UINT8)((v >> 8) & 0xffu);
	p[2] = (UINT8)((v >> 16) & 0xffu);
	p[3] = (UINT8)((v >> 24) & 0xffu);
}

static UINT32 VirtioPciModernClampCapLength(UINT32 mapped_len, UINT32 cap_offset, UINT32 cap_length)
{
	UINT32 max_len;

	if (cap_offset >= mapped_len) {
		return 0;
	}

	max_len = mapped_len - cap_offset;
	if (cap_length > max_len) {
		cap_length = max_len;
	}

	return cap_length;
}

static VOID VirtioPciModernReadCfgSpace256(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT8 cfg_space[256])
{
	UINT16 off;

	for (off = 0; off < 256; off = (UINT16)(off + 4)) {
		UINT32 v = t->Os->PciRead32(t->OsContext, off);
		VirtioPciModernWriteLe32(&cfg_space[off], v);
	}
}

static BOOLEAN VirtioPciModernValidateCapListAlignment(VIRTIO_PCI_MODERN_TRANSPORT *t, const UINT8 cfg_space[256])
{
	UINT8 visited[256];
	UINT8 current;
	UINT32 caps_seen;
	UINT16 i;

	(void)t;

	for (i = 0; i < (UINT16)sizeof(visited); ++i) {
		visited[i] = 0;
	}

	current = cfg_space[PCI_CFG_CAP_PTR_OFF];
	if (current == 0) {
		return FALSE;
	}
	if ((current & 0x03u) != 0) {
		return FALSE;
	}
	if (current < 0x40u || current >= 256u) {
		return FALSE;
	}

	caps_seen = 0;
	while (current != 0) {
		UINT8 cap_id;
		UINT8 cap_next;

		if ((current & 0x03u) != 0) {
			return FALSE;
		}
		if (current < 0x40u || current >= 256u) {
			return FALSE;
		}
		if (visited[current] != 0) {
			return FALSE;
		}
		visited[current] = 1;

		/* Need at least cap_id + cap_next */
		if ((UINT32)current + 2u > 256u) {
			return FALSE;
		}

		cap_id = cfg_space[current + 0];
		(void)cap_id;

		cap_next = cfg_space[current + 1];
		if ((cap_next & 0x03u) != 0) {
			return FALSE;
		}
		if (cap_next != 0 && (cap_next < 0x40u || cap_next >= 256u)) {
			return FALSE;
		}

		current = cap_next;

		/* Hard bound for safety against malicious config space. */
		if (++caps_seen > 64u) {
			return FALSE;
		}
	}

	return TRUE;
}

static NTSTATUS VirtioPciModernValidateContractCaps(VIRTIO_PCI_MODERN_TRANSPORT *t, const virtio_pci_parsed_caps_t *caps)
{
	/* All regions must fit within BAR0. (The cap parser validates the cfg list, not BAR bounds.) */
	{
		UINT64 end_common = (UINT64)caps->common_cfg.offset + (UINT64)caps->common_cfg.length;
		UINT64 end_notify = (UINT64)caps->notify_cfg.offset + (UINT64)caps->notify_cfg.length;
		UINT64 end_isr = (UINT64)caps->isr_cfg.offset + (UINT64)caps->isr_cfg.length;
		UINT64 end_device = (UINT64)caps->device_cfg.offset + (UINT64)caps->device_cfg.length;

		if (end_common > (UINT64)t->Bar0Length || end_notify > (UINT64)t->Bar0Length || end_isr > (UINT64)t->Bar0Length ||
		    end_device > (UINT64)t->Bar0Length) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL;
			return STATUS_NOT_SUPPORTED;
		}
	}

	if (caps->notify_off_multiplier != AERO_W7_VIRTIO_NOTIFY_MULTIPLIER) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_NOTIFY_MULTIPLIER_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}

	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
		/* Fixed offsets enforced by AERO-W7-VIRTIO v1 */
		if (caps->common_cfg.bar != 0 || caps->common_cfg.offset != AERO_W7_VIRTIO_COMMON_OFF ||
		    caps->common_cfg.length < AERO_W7_VIRTIO_COMMON_MIN_LEN) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
			return STATUS_NOT_SUPPORTED;
		}
		if (caps->notify_cfg.bar != 0 || caps->notify_cfg.offset != AERO_W7_VIRTIO_NOTIFY_OFF ||
		    caps->notify_cfg.length < AERO_W7_VIRTIO_NOTIFY_MIN_LEN) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
			return STATUS_NOT_SUPPORTED;
		}
		if (caps->isr_cfg.bar != 0 || caps->isr_cfg.offset != AERO_W7_VIRTIO_ISR_OFF ||
		    caps->isr_cfg.length < AERO_W7_VIRTIO_ISR_MIN_LEN) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
			return STATUS_NOT_SUPPORTED;
		}
		if (caps->device_cfg.bar != 0 || caps->device_cfg.offset != AERO_W7_VIRTIO_DEVICE_OFF ||
		    caps->device_cfg.length < AERO_W7_VIRTIO_DEVICE_MIN_LEN) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
			return STATUS_NOT_SUPPORTED;
		}
	}

	/*
	 * In COMPAT mode we still require the regions to be present and to fit
	 * within BAR0, but don't require the exact fixed offsets.
	 */
	if (caps->common_cfg.bar != 0 || caps->common_cfg.length < AERO_W7_VIRTIO_COMMON_MIN_LEN) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}
	if (caps->notify_cfg.bar != 0 || caps->notify_cfg.length < AERO_W7_VIRTIO_NOTIFY_MIN_LEN) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}
	if (caps->isr_cfg.bar != 0 || caps->isr_cfg.length < AERO_W7_VIRTIO_ISR_MIN_LEN) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}
	if (caps->device_cfg.bar != 0 || caps->device_cfg.length < AERO_W7_VIRTIO_DEVICE_MIN_LEN) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}

	/* All regions must be within BAR0 length. */
	{
		UINT64 end_common = (UINT64)caps->common_cfg.offset + (UINT64)AERO_W7_VIRTIO_COMMON_MIN_LEN;
		UINT64 end_notify = (UINT64)caps->notify_cfg.offset + (UINT64)AERO_W7_VIRTIO_NOTIFY_MIN_LEN;
		UINT64 end_isr = (UINT64)caps->isr_cfg.offset + (UINT64)AERO_W7_VIRTIO_ISR_MIN_LEN;
		UINT64 end_device = (UINT64)caps->device_cfg.offset + (UINT64)AERO_W7_VIRTIO_DEVICE_MIN_LEN;
		UINT64 max_end = end_common;

		if (end_notify > max_end) {
			max_end = end_notify;
		}
		if (end_isr > max_end) {
			max_end = end_isr;
		}
		if (end_device > max_end) {
			max_end = end_device;
		}

		if (max_end > (UINT64)t->Bar0Length) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL;
			return STATUS_BUFFER_TOO_SMALL;
		}
	}

	return STATUS_SUCCESS;
}

const char *VirtioPciModernTransportInitErrorStr(VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR err)
{
	switch (err) {
		case VIRTIO_PCI_MODERN_INIT_OK:
			return "OK";
		case VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT:
			return "BAD_ARGUMENT";
		case VIRTIO_PCI_MODERN_INIT_ERR_VENDOR_MISMATCH:
			return "VENDOR_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_DEVICE_ID_NOT_MODERN:
			return "DEVICE_ID_NOT_MODERN";
		case VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION:
			return "UNSUPPORTED_REVISION";
		case VIRTIO_PCI_MODERN_INIT_ERR_SUBSYSTEM_VENDOR_MISMATCH:
			return "SUBSYSTEM_VENDOR_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_INTERRUPT_PIN_MISMATCH:
			return "INTERRUPT_PIN_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_BAR0_ADDRESS_MISMATCH:
			return "BAR0_ADDRESS_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO:
			return "BAR0_NOT_MMIO";
		case VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO:
			return "BAR0_NOT_64BIT_MMIO";
		case VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL:
			return "BAR0_TOO_SMALL";
		case VIRTIO_PCI_MODERN_INIT_ERR_PCI_NO_CAP_LIST_STATUS:
			return "PCI_NO_CAP_LIST_STATUS";
		case VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_PTR_UNALIGNED:
			return "PCI_CAP_PTR_UNALIGNED";
		case VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID:
			return "PCI_CAP_LIST_INVALID";
		case VIRTIO_PCI_MODERN_INIT_ERR_CAP_PARSE_FAILED:
			return "CAP_PARSE_FAILED";
		case VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH:
			return "CAP_LAYOUT_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_NOTIFY_MULTIPLIER_MISMATCH:
			return "NOTIFY_MULTIPLIER_MISMATCH";
		case VIRTIO_PCI_MODERN_INIT_ERR_MAP_MMIO_FAILED:
			return "MAP_MMIO_FAILED";
		case VIRTIO_PCI_MODERN_INIT_ERR_LOCK_CREATE_FAILED:
			return "LOCK_CREATE_FAILED";
		default:
			return "UNKNOWN";
	}
}

const char *VirtioPciModernTransportCapParseResultStr(UINT32 cap_parse_result)
{
	return virtio_pci_cap_parse_result_str((virtio_pci_cap_parse_result_t)cap_parse_result);
}

NTSTATUS VirtioPciModernTransportInit(VIRTIO_PCI_MODERN_TRANSPORT *t, const VIRTIO_PCI_MODERN_OS_INTERFACE *os,
				     VIRTIO_PCI_MODERN_TRANSPORT_MODE mode, UINT64 bar0_pa, UINT32 bar0_len)
{
	UINT8 cfg_space[256];
	UINT16 vendor;
	UINT16 device;
	UINT8 revision;
	UINT16 subsystem_vendor;
	UINT16 subsystem_device;
	UINT8 interrupt_pin;
	UINT16 status;
	UINT32 bar0_low;
	UINT32 bar0_high;
	UINT64 bar0_cfg_base;
	virtio_pci_parsed_caps_t caps;
	virtio_pci_cap_parse_result_t cap_res;
	UINT64 bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
	NTSTATUS st;
	UINT16 cfg_status_le;

	if (t == NULL || os == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	VirtioZeroMemory(t, sizeof(*t));
	t->Os = os;
	t->OsContext = os->Context;
	t->Mode = mode;
	t->Bar0Pa = bar0_pa;
	t->Bar0Length = bar0_len;
	t->Bar0MappedLength = 0;
	t->InitError = VIRTIO_PCI_MODERN_INIT_OK;

	if (os->PciRead8 == NULL || os->PciRead16 == NULL || os->PciRead32 == NULL || os->MapMmio == NULL ||
	    os->UnmapMmio == NULL || os->SpinlockCreate == NULL || os->SpinlockDestroy == NULL ||
	    os->SpinlockAcquire == NULL || os->SpinlockRelease == NULL || os->StallUs == NULL) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT;
		return STATUS_INVALID_PARAMETER;
	}

	if (mode != VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && mode != VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT;
		return STATUS_INVALID_PARAMETER;
	}

	if (bar0_pa == 0 || bar0_len == 0) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT;
		return STATUS_INVALID_PARAMETER;
	}

	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && bar0_len < AERO_W7_VIRTIO_BAR0_REQUIRED_LEN) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL;
		return STATUS_BUFFER_TOO_SMALL;
	}

	/* Enforce AERO-W7-VIRTIO v1 PCI identity (vendor/device/revision). */
	vendor = os->PciRead16(t->OsContext, PCI_CFG_VENDOR_OFF);
	device = os->PciRead16(t->OsContext, PCI_CFG_DEVICE_OFF);
	revision = os->PciRead8(t->OsContext, PCI_CFG_REVISION_OFF);
	subsystem_vendor = os->PciRead16(t->OsContext, PCI_CFG_SUBSYSTEM_VENDOR_OFF);
	subsystem_device = os->PciRead16(t->OsContext, PCI_CFG_SUBSYSTEM_DEVICE_OFF);
	interrupt_pin = os->PciRead8(t->OsContext, PCI_CFG_INTERRUPT_PIN_OFF);

	t->PciVendorId = vendor;
	t->PciDeviceId = device;
	t->PciRevisionId = revision;
	t->PciSubsystemVendorId = subsystem_vendor;
	t->PciSubsystemDeviceId = subsystem_device;
	t->PciInterruptPin = interrupt_pin;

	if (vendor != (UINT16)AERO_W7_VIRTIO_PCI_VENDOR_ID) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_VENDOR_MISMATCH;
		VirtioPciModernLog(t, "virtio_pci_modern_transport: unsupported PCI vendor id");
		return STATUS_NOT_SUPPORTED;
	}

	if (device < (UINT16)AERO_W7_VIRTIO_PCI_DEVICE_MODERN_BASE) {
#if AERO_VIRTIO_PCI_ALLOW_TRANSITIONAL_DEVICE_ID
		/*
		 * QEMU compatibility: allow virtio-pci transitional device IDs when the
		 * caller explicitly opts in (typically for driver bring-up on stock QEMU
		 * defaults).
		 *
		 * Transitional IDs live in 0x1000..0x103f. Even when a device advertises a
		 * transitional ID, drivers can still use the modern capability transport.
		 *
		 * Treat transitional IDs as COMPAT mode regardless of the requested mode:
		 * they are outside the strict AERO-W7-VIRTIO contract.
		 */
		if (device < (UINT16)AERO_W7_VIRTIO_PCI_DEVICE_TRANSITIONAL_BASE) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_DEVICE_ID_NOT_MODERN;
			VirtioPciModernLog(t, "virtio_pci_modern_transport: PCI device id not in virtio transitional range");
			return STATUS_NOT_SUPPORTED;
		}
		if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
			t->Mode = VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT;
		}
#else
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_DEVICE_ID_NOT_MODERN;
		VirtioPciModernLog(t, "virtio_pci_modern_transport: PCI device id not in modern-only range");
		return STATUS_NOT_SUPPORTED;
#endif
	}

	/* Enforce AERO-W7-VIRTIO v1 revision ID unless the caller opts out. */
#if AERO_VIRTIO_PCI_ENFORCE_REVISION_ID
	if (revision != AERO_W7_VIRTIO_PCI_REVISION) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION;
		return STATUS_NOT_SUPPORTED;
	}
#else
	if (revision != AERO_W7_VIRTIO_PCI_REVISION && t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
		t->Mode = VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT;
	}
#endif

	/* Subsystem vendor ID is fixed by contract v1. */
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && subsystem_vendor != (UINT16)AERO_W7_VIRTIO_PCI_VENDOR_ID) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_SUBSYSTEM_VENDOR_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}

	/* Interrupt pin is fixed by contract v1 (INTA#). */
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && interrupt_pin != 0x01) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_INTERRUPT_PIN_MISMATCH;
		return STATUS_NOT_SUPPORTED;
	}

	/* BAR0 must be memory (MMIO). */
	bar0_low = os->PciRead32(t->OsContext, PCI_CFG_BAR0_OFF);
	if ((bar0_low & 0x01u) != 0) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO;
		return STATUS_NOT_SUPPORTED;
	}
	/*
	 * AERO-W7-VIRTIO v1 requires BAR0 to be a 64-bit MMIO BAR.
	 *
	 * PCI BAR memory type encoding:
	 *   bits [2:1] == 0b10 => 64-bit address
	 */
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && (bar0_low & 0x06u) != 0x04u) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO;
		return STATUS_NOT_SUPPORTED;
	}

	/*
	 * BAR0 base address in PCI config space must match the BAR0 physical address
	 * supplied by the caller.
	 *
	 * This catches driver resource discovery bugs where a different MMIO range
	 * is mapped than the one the device is programmed to use.
	 */
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
		bar0_high = os->PciRead32(t->OsContext, (UINT16)(PCI_CFG_BAR0_OFF + 4));
		bar0_cfg_base = ((UINT64)bar0_high << 32) | (UINT64)(bar0_low & ~0x0Fu);
		if (bar0_cfg_base != bar0_pa) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_BAR0_ADDRESS_MISMATCH;
			return STATUS_NOT_SUPPORTED;
		}
	}

	/* PCI capabilities list must be present and aligned (Status bit 4). */
	status = os->PciRead16(t->OsContext, PCI_CFG_STATUS_OFF);
	if ((status & (1u << 4)) == 0) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_PCI_NO_CAP_LIST_STATUS;
		return STATUS_NOT_SUPPORTED;
	}

	VirtioPciModernReadCfgSpace256(t, cfg_space);

	/* Reject unaligned cap list pointers (contract requirement). */
	if ((cfg_space[PCI_CFG_CAP_PTR_OFF] & 0x03u) != 0) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_PTR_UNALIGNED;
		return STATUS_NOT_SUPPORTED;
	}

	/*
	 * Validate the capability list encoding strictly before parsing.
	 * (The portable parser is tolerant and masks pointer alignment.)
	 */
	if (!VirtioPciModernValidateCapListAlignment(t, cfg_space)) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID;
		return STATUS_NOT_SUPPORTED;
	}

	/* Re-check status bit from the buffered cfg space for consistency. */
	cfg_status_le = VirtioPciModernReadLe16(&cfg_space[PCI_CFG_STATUS_OFF]);
	if ((cfg_status_le & (1u << 4)) == 0) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_PCI_NO_CAP_LIST_STATUS;
		return STATUS_NOT_SUPPORTED;
	}

	/* Parse virtio vendor caps; only BAR0 is allowed by contract. */
	VirtioZeroMemory(bar_addrs, sizeof(bar_addrs));
	bar_addrs[0] = bar0_pa;

	cap_res = virtio_pci_cap_parse(cfg_space, sizeof(cfg_space), bar_addrs, &caps);
	t->CapParseResult = (UINT32)cap_res;
	if (cap_res != VIRTIO_PCI_CAP_PARSE_OK) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_CAP_PARSE_FAILED;
		return STATUS_NOT_SUPPORTED;
	}

	st = VirtioPciModernValidateContractCaps(t, &caps);
	if (!NT_SUCCESS(st)) {
		return st;
	}

	/* Map BAR0. */
	{
		volatile void *va = NULL;
		UINT32 map_len = bar0_len;
		if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && map_len > AERO_W7_VIRTIO_BAR0_REQUIRED_LEN) {
			map_len = AERO_W7_VIRTIO_BAR0_REQUIRED_LEN;
		}

		st = os->MapMmio(t->OsContext, bar0_pa, map_len, &va);
		if (!NT_SUCCESS(st) || va == NULL) {
			t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_MAP_MMIO_FAILED;
			return NT_SUCCESS(st) ? STATUS_INSUFFICIENT_RESOURCES : st;
		}

		t->Bar0Va = (volatile UINT8 *)va;
		t->Bar0MappedLength = map_len;
	}

	/* Compute per-capability pointers. */
	t->CommonCfg = (volatile virtio_pci_common_cfg *)(t->Bar0Va + caps.common_cfg.offset);
	t->NotifyBase = (volatile UINT8 *)(t->Bar0Va + caps.notify_cfg.offset);
	t->NotifyOffMultiplier = caps.notify_off_multiplier;
	t->NotifyLength = VirtioPciModernClampCapLength(t->Bar0MappedLength, caps.notify_cfg.offset, caps.notify_cfg.length);
	t->IsrStatus = (volatile UINT8 *)(t->Bar0Va + caps.isr_cfg.offset);
	t->IsrLength = VirtioPciModernClampCapLength(t->Bar0MappedLength, caps.isr_cfg.offset, caps.isr_cfg.length);
	t->DeviceCfg = (volatile UINT8 *)(t->Bar0Va + caps.device_cfg.offset);
	t->DeviceCfgLength = VirtioPciModernClampCapLength(t->Bar0MappedLength, caps.device_cfg.offset, caps.device_cfg.length);

	/* Create the CommonCfg selector lock. */
	t->CommonCfgLock = os->SpinlockCreate(t->OsContext);
	if (t->CommonCfgLock == NULL) {
		t->InitError = VIRTIO_PCI_MODERN_INIT_ERR_LOCK_CREATE_FAILED;
		os->UnmapMmio(t->OsContext, t->Bar0Va, t->Bar0MappedLength);
		t->Bar0Va = NULL;
		t->Bar0MappedLength = 0;
		return STATUS_INSUFFICIENT_RESOURCES;
	}

	VirtioPciModernLog(t, "virtio_pci_modern_transport: init OK");
	return STATUS_SUCCESS;
}

VOID VirtioPciModernTransportUninit(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	if (t == NULL || t->Os == NULL) {
		return;
	}

	if (t->CommonCfgLock != NULL && t->Os->SpinlockDestroy != NULL) {
		t->Os->SpinlockDestroy(t->OsContext, t->CommonCfgLock);
		t->CommonCfgLock = NULL;
	}

	if (t->Bar0Va != NULL && t->Os->UnmapMmio != NULL) {
		t->Os->UnmapMmio(t->OsContext, t->Bar0Va, t->Bar0MappedLength);
		t->Bar0Va = NULL;
		t->Bar0MappedLength = 0;
	}

	t->CommonCfg = NULL;
	t->NotifyBase = NULL;
	t->IsrStatus = NULL;
	t->DeviceCfg = NULL;
}

VOID VirtioPciModernTransportResetDevice(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	UINT32 waited_us;

	if (t == NULL || t->CommonCfg == NULL || t->Os == NULL) {
		return;
	}

	/* Writing 0 resets the device. */
	t->CommonCfg->device_status = 0;
	VirtioPciModernMb(t);

	/* Immediate readback fast-path. */
	if (t->CommonCfg->device_status == 0) {
		VirtioPciModernMb(t);
		return;
	}

#if VIRTIO_OSDEP_KERNEL_MODE
	/*
	 * Reset may be invoked from a variety of driver stacks. Avoid spending up to
	 * 1 second busy-waiting at DISPATCH/DIRQL.
	 */
	{
		KIRQL irql;
		irql = KeGetCurrentIrql();

		if (irql == PASSIVE_LEVEL) {
			const ULONGLONG timeout100ns = (ULONGLONG)VIRTIO_PCI_RESET_TIMEOUT_US * 10ull;
			const ULONGLONG pollDelay100ns = (ULONGLONG)VIRTIO_PCI_RESET_POLL_DELAY_US * 10ull;
			const ULONGLONG start100ns = KeQueryInterruptTime();
			const ULONGLONG deadline100ns = start100ns + timeout100ns;

			for (;;) {
				ULONGLONG now100ns;
				ULONGLONG remaining100ns;
				LARGE_INTEGER delay;

				if (t->CommonCfg->device_status == 0) {
					VirtioPciModernMb(t);
					return;
				}

				now100ns = KeQueryInterruptTime();
				if (now100ns >= deadline100ns) {
					break;
				}

				remaining100ns = deadline100ns - now100ns;
				if (remaining100ns > pollDelay100ns) {
					remaining100ns = pollDelay100ns;
				}

				delay.QuadPart = -((LONGLONG)remaining100ns);
				(void)KeDelayExecutionThread(KernelMode, FALSE, &delay);
			}

			VirtioPciModernLog(t, "virtio_pci_modern_transport: reset timeout");
			return;
		}

		/*
		 * Elevated IRQL: only poll for a small budget, then give up.
		 */
		for (waited_us = 0; waited_us < VIRTIO_PCI_RESET_HIGH_IRQL_TIMEOUT_US;
		     waited_us += VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US) {
			if (t->CommonCfg->device_status == 0) {
				VirtioPciModernMb(t);
				return;
			}
			t->Os->StallUs(t->OsContext, VIRTIO_PCI_RESET_HIGH_IRQL_POLL_DELAY_US);
		}

		VirtioPciModernLog(t, "virtio_pci_modern_transport: reset timeout (high IRQL)");
		return;
	}
#else
	/*
	 * Poll until the device acknowledges reset (bounded).
	 *
	 * Non-kernel builds (host-side tests) do not have IRQL or thread-wait APIs;
	 * keep the original stall-based loop.
	 */
	for (waited_us = 0; waited_us < VIRTIO_PCI_RESET_TIMEOUT_US; waited_us += VIRTIO_PCI_RESET_POLL_DELAY_US) {
		if (t->CommonCfg->device_status == 0) {
			return;
		}
		t->Os->StallUs(t->OsContext, VIRTIO_PCI_RESET_POLL_DELAY_US);
	}
#endif
}

UINT8 VirtioPciModernTransportGetStatus(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	if (t == NULL || t->CommonCfg == NULL) {
		return 0;
	}
	return t->CommonCfg->device_status;
}

VOID VirtioPciModernTransportSetStatus(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT8 status)
{
	if (t == NULL || t->CommonCfg == NULL) {
		return;
	}
	t->CommonCfg->device_status = status;
	VirtioPciModernMb(t);
}

VOID VirtioPciModernTransportAddStatus(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT8 bits)
{
	UINT8 status;

	if (t == NULL) {
		return;
	}
	status = VirtioPciModernTransportGetStatus(t);
	status |= bits;
	VirtioPciModernTransportSetStatus(t, status);
}

static UINT64 VirtioPciModernReadDeviceFeaturesLocked(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	UINT64 lo;
	UINT64 hi;

	t->CommonCfg->device_feature_select = 0;
	VirtioPciModernMb(t);
	lo = (UINT64)t->CommonCfg->device_feature;

	t->CommonCfg->device_feature_select = 1;
	VirtioPciModernMb(t);
	hi = (UINT64)t->CommonCfg->device_feature;

	return lo | (hi << 32);
}

UINT64 VirtioPciModernTransportReadDeviceFeatures(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT64 features;

	if (t == NULL || t->CommonCfg == NULL) {
		return 0;
	}

	VirtioPciModernLock(t, &state);
	features = VirtioPciModernReadDeviceFeaturesLocked(t);
	VirtioPciModernUnlock(t, state);
	return features;
}

static VOID VirtioPciModernWriteDriverFeaturesLocked(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT64 features)
{
	t->CommonCfg->driver_feature_select = 0;
	VirtioPciModernMb(t);
	t->CommonCfg->driver_feature = (UINT32)features;
	VirtioPciModernMb(t);

	t->CommonCfg->driver_feature_select = 1;
	VirtioPciModernMb(t);
	t->CommonCfg->driver_feature = (UINT32)(features >> 32);
	VirtioPciModernMb(t);
}

VOID VirtioPciModernTransportWriteDriverFeatures(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT64 features)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;

	if (t == NULL || t->CommonCfg == NULL) {
		return;
	}

	VirtioPciModernLock(t, &state);
	VirtioPciModernWriteDriverFeaturesLocked(t, features);
	VirtioPciModernUnlock(t, state);
}

NTSTATUS VirtioPciModernTransportNegotiateFeatures(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT64 required, UINT64 wanted,
						  UINT64 *negotiated_out)
{
	UINT64 device_features;
	UINT64 negotiated;
	UINT64 forbidden;
	enum {
		VIRTIO_F_RING_INDIRECT_DESC_BIT = 28,
		VIRTIO_F_RING_EVENT_IDX_BIT = 29,
		VIRTIO_F_RING_PACKED_BIT = 34,
	};
	UINT64 indirect_desc;

	if (t == NULL || t->CommonCfg == NULL || negotiated_out == NULL) {
		return STATUS_INVALID_PARAMETER;
	}
	*negotiated_out = 0;

	/* Contract requirement: split ring only; never negotiate EVENT_IDX or PACKED ring. */
	forbidden = (UINT64)1u << VIRTIO_F_RING_EVENT_IDX_BIT;
	forbidden |= (UINT64)1u << VIRTIO_F_RING_PACKED_BIT;

	/*
	 * Reject callers that attempt to require forbidden ring features.
	 *
	 * These features are never negotiated by the AERO-W7-VIRTIO v1 transport; dropping them
	 * silently from the required set can mask driver bugs.
	 */
	if ((required & forbidden) != 0) {
		return STATUS_INVALID_PARAMETER;
	}

	/* Contract requirement: modern device (VERSION_1). */
	required |= VIRTIO_F_VERSION_1;

	wanted &= ~forbidden;
	indirect_desc = (UINT64)1u << VIRTIO_F_RING_INDIRECT_DESC_BIT;

	VirtioPciModernTransportResetDevice(t);
	VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

	device_features = VirtioPciModernTransportReadDeviceFeatures(t);
	if ((device_features & VIRTIO_F_VERSION_1) == 0) {
		VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_FAILED);
		return STATUS_NOT_SUPPORTED;
	}
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && (device_features & indirect_desc) == 0) {
		/* Contract v1 devices must offer INDIRECT_DESC. */
		VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_FAILED);
		return STATUS_NOT_SUPPORTED;
	}
	/*
	 * Never negotiate EVENT_IDX or PACKED ring.
	 *
	 * Note: Some virtio-pci implementations (including QEMU) advertise these features even
	 * when the driver chooses not to negotiate them. Since the Windows 7 drivers in this
	 * repo operate correctly without EVENT_IDX/PACKED, do not fail feature negotiation
	 * simply because the device offers them.
	 */

	if ((required & ~device_features) != 0) {
		VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_FAILED);
		return STATUS_NOT_SUPPORTED;
	}

	negotiated = (device_features & wanted) | required;

	VirtioPciModernTransportWriteDriverFeatures(t, negotiated);

	VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_FEATURES_OK);
	if ((VirtioPciModernTransportGetStatus(t) & VIRTIO_STATUS_FEATURES_OK) == 0) {
		VirtioPciModernTransportAddStatus(t, VIRTIO_STATUS_FAILED);
		return STATUS_NOT_SUPPORTED;
	}

	*negotiated_out = negotiated;
	return STATUS_SUCCESS;
}

UINT16 VirtioPciModernTransportGetNumQueues(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	if (t == NULL || t->CommonCfg == NULL) {
		return 0;
	}
	return t->CommonCfg->num_queues;
}

NTSTATUS VirtioPciModernTransportGetQueueSize(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q, UINT16 *size_out)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 qsz;

	if (t == NULL || t->CommonCfg == NULL || size_out == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	VirtioPciModernLock(t, &state);
	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);
	qsz = t->CommonCfg->queue_size;
	VirtioPciModernUnlock(t, state);

	*size_out = qsz;
	return (qsz != 0) ? STATUS_SUCCESS : STATUS_NOT_FOUND;
}

NTSTATUS VirtioPciModernTransportGetQueueNotifyOff(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q, UINT16 *notify_off_out)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 qsz;
	UINT16 notify_off;
	UINT64 byte_off;

	if (t == NULL || t->CommonCfg == NULL || notify_off_out == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	if (t->NotifyBase == NULL || t->NotifyOffMultiplier == 0 || t->NotifyLength < sizeof(UINT16)) {
		return STATUS_INVALID_DEVICE_STATE;
	}

	VirtioPciModernLock(t, &state);
	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);
	qsz = t->CommonCfg->queue_size;
	notify_off = t->CommonCfg->queue_notify_off;
	VirtioPciModernUnlock(t, state);

	if (qsz == 0) {
		return STATUS_NOT_FOUND;
	}

	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && notify_off != q) {
		t->StrictNotifyOffMismatch = TRUE;
		return STATUS_NOT_SUPPORTED;
	}

	byte_off = (UINT64)notify_off * (UINT64)t->NotifyOffMultiplier;
	if (byte_off + sizeof(UINT16) > (UINT64)t->NotifyLength) {
		return STATUS_INVALID_PARAMETER;
	}

	*notify_off_out = notify_off;
	return STATUS_SUCCESS;
}

NTSTATUS VirtioPciModernTransportSetupQueue(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q, UINT64 desc_pa, UINT64 avail_pa,
					    UINT64 used_pa)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 notify_off;
	UINT16 enabled;
	UINT64 notify_byte_off;

	if (t == NULL || t->CommonCfg == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	/* Basic alignment checks (contract v1). */
	if ((desc_pa & 0xFu) != 0 || (avail_pa & 0x1u) != 0 || (used_pa & 0x3u) != 0) {
		return STATUS_INVALID_PARAMETER;
	}

	VirtioPciModernLock(t, &state);

	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);

	if (t->CommonCfg->queue_size == 0) {
		VirtioPciModernUnlock(t, state);
		return STATUS_NOT_FOUND;
	}

	notify_off = t->CommonCfg->queue_notify_off;
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT && notify_off != q) {
		t->StrictNotifyOffMismatch = TRUE;
		VirtioPciModernUnlock(t, state);
		return STATUS_NOT_SUPPORTED;
	}

	/* Ensure the queue's notify address is within the mapped notify region. */
	notify_byte_off = (UINT64)notify_off * (UINT64)t->NotifyOffMultiplier;
	if (notify_byte_off + sizeof(UINT16) > (UINT64)t->NotifyLength) {
		VirtioPciModernUnlock(t, state);
		return STATUS_INVALID_PARAMETER;
	}

	t->CommonCfg->queue_desc_lo = (UINT32)desc_pa;
	t->CommonCfg->queue_desc_hi = (UINT32)(desc_pa >> 32);
	t->CommonCfg->queue_avail_lo = (UINT32)avail_pa;
	t->CommonCfg->queue_avail_hi = (UINT32)(avail_pa >> 32);
	t->CommonCfg->queue_used_lo = (UINT32)used_pa;
	t->CommonCfg->queue_used_hi = (UINT32)(used_pa >> 32);
	VirtioPciModernMb(t);

	t->CommonCfg->queue_enable = 1;
	VirtioPciModernMb(t);

	/* Optional readback confirmation. */
	enabled = t->CommonCfg->queue_enable;
	if (enabled != 1) {
		VirtioPciModernUnlock(t, state);
		return STATUS_IO_DEVICE_ERROR;
	}

	VirtioPciModernUnlock(t, state);
	return STATUS_SUCCESS;
}

VOID VirtioPciModernTransportDisableQueue(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 qsz;

	if (t == NULL || t->CommonCfg == NULL) {
		return;
	}

	VirtioPciModernLock(t, &state);
	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);
	qsz = t->CommonCfg->queue_size;
	if (qsz != 0) {
		t->CommonCfg->queue_enable = 0;
		VirtioPciModernMb(t);
	}
	VirtioPciModernUnlock(t, state);
}

NTSTATUS VirtioPciModernTransportNotifyQueue(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 qsz;
	UINT16 notify_off;
	UINT64 byte_off;

	if (t == NULL || t->CommonCfg == NULL || t->NotifyBase == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	/*
	 * AERO-W7-VIRTIO v1 fixes notify semantics:
	 *   notify_off_multiplier = 4
	 *   queue_notify_off(q) = q
	 *
	 * Avoid touching the selector-based common_cfg registers on the hot path.
	 */
	if (t->Mode == VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT) {
		if (t->StrictNotifyOffMismatch) {
			return STATUS_NOT_SUPPORTED;
		}
		if (q >= t->CommonCfg->num_queues) {
			return STATUS_NOT_FOUND;
		}
		byte_off = (UINT64)q * (UINT64)t->NotifyOffMultiplier;
		if (byte_off + sizeof(UINT16) > (UINT64)t->NotifyLength) {
			return STATUS_INVALID_PARAMETER;
		}
		/*
		 * Ensure all descriptor / avail ring writes are globally visible before
		 * ringing the doorbell.
		 */
		VirtioPciModernMb(t);
		*(volatile UINT16 *)(t->NotifyBase + (UINT32)byte_off) = q;
		VirtioPciModernMb(t);
		return STATUS_SUCCESS;
	}

	VirtioPciModernLock(t, &state);
	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);
	qsz = t->CommonCfg->queue_size;
	notify_off = t->CommonCfg->queue_notify_off;
	VirtioPciModernUnlock(t, state);

	if (qsz == 0) {
		return STATUS_NOT_FOUND;
	}

	byte_off = (UINT64)notify_off * (UINT64)t->NotifyOffMultiplier;
	if (byte_off + sizeof(UINT16) > (UINT64)t->NotifyLength) {
		return STATUS_INVALID_PARAMETER;
	}

	/* Notify uses a 16-bit write by contract. */
	/*
	 * Ensure all descriptor / avail ring writes are globally visible before
	 * ringing the doorbell.
	 */
	VirtioPciModernMb(t);
	*(volatile UINT16 *)(t->NotifyBase + (UINT32)byte_off) = q;
	VirtioPciModernMb(t);
	return STATUS_SUCCESS;
}

NTSTATUS VirtioPciModernTransportSetConfigMsixVector(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 vector)
{
	UINT16 read_vector;

	if (t == NULL || t->CommonCfg == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	t->CommonCfg->msix_config = vector;
	VirtioPciModernMb(t);
	read_vector = t->CommonCfg->msix_config;

	/*
	 * Virtio spec: devices return VIRTIO_PCI_MSI_NO_VECTOR when MSI-X vector
	 * assignment fails.
	 *
	 * When disabling vectors (vector == VIRTIO_PCI_MSI_NO_VECTOR), accept a
	 * VIRTIO_PCI_MSI_NO_VECTOR readback.
	 */
	if (vector == VIRTIO_PCI_MSI_NO_VECTOR) {
		return (read_vector == VIRTIO_PCI_MSI_NO_VECTOR) ? STATUS_SUCCESS : STATUS_IO_DEVICE_ERROR;
	}

	if (read_vector == VIRTIO_PCI_MSI_NO_VECTOR || read_vector != vector) {
		return STATUS_IO_DEVICE_ERROR;
	}
	return STATUS_SUCCESS;
}

NTSTATUS VirtioPciModernTransportSetQueueMsixVector(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT16 q, UINT16 vector)
{
	VIRTIO_PCI_MODERN_SPINLOCK_STATE state;
	UINT16 qsz;
	UINT16 read_vector;
	NTSTATUS st;

	if (t == NULL || t->CommonCfg == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	VirtioPciModernLock(t, &state);

	t->CommonCfg->queue_select = q;
	VirtioPciModernMb(t);

	qsz = t->CommonCfg->queue_size;
	if (qsz == 0) {
		st = STATUS_NOT_FOUND;
		goto out_unlock;
	}

	t->CommonCfg->queue_msix_vector = vector;
	VirtioPciModernMb(t);
	read_vector = t->CommonCfg->queue_msix_vector;

	if (vector == VIRTIO_PCI_MSI_NO_VECTOR) {
		st = (read_vector == VIRTIO_PCI_MSI_NO_VECTOR) ? STATUS_SUCCESS : STATUS_IO_DEVICE_ERROR;
		goto out_unlock;
	}

	if (read_vector == VIRTIO_PCI_MSI_NO_VECTOR || read_vector != vector) {
		st = STATUS_IO_DEVICE_ERROR;
		goto out_unlock;
	}

	st = STATUS_SUCCESS;

out_unlock:
	VirtioPciModernUnlock(t, state);
	return st;
}

UINT8 VirtioPciModernTransportReadIsrStatus(VIRTIO_PCI_MODERN_TRANSPORT *t)
{
	UINT8 v;

	if (t == NULL || t->IsrStatus == NULL) {
		return 0;
	}

	/* Read-to-ack. */
	v = t->IsrStatus[0];
	VirtioPciModernMb(t);
	return v;
}

NTSTATUS VirtioPciModernTransportReadDeviceConfig(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT32 offset, VOID *buffer,
						  UINT32 length)
{
	UINT8 *out;
	UINT32 i;
	UINT32 attempt;
	UINT8 gen1;
	UINT8 gen2;

	if (t == NULL || t->CommonCfg == NULL || t->DeviceCfg == NULL) {
		return STATUS_INVALID_PARAMETER;
	}
	if (length == 0) {
		return STATUS_SUCCESS;
	}
	if (buffer == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	if (offset + length < offset) {
		return STATUS_INVALID_PARAMETER;
	}
	if ((UINT64)offset + (UINT64)length > (UINT64)t->DeviceCfgLength) {
		return STATUS_BUFFER_TOO_SMALL;
	}

	out = (UINT8 *)buffer;

	for (attempt = 0; attempt < VIRTIO_PCI_CONFIG_MAX_READ_RETRIES; ++attempt) {
		gen1 = t->CommonCfg->config_generation;
		VirtioPciModernMb(t);
		for (i = 0; i < length; ++i) {
			out[i] = t->DeviceCfg[offset + i];
		}
		VirtioPciModernMb(t);
		gen2 = t->CommonCfg->config_generation;
		if (gen1 == gen2) {
			return STATUS_SUCCESS;
		}
	}

	return STATUS_IO_DEVICE_ERROR;
}

NTSTATUS VirtioPciModernTransportWriteDeviceConfig(VIRTIO_PCI_MODERN_TRANSPORT *t, UINT32 offset, const VOID *buffer,
						   UINT32 length)
{
	const UINT8 *in;
	UINT32 i;
	UINT32 attempt;
	UINT8 gen1;
	UINT8 gen2;

	if (t == NULL || t->CommonCfg == NULL || t->DeviceCfg == NULL) {
		return STATUS_INVALID_PARAMETER;
	}
	if (length == 0) {
		return STATUS_SUCCESS;
	}
	if (buffer == NULL) {
		return STATUS_INVALID_PARAMETER;
	}

	if (offset + length < offset) {
		return STATUS_INVALID_PARAMETER;
	}
	if ((UINT64)offset + (UINT64)length > (UINT64)t->DeviceCfgLength) {
		return STATUS_BUFFER_TOO_SMALL;
	}

	in = (const UINT8 *)buffer;

	for (attempt = 0; attempt < VIRTIO_PCI_CONFIG_MAX_READ_RETRIES; ++attempt) {
		gen1 = t->CommonCfg->config_generation;
		VirtioPciModernMb(t);
		for (i = 0; i < length; ++i) {
			t->DeviceCfg[offset + i] = in[i];
		}
		VirtioPciModernMb(t);
		gen2 = t->CommonCfg->config_generation;
		if (gen1 == gen2) {
			return STATUS_SUCCESS;
		}
	}

	return STATUS_IO_DEVICE_ERROR;
}
