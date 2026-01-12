#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../virtio_pci_modern_transport.h"

#include "../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.h"

/*
 * Keep assertions active in all build configurations.
 *
 * These host tests run under Release in CI. CMake Release builds define NDEBUG,
 * which would normally compile out assert() checks. Override assert() so test
 * coverage is preserved and side-effectful expressions still execute.
 */
#undef assert
#define assert(expr)                                                                                                   \
	do {                                                                                                           \
		if (!(expr)) {                                                                                        \
			fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                   \
			abort();                                                                                     \
		}                                                                                                      \
	} while (0)

enum {
	PCI_VENDOR_OFF = 0x00,
	PCI_DEVICE_OFF = 0x02,
	PCI_STATUS_OFF = 0x06,
	PCI_REVISION_OFF = 0x08,
	PCI_SUBSYSTEM_VENDOR_OFF = 0x2C,
	PCI_SUBSYSTEM_DEVICE_OFF = 0x2E,
	PCI_BAR0_OFF = 0x10,
	PCI_CAP_PTR_OFF = 0x34,
	PCI_INTERRUPT_PIN_OFF = 0x3D,

	PCI_STATUS_CAP_LIST = 1u << 4,

	BAR0_LEN = 0x4000,
	FAKE_MAX_QUEUES = 8,
};

typedef struct _FAKE_DEV {
	UINT8 Cfg[256];
	UINT8 Bar0[BAR0_LEN];
	UINT64 DeviceFeatures;
	UINT64 DriverFeatures;
	UINT16 QueueSize[FAKE_MAX_QUEUES];
	UINT16 QueueNotifyOff[FAKE_MAX_QUEUES];
	UINT32 MbBumpConfigGenRemaining;
	BOOLEAN MbFillDeviceCfgOnBump;
	UINT32 MbFillDeviceCfgOffset;
	UINT32 MbFillDeviceCfgLength;
	UINT8 MbFillDeviceCfgValue;
	BOOLEAN MbForceMsixConfigNoVector;
	BOOLEAN MbForceQueueMsixVectorNoVector;
	BOOLEAN MbForceMsixConfigMismatch;
	UINT16 MbForcedMsixConfigMismatch;
	BOOLEAN MbForceQueueMsixVectorMismatch;
	UINT16 MbForcedQueueMsixVectorMismatch;

	BOOLEAN MbPoisonNotifyOnNextMb;
	UINT32 MbPoisonNotifyBar0Off;
	UINT16 MbPoisonNotifyValue;

	/* OsMb() instrumentation used by host tests. */
	UINT32 MbCallCount;
	BOOLEAN MbRecordDoorbell;
	UINT32 MbRecordDoorbellOffset;
	UINT32 MbDoorbellSampleCount;
	UINT16 MbDoorbellSamples[4];
} FAKE_DEV;

static void WriteLe16(UINT8 *p, UINT16 v)
{
	p[0] = (UINT8)(v & 0xffu);
	p[1] = (UINT8)((v >> 8) & 0xffu);
}

static UINT16 ReadLe16(const UINT8 *p)
{
	return (UINT16)p[0] | ((UINT16)p[1] << 8);
}

static void WriteLe32(UINT8 *p, UINT32 v)
{
	p[0] = (UINT8)(v & 0xffu);
	p[1] = (UINT8)((v >> 8) & 0xffu);
	p[2] = (UINT8)((v >> 16) & 0xffu);
	p[3] = (UINT8)((v >> 24) & 0xffu);
}

static void AddVirtioCap(UINT8 cfg[256], UINT8 cap_off, UINT8 cap_next, UINT8 cfg_type, UINT8 bar, UINT32 region_off,
			 UINT32 region_len, UINT8 cap_len)
{
	cfg[cap_off + 0] = VIRTIO_PCI_CAP_PARSER_PCI_CAP_ID_VNDR;
	cfg[cap_off + 1] = cap_next;
	cfg[cap_off + 2] = cap_len;
	cfg[cap_off + 3] = cfg_type;
	cfg[cap_off + 4] = bar;
	cfg[cap_off + 5] = 0;
	cfg[cap_off + 6] = 0;
	cfg[cap_off + 7] = 0;
	WriteLe32(&cfg[cap_off + 8], region_off);
	WriteLe32(&cfg[cap_off + 12], region_len);
}

static void AddVirtioNotifyCap(UINT8 cfg[256], UINT8 cap_off, UINT8 cap_next, UINT8 bar, UINT32 region_off, UINT32 region_len,
			       UINT32 mult)
{
	AddVirtioCap(cfg, cap_off, cap_next, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_NOTIFY, bar, region_off, region_len, 20);
	WriteLe32(&cfg[cap_off + 16], mult);
}

static void FakeDevInitValid(FAKE_DEV *dev)
{
	volatile virtio_pci_common_cfg *common;

	memset(dev, 0, sizeof(*dev));
	dev->DeviceFeatures = VIRTIO_F_VERSION_1 | ((UINT64)1u << 28); /* INDIRECT_DESC */
	dev->DriverFeatures = 0;
	dev->QueueSize[0] = 8;
	dev->QueueNotifyOff[0] = 0;

	/* PCI header */
	WriteLe16(&dev->Cfg[PCI_VENDOR_OFF], 0x1AF4);
	WriteLe16(&dev->Cfg[PCI_DEVICE_OFF], 0x1052);
	WriteLe16(&dev->Cfg[PCI_STATUS_OFF], PCI_STATUS_CAP_LIST);
	dev->Cfg[PCI_REVISION_OFF] = 0x01;
	WriteLe16(&dev->Cfg[PCI_SUBSYSTEM_VENDOR_OFF], 0x1AF4);
	WriteLe16(&dev->Cfg[PCI_SUBSYSTEM_DEVICE_OFF], 0x0010);
	dev->Cfg[PCI_INTERRUPT_PIN_OFF] = 0x01;
	/* BAR0: memory, 64-bit indicator (bits 2:1 = 2) */
	WriteLe32(&dev->Cfg[PCI_BAR0_OFF], 0x10000000u | 0x4u);

	/* Cap list */
	dev->Cfg[PCI_CAP_PTR_OFF] = 0x40;
	AddVirtioCap(dev->Cfg, 0x40, 0x50, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, 0x0000, 0x0100, 16);
	/* Notify cap is 20 bytes; next must not overlap the notify_off_multiplier field at +16. */
	AddVirtioNotifyCap(dev->Cfg, 0x50, 0x64, 0, 0x1000, 0x0100, 4);
	AddVirtioCap(dev->Cfg, 0x64, 0x74, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 0, 0x2000, 0x0020, 16);
	AddVirtioCap(dev->Cfg, 0x74, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 0, 0x3000, 0x0100, 16);

	/* BAR0 MMIO contents */
	common = (volatile virtio_pci_common_cfg *)(dev->Bar0 + 0x0000);
	common->num_queues = 1;
	common->queue_size = 0;
	common->queue_notify_off = 0;
	common->device_feature = 0;
}

static void FakeDevInitCompatRelocated(FAKE_DEV *dev)
{
	enum {
		COMMON_OFF = 0x0100,
		NOTIFY_OFF = 0x1200,
		ISR_OFF = 0x2200,
		DEVICE_OFF = 0x3200,
	};

	volatile virtio_pci_common_cfg *common;

	memset(dev, 0, sizeof(*dev));

	/* PCI header */
	WriteLe16(&dev->Cfg[PCI_VENDOR_OFF], 0x1AF4);
	WriteLe16(&dev->Cfg[PCI_DEVICE_OFF], 0x1052);
	WriteLe16(&dev->Cfg[PCI_STATUS_OFF], PCI_STATUS_CAP_LIST);
	dev->Cfg[PCI_REVISION_OFF] = 0x01;
	WriteLe16(&dev->Cfg[PCI_SUBSYSTEM_VENDOR_OFF], 0x1AF4);
	WriteLe16(&dev->Cfg[PCI_SUBSYSTEM_DEVICE_OFF], 0x0010);
	dev->Cfg[PCI_INTERRUPT_PIN_OFF] = 0x01;
	/* BAR0: memory, 64-bit indicator (bits 2:1 = 2) */
	WriteLe32(&dev->Cfg[PCI_BAR0_OFF], 0x10000000u | 0x4u);

	/* Cap list */
	dev->Cfg[PCI_CAP_PTR_OFF] = 0x40;
	AddVirtioCap(dev->Cfg, 0x40, 0x50, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_COMMON, 0, COMMON_OFF, 0x0100, 16);
	AddVirtioNotifyCap(dev->Cfg, 0x50, 0x64, 0, NOTIFY_OFF, 0x0100, 4);
	AddVirtioCap(dev->Cfg, 0x64, 0x74, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_ISR, 0, ISR_OFF, 0x0020, 16);
	AddVirtioCap(dev->Cfg, 0x74, 0x00, VIRTIO_PCI_CAP_PARSER_CFG_TYPE_DEVICE, 0, DEVICE_OFF, 0x0100, 16);

	/* BAR0 MMIO contents */
	common = (volatile virtio_pci_common_cfg *)(dev->Bar0 + COMMON_OFF);
	common->num_queues = 1;
	common->queue_size = 8;
	common->queue_notify_off = 0;
	common->device_feature = 1;
}

static void FakeDevResetMbInstrumentation(FAKE_DEV *dev)
{
	UINT32 i;

	dev->MbCallCount = 0;
	dev->MbDoorbellSampleCount = 0;
	for (i = 0; i < (UINT32)(sizeof(dev->MbDoorbellSamples) / sizeof(dev->MbDoorbellSamples[0])); ++i) {
		dev->MbDoorbellSamples[i] = 0;
	}
}

static UINT8 OsPciRead8(void *ctx, UINT16 off)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	assert(off < 256);
	return dev->Cfg[off];
}

static UINT16 OsPciRead16(void *ctx, UINT16 off)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	assert((UINT32)off + 1u < 256u);
	return (UINT16)dev->Cfg[off] | ((UINT16)dev->Cfg[off + 1] << 8);
}

static UINT32 OsPciRead32(void *ctx, UINT16 off)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	assert((UINT32)off + 3u < 256u);
	return (UINT32)dev->Cfg[off] | ((UINT32)dev->Cfg[off + 1] << 8) | ((UINT32)dev->Cfg[off + 2] << 16) |
	       ((UINT32)dev->Cfg[off + 3] << 24);
}

static NTSTATUS OsMapMmio(void *ctx, UINT64 pa, UINT32 len, volatile void **va_out)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	(void)pa;
	assert(len <= sizeof(dev->Bar0));
	*va_out = dev->Bar0;
	return STATUS_SUCCESS;
}

static void OsUnmapMmio(void *ctx, volatile void *va, UINT32 len)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	(void)len;
	assert(va == dev->Bar0);
}

static void OsStallUs(void *ctx, UINT32 us)
{
	(void)ctx;
	(void)us;
}

static void OsMb(void *ctx)
{
	FAKE_DEV *dev = (FAKE_DEV *)ctx;
	volatile virtio_pci_common_cfg *common;
	UINT32 sel;
	UINT16 q;
	UINT64 feat;
	UINT32 i;

	dev->MbCallCount++;
	if (dev->MbRecordDoorbell && dev->MbDoorbellSampleCount < (UINT32)(sizeof(dev->MbDoorbellSamples) / sizeof(dev->MbDoorbellSamples[0]))) {
		assert(dev->MbRecordDoorbellOffset + sizeof(UINT16) <= BAR0_LEN);
		dev->MbDoorbellSamples[dev->MbDoorbellSampleCount++] = ReadLe16(dev->Bar0 + dev->MbRecordDoorbellOffset);
	}

	common = (volatile virtio_pci_common_cfg *)(dev->Bar0 + 0x0000);

	/*
	 * Emulate the selector semantics of virtio_pci_common_cfg for host tests.
	 *
	 * Real hardware exposes device_feature / queue_size / queue_notify_off as
	 * selector-indexed windows, but our BAR0 is just a byte array.
	 *
	 * The transport calls MemoryBarrier() after updating selectors; use that hook
	 * to update the windows so tests exercise the correct access patterns.
	 */
	sel = common->device_feature_select;
	feat = dev->DeviceFeatures;
	if (sel == 0) {
		common->device_feature = (UINT32)(feat & 0xFFFFFFFFull);
	} else if (sel == 1) {
		common->device_feature = (UINT32)(feat >> 32);
	} else {
		common->device_feature = 0;
	}

	q = common->queue_select;
	if (q < common->num_queues && q < FAKE_MAX_QUEUES) {
		common->queue_size = dev->QueueSize[q];
		common->queue_notify_off = dev->QueueNotifyOff[q];
	} else {
		common->queue_size = 0;
		common->queue_notify_off = 0;
	}

	/* Capture driver features written by the transport. */
	sel = common->driver_feature_select;
	if (sel == 0) {
		dev->DriverFeatures = (dev->DriverFeatures & 0xFFFFFFFF00000000ull) | (UINT64)common->driver_feature;
	} else if (sel == 1) {
		dev->DriverFeatures = (dev->DriverFeatures & 0x00000000FFFFFFFFull) | ((UINT64)common->driver_feature << 32);
	}

	/*
	 * Optional config_generation / DEVICE_CFG mutation hook used by config read/write
	 * unit tests.
	 */
	if (dev->MbBumpConfigGenRemaining != 0) {
		dev->MbBumpConfigGenRemaining--;
		common->config_generation = (UINT8)(common->config_generation + 1u);

		if (dev->MbFillDeviceCfgOnBump) {
			assert(0x3000u + dev->MbFillDeviceCfgOffset + dev->MbFillDeviceCfgLength <= BAR0_LEN);
			for (i = 0; i < dev->MbFillDeviceCfgLength; ++i) {
				dev->Bar0[0x3000u + dev->MbFillDeviceCfgOffset + i] = dev->MbFillDeviceCfgValue;
			}
		}
	}

	/*
	 * Optional MSI-X vector programming behavior override.
	 *
	 * Some virtio devices may refuse vector programming and read back
	 * VIRTIO_PCI_MSI_NO_VECTOR (0xFFFF). The transport is expected to detect
	 * this via readback validation.
	 */
	if (dev->MbForceMsixConfigNoVector) {
		common->msix_config = VIRTIO_PCI_MSI_NO_VECTOR;
	}
	if (dev->MbForceQueueMsixVectorNoVector) {
		common->queue_msix_vector = VIRTIO_PCI_MSI_NO_VECTOR;
	}
	if (dev->MbForceMsixConfigMismatch) {
		common->msix_config = dev->MbForcedMsixConfigMismatch;
	}
	if (dev->MbForceQueueMsixVectorMismatch) {
		common->queue_msix_vector = dev->MbForcedQueueMsixVectorMismatch;
	}

	/*
	 * One-shot "poison" write hook used by notify ordering tests.
	 *
	 * When armed, the next MemoryBarrier() call will overwrite the notify
	 * register. This lets unit tests detect whether the transport issues a
	 * barrier before or only after ringing the notify doorbell.
	 */
	if (dev->MbPoisonNotifyOnNextMb) {
		dev->MbPoisonNotifyOnNextMb = FALSE;
		assert(dev->MbPoisonNotifyBar0Off + sizeof(UINT16) <= BAR0_LEN);
		*(volatile UINT16 *)(dev->Bar0 + dev->MbPoisonNotifyBar0Off) = dev->MbPoisonNotifyValue;
	}
}

static void *OsSpinlockCreate(void *ctx)
{
	(void)ctx;
	return malloc(sizeof(int));
}

static void OsSpinlockDestroy(void *ctx, void *lock)
{
	(void)ctx;
	free(lock);
}

static void OsSpinlockAcquire(void *ctx, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE *state_out)
{
	(void)ctx;
	(void)state_out;
	assert(lock != NULL);
}

static void OsSpinlockRelease(void *ctx, void *lock, VIRTIO_PCI_MODERN_SPINLOCK_STATE state)
{
	(void)ctx;
	(void)lock;
	(void)state;
}

static void OsLog(void *ctx, const char *msg)
{
	(void)ctx;
	(void)msg;
}

static VIRTIO_PCI_MODERN_OS_INTERFACE GetOs(FAKE_DEV *dev)
{
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	memset(&os, 0, sizeof(os));
	os.Context = dev;
	os.PciRead8 = OsPciRead8;
	os.PciRead16 = OsPciRead16;
	os.PciRead32 = OsPciRead32;
	os.MapMmio = OsMapMmio;
	os.UnmapMmio = OsUnmapMmio;
	os.StallUs = OsStallUs;
	os.MemoryBarrier = OsMb;
	os.SpinlockCreate = OsSpinlockCreate;
	os.SpinlockDestroy = OsSpinlockDestroy;
	os.SpinlockAcquire = OsSpinlockAcquire;
	os.SpinlockRelease = OsSpinlockRelease;
	os.Log = OsLog;
	return os;
}

static void ExpectInitFail(const char *name, FAKE_DEV *dev, VIRTIO_PCI_MODERN_TRANSPORT_INIT_ERROR expected_err)
{
	VIRTIO_PCI_MODERN_OS_INTERFACE os = GetOs(dev);
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev->Bar0));
	if (st == STATUS_SUCCESS) {
		fprintf(stderr, "FAIL %s: init unexpectedly succeeded\n", name);
		abort();
	}
	if (t.InitError != expected_err) {
		fprintf(stderr, "FAIL %s: InitError=%s (%u) expected=%s (%u)\n", name,
			VirtioPciModernTransportInitErrorStr(t.InitError), (unsigned)t.InitError,
			VirtioPciModernTransportInitErrorStr(expected_err), (unsigned)expected_err);
		abort();
	}
}

static void TestInitOk(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	if (st != STATUS_SUCCESS) {
		fprintf(stderr, "FAIL init_ok: status=0x%x InitError=%s (%u) CapParse=%u\n", (unsigned)st,
			VirtioPciModernTransportInitErrorStr(t.InitError), (unsigned)t.InitError, (unsigned)t.CapParseResult);
		abort();
	}
	assert(t.CommonCfg != NULL);
	assert(t.NotifyBase != NULL);
	assert(t.IsrStatus != NULL);
	assert(t.DeviceCfg != NULL);
	assert(t.NotifyOffMultiplier == 4);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_OK);
	assert(t.PciVendorId == 0x1AF4);
	assert(t.PciDeviceId == 0x1052);
	assert(t.PciRevisionId == 0x01);
	assert(t.PciSubsystemVendorId == 0x1AF4);
	assert(t.PciSubsystemDeviceId == 0x0010);
	assert(t.PciInterruptPin == 0x01);

	VirtioPciModernTransportUninit(&t);
}

static void TestRejectBadVendor(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	WriteLe16(&dev.Cfg[PCI_VENDOR_OFF], 0x1234);

	ExpectInitFail("bad_vendor", &dev, VIRTIO_PCI_MODERN_INIT_ERR_VENDOR_MISMATCH);
}

static void TestRejectInvalidMode(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, (VIRTIO_PCI_MODERN_TRANSPORT_MODE)2, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_INVALID_PARAMETER);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_ERR_BAD_ARGUMENT);
}

static void TestRejectNonModernDeviceId(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	WriteLe16(&dev.Cfg[PCI_DEVICE_OFF], 0x1000);

	ExpectInitFail("device_id_not_modern", &dev, VIRTIO_PCI_MODERN_INIT_ERR_DEVICE_ID_NOT_MODERN);
}

static void TestRejectBadRevision(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_REVISION_OFF] = 0x02;

	ExpectInitFail("bad_revision", &dev, VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION);
}

static void TestRejectBadSubsystemVendor(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	WriteLe16(&dev.Cfg[PCI_SUBSYSTEM_VENDOR_OFF], 0x1234);

	ExpectInitFail("bad_subsystem_vendor", &dev, VIRTIO_PCI_MODERN_INIT_ERR_SUBSYSTEM_VENDOR_MISMATCH);
}

static void TestRejectBadInterruptPin(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_INTERRUPT_PIN_OFF] = 0;

	ExpectInitFail("bad_interrupt_pin", &dev, VIRTIO_PCI_MODERN_INIT_ERR_INTERRUPT_PIN_MISMATCH);
}

static void TestRejectBar0IoSpace(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* BAR0 bit0=1 => I/O */
	WriteLe32(&dev.Cfg[PCI_BAR0_OFF], 0xC001u);

	ExpectInitFail("bar0_not_mmio", &dev, VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO);
}

static void TestRejectBar0Not64BitMmio(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* Memory BAR, but 32-bit type (bits [2:1]=0b00). */
	WriteLe32(&dev.Cfg[PCI_BAR0_OFF], 0x10000000u);

	ExpectInitFail("bar0_not_64bit_mmio", &dev, VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_64BIT_MMIO);
}

static void TestRejectBar0AddressMismatch(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* BAR0 base differs from the Bar0Pa passed to init. */
	WriteLe32(&dev.Cfg[PCI_BAR0_OFF], 0x20000000u | 0x4u);

	ExpectInitFail("bar0_address_mismatch", &dev, VIRTIO_PCI_MODERN_INIT_ERR_BAR0_ADDRESS_MISMATCH);
}

static void TestRejectMissingStatusCapList(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	WriteLe16(&dev.Cfg[PCI_STATUS_OFF], 0);

	ExpectInitFail("missing_status_cap_list", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_NO_CAP_LIST_STATUS);
}

static void TestRejectUnalignedCapPtr(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_CAP_PTR_OFF] = 0x41;

	ExpectInitFail("unaligned_cap_ptr", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_PTR_UNALIGNED);
}

static void TestRejectZeroCapPtr(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_CAP_PTR_OFF] = 0;

	ExpectInitFail("zero_cap_ptr", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID);
}

static void TestRejectCapPtrBelow0x40(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_CAP_PTR_OFF] = 0x20;

	ExpectInitFail("cap_ptr_below_0x40", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID);
}

static void TestRejectCapNextBelow0x40(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* cap_next must point to another entry in the capabilities area (>=0x40) */
	dev.Cfg[0x40 + 1] = 0x20;

	ExpectInitFail("cap_next_below_0x40", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID);
}

static void TestRejectCapListLoop(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* Create a cycle: last cap points back to the first cap. */
	dev.Cfg[0x74 + 1] = 0x40;

	ExpectInitFail("cap_list_loop", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID);
}

static void TestRejectWrongNotifyMultiplier(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	WriteLe32(&dev.Cfg[0x50 + 16], 8);

	ExpectInitFail("wrong_notify_multiplier", &dev, VIRTIO_PCI_MODERN_INIT_ERR_NOTIFY_MULTIPLIER_MISMATCH);
}

static void TestRejectWrongOffsets(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* CommonCfg must be at 0x0000; move it to 0x0100. */
	WriteLe32(&dev.Cfg[0x40 + 8], 0x0100);

	ExpectInitFail("wrong_offsets", &dev, VIRTIO_PCI_MODERN_INIT_ERR_CAP_LAYOUT_MISMATCH);
}

static void TestCompatAllowsNonContractOffsets(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);

	/*
	 * COMPAT mode relaxes the fixed-offset requirement (e.g. for QEMU-style
	 * layouts) as long as BAR0-only virtio caps exist and satisfy minimum sizes.
	 */
	WriteLe32(&dev.Cfg[0x40 + 8], 0x0100); /* COMMON */
	WriteLe32(&dev.Cfg[0x50 + 8], 0x1100); /* NOTIFY */
	WriteLe32(&dev.Cfg[0x64 + 8], 0x2100); /* ISR */
	WriteLe32(&dev.Cfg[0x74 + 8], 0x3100); /* DEVICE */

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	if (st != STATUS_SUCCESS) {
		fprintf(stderr, "FAIL compat_offsets_ok: status=0x%x InitError=%s (%u)\n", (unsigned)st,
			VirtioPciModernTransportInitErrorStr(t.InitError), (unsigned)t.InitError);
		abort();
	}

	assert((const UINT8 *)t.CommonCfg == dev.Bar0 + 0x0100);
	assert((const UINT8 *)t.NotifyBase == dev.Bar0 + 0x1100);
	assert((const UINT8 *)t.IsrStatus == dev.Bar0 + 0x2100);
	assert((const UINT8 *)t.DeviceCfg == dev.Bar0 + 0x3100);

	VirtioPciModernTransportUninit(&t);
}

static void TestRejectBar0TooSmall(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, 0x2000);
	assert(st == STATUS_BUFFER_TOO_SMALL);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_ERR_BAR0_TOO_SMALL);
}

static void TestRejectUnalignedCapNext(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* cap_next must be 4-byte aligned. */
	dev.Cfg[0x40 + 1] = 0x51;

	ExpectInitFail("unaligned_cap_next", &dev, VIRTIO_PCI_MODERN_INIT_ERR_PCI_CAP_LIST_INVALID);
}

static void TestRejectMissingDeviceCfgCap(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* Turn the DEVICE cfg cap into a non-vendor capability so parsing fails. */
	dev.Cfg[0x74 + 0] = 0x05;

	ExpectInitFail("missing_device_cfg_cap", &dev, VIRTIO_PCI_MODERN_INIT_ERR_CAP_PARSE_FAILED);
}

static NTSTATUS OsMapMmioFail(void *ctx, UINT64 pa, UINT32 len, volatile void **va_out)
{
	(void)ctx;
	(void)pa;
	(void)len;
	(void)va_out;
	return STATUS_UNSUCCESSFUL;
}

static NTSTATUS OsMapMmioNull(void *ctx, UINT64 pa, UINT32 len, volatile void **va_out)
{
	(void)ctx;
	(void)pa;
	(void)len;
	*va_out = NULL;
	return STATUS_SUCCESS;
}

static void *OsSpinlockCreateFail(void *ctx)
{
	(void)ctx;
	return NULL;
}

static void TestRejectMapMmioFailure(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);
	os.MapMmio = OsMapMmioFail;

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_UNSUCCESSFUL);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_ERR_MAP_MMIO_FAILED);
}

static void TestRejectMapMmioNullPointer(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);
	os.MapMmio = OsMapMmioNull;

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_INSUFFICIENT_RESOURCES);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_ERR_MAP_MMIO_FAILED);
}

static void TestRejectSpinlockCreateFailure(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);
	os.SpinlockCreate = OsSpinlockCreateFail;

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_INSUFFICIENT_RESOURCES);
	assert(t.InitError == VIRTIO_PCI_MODERN_INIT_ERR_LOCK_CREATE_FAILED);
}

static void TestNegotiateFeaturesOk(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;
	UINT64 wanted;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	wanted = (UINT64)1u << 28; /* INDIRECT_DESC */
	negotiated = 0;
	st = VirtioPciModernTransportNegotiateFeatures(&t, 0, wanted, &negotiated);
	assert(st == STATUS_SUCCESS);
	assert((negotiated & VIRTIO_F_VERSION_1) != 0);
	assert((negotiated & wanted) == wanted);
	assert((negotiated & ((UINT64)1u << 29)) == 0);
	assert((negotiated & ((UINT64)1u << 34)) == 0);
	assert(dev.DriverFeatures == negotiated);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) == 0);
	assert((VirtioPciModernTransportGetStatus(&t) & (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK)) ==
	       (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK));

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesRejectNoVersion1(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	/* device_features must include VIRTIO_F_VERSION_1 (bit 32). */
	dev.DeviceFeatures = 0;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	negotiated = 0xDEADBEEFDEADBEEFull;
	st = VirtioPciModernTransportNegotiateFeatures(&t, 0, 0, &negotiated);
	assert(st == STATUS_NOT_SUPPORTED);
	assert(negotiated == 0);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) != 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesStrictRejectNoIndirectDesc(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.DeviceFeatures &= ~((UINT64)1u << 28);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	negotiated = 0xDEADBEEFDEADBEEFull;
	st = VirtioPciModernTransportNegotiateFeatures(&t, 0, 0, &negotiated);
	assert(st == STATUS_NOT_SUPPORTED);
	assert(negotiated == 0);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) != 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesStrictDoesNotNegotiateEventIdxOffered(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;
	UINT64 required;
	UINT64 wanted;

	FakeDevInitValid(&dev);
	/* Device offers EVENT_IDX. STRICT must still succeed but must not negotiate it. */
	dev.DeviceFeatures = VIRTIO_F_VERSION_1 | ((UINT64)1u << 28) | ((UINT64)1u << 29);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	required = (UINT64)1u << 28; /* INDIRECT_DESC */
	wanted = (UINT64)1u << 29;   /* EVENT_IDX (must be masked out) */
	negotiated = 0;
	st = VirtioPciModernTransportNegotiateFeatures(&t, required, wanted, &negotiated);
	assert(st == STATUS_SUCCESS);
	assert((negotiated & ((UINT64)1u << 29)) == 0);
	assert((negotiated & ((UINT64)1u << 34)) == 0);
	assert((negotiated & VIRTIO_F_VERSION_1) != 0);
	assert((negotiated & required) == required);
	assert(dev.DriverFeatures == negotiated);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesCompatDoesNotNegotiateEventIdx(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	/* Device offers EVENT_IDX. COMPAT mode allows init + negotiation but must not accept it. */
	dev.DeviceFeatures = VIRTIO_F_VERSION_1 | ((UINT64)1u << 28) | ((UINT64)1u << 29);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	negotiated = 0;
	st = VirtioPciModernTransportNegotiateFeatures(&t, 0, (UINT64)1u << 29, &negotiated);
	assert(st == STATUS_SUCCESS);
	assert((negotiated & ((UINT64)1u << 29)) == 0);
	assert((negotiated & ((UINT64)1u << 34)) == 0);
	assert((negotiated & VIRTIO_F_VERSION_1) != 0);
	assert(dev.DriverFeatures == negotiated);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesRejectsRequiredEventIdx(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	assert(dev.DriverFeatures == 0);
	assert(VirtioPciModernTransportGetStatus(&t) == 0);

	negotiated = 0xDEADBEEFDEADBEEFull;
	st = VirtioPciModernTransportNegotiateFeatures(&t, (UINT64)1u << 29, 0, &negotiated);
	assert(st == STATUS_INVALID_PARAMETER);
	assert(negotiated == 0);
	assert(dev.DriverFeatures == 0);
	assert(VirtioPciModernTransportGetStatus(&t) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesStrictDoesNotNegotiatePackedRingOffered(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;
	UINT64 required;
	UINT64 wanted;

	FakeDevInitValid(&dev);
	/* Device offers PACKED ring. STRICT must still succeed but must not negotiate it. */
	dev.DeviceFeatures = VIRTIO_F_VERSION_1 | ((UINT64)1u << 28) | ((UINT64)1u << 34);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	required = (UINT64)1u << 28; /* INDIRECT_DESC */
	wanted = (UINT64)1u << 34;   /* PACKED (must be masked out) */
	negotiated = 0;
	st = VirtioPciModernTransportNegotiateFeatures(&t, required, wanted, &negotiated);
	assert(st == STATUS_SUCCESS);
	assert((negotiated & ((UINT64)1u << 34)) == 0);
	assert((negotiated & VIRTIO_F_VERSION_1) != 0);
	assert((negotiated & required) == required);
	assert(dev.DriverFeatures == negotiated);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesCompatDoesNotNegotiatePackedRing(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	/* Device offers PACKED ring. COMPAT mode allows init + negotiation but must not accept it. */
	dev.DeviceFeatures = VIRTIO_F_VERSION_1 | ((UINT64)1u << 28) | ((UINT64)1u << 34);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	negotiated = 0;
	st = VirtioPciModernTransportNegotiateFeatures(&t, 0, (UINT64)1u << 34, &negotiated);
	assert(st == STATUS_SUCCESS);
	assert((negotiated & ((UINT64)1u << 34)) == 0);
	assert((negotiated & VIRTIO_F_VERSION_1) != 0);
	assert(dev.DriverFeatures == negotiated);
	assert((VirtioPciModernTransportGetStatus(&t) & VIRTIO_STATUS_FAILED) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestNegotiateFeaturesRejectsRequiredPackedRing(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT64 negotiated;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	assert(dev.DriverFeatures == 0);
	assert(VirtioPciModernTransportGetStatus(&t) == 0);

	negotiated = 0xDEADBEEFDEADBEEFull;
	st = VirtioPciModernTransportNegotiateFeatures(&t, (UINT64)1u << 34, 0, &negotiated);
	assert(st == STATUS_INVALID_PARAMETER);
	assert(negotiated == 0);
	assert(dev.DriverFeatures == 0);
	assert(VirtioPciModernTransportGetStatus(&t) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueSetupAndNotify(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	volatile virtio_pci_common_cfg *common;
	NTSTATUS st;
	UINT16 qsz;
	UINT16 notify_off;
	const UINT64 desc_pa = 0x1122334455667700ull;
	const UINT64 avail_pa = 0x1122334455668800ull;
	const UINT64 used_pa = 0x1122334455669900ull;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	common = (volatile virtio_pci_common_cfg *)(dev.Bar0 + 0x0000);

	qsz = 0;
	st = VirtioPciModernTransportGetQueueSize(&t, 0, &qsz);
	assert(st == STATUS_SUCCESS);
	assert(qsz == 8);

	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa, avail_pa, used_pa);
	assert(st == STATUS_SUCCESS);
	assert(common->queue_desc_lo == (UINT32)desc_pa);
	assert(common->queue_desc_hi == (UINT32)(desc_pa >> 32));
	assert(common->queue_avail_lo == (UINT32)avail_pa);
	assert(common->queue_avail_hi == (UINT32)(avail_pa >> 32));
	assert(common->queue_used_lo == (UINT32)used_pa);
	assert(common->queue_used_hi == (UINT32)(used_pa >> 32));
	assert(common->queue_enable == 1);

	/* Notify should write the queue index into BAR0+0x1000. */
	*(UINT16 *)(dev.Bar0 + 0x1000) = 0xFFFFu;
	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_SUCCESS);
	assert(*(UINT16 *)(dev.Bar0 + 0x1000) == 0);

	notify_off = 0;
	st = VirtioPciModernTransportGetQueueNotifyOff(&t, 0, &notify_off);
	assert(st == STATUS_SUCCESS);
	assert(notify_off == 0);

	/* STRICT: reject queue_notify_off mismatch. */
	dev.QueueNotifyOff[0] = 5;
	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa, avail_pa, used_pa);
	assert(st == STATUS_NOT_SUPPORTED);

	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_NOT_SUPPORTED);

	st = VirtioPciModernTransportGetQueueNotifyOff(&t, 0, &notify_off);
	assert(st == STATUS_NOT_SUPPORTED);

	/* MSI-X helpers should program fields under the selector lock. */
	st = VirtioPciModernTransportSetConfigMsixVector(&t, VIRTIO_PCI_MSI_NO_VECTOR);
	assert(st == STATUS_SUCCESS);
	assert(common->msix_config == VIRTIO_PCI_MSI_NO_VECTOR);

	st = VirtioPciModernTransportSetQueueMsixVector(&t, 0, VIRTIO_PCI_MSI_NO_VECTOR);
	assert(st == STATUS_SUCCESS);
	assert(common->queue_msix_vector == VIRTIO_PCI_MSI_NO_VECTOR);

	VirtioPciModernTransportUninit(&t);
}

static void TestStrictNotifyHasPreAndPostMemoryBarrier(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	WriteLe16(dev.Bar0 + 0x1000, 0xFFFFu);
	dev.MbRecordDoorbell = TRUE;
	dev.MbRecordDoorbellOffset = 0x1000;
	FakeDevResetMbInstrumentation(&dev);

	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_SUCCESS);
	assert(ReadLe16(dev.Bar0 + 0x1000) == 0);

	assert(dev.MbCallCount == 2);
	assert(dev.MbDoorbellSampleCount == 2);
	assert(dev.MbDoorbellSamples[0] == 0xFFFFu);
	assert(dev.MbDoorbellSamples[1] == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestCompatNotifyHasSelectorAndPreAndPostMemoryBarrier(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	WriteLe16(dev.Bar0 + 0x1000, 0xFFFFu);
	dev.MbRecordDoorbell = TRUE;
	dev.MbRecordDoorbellOffset = 0x1000;
	FakeDevResetMbInstrumentation(&dev);

	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_SUCCESS);
	assert(ReadLe16(dev.Bar0 + 0x1000) == 0);

	/* COMPAT notify touches the selector window + pre- and post-notify barriers. */
	assert(dev.MbCallCount == 3);
	assert(dev.MbDoorbellSampleCount == 3);
	assert(dev.MbDoorbellSamples[1] == 0xFFFFu);
	assert(dev.MbDoorbellSamples[2] == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestMsixConfigVectorRefusedFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceMsixConfigNoVector = TRUE;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetConfigMsixVector(&t, 1);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueMsixVectorRefusedFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceQueueMsixVectorNoVector = TRUE;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetQueueMsixVector(&t, 0, 2);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestMsixConfigVectorMismatchFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceMsixConfigMismatch = TRUE;
	dev.MbForcedMsixConfigMismatch = 5;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetConfigMsixVector(&t, 1);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueMsixVectorMismatchFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceQueueMsixVectorMismatch = TRUE;
	dev.MbForcedQueueMsixVectorMismatch = 7;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetQueueMsixVector(&t, 0, 2);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestMsixConfigDisableMismatchFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceMsixConfigMismatch = TRUE;
	dev.MbForcedMsixConfigMismatch = 0;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetConfigMsixVector(&t, VIRTIO_PCI_MSI_NO_VECTOR);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueMsixVectorDisableMismatchFails(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbForceQueueMsixVectorMismatch = TRUE;
	dev.MbForcedQueueMsixVectorMismatch = 0;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetQueueMsixVector(&t, 0, VIRTIO_PCI_MSI_NO_VECTOR);
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestNotifyHasPreBarrier(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	/*
	 * Pre-notify barrier regression test:
	 *
	 * Arm a one-shot hook that overwrites the notify register on the next
	 * MemoryBarrier() callback.
	 *
	 * - Old implementation: only does a post-doorbell barrier, so the hook runs
	 *   after the doorbell write and overwrites it (fail).
	 * - Fixed implementation: does a pre-doorbell barrier, so the hook runs
	 *   before the doorbell write and is overwritten by it (pass).
	 */
	*(UINT16 *)(dev.Bar0 + 0x1000) = 0xFFFFu;
	dev.MbPoisonNotifyOnNextMb = TRUE;
	dev.MbPoisonNotifyBar0Off = 0x1000u;
	dev.MbPoisonNotifyValue = 0xFFFFu;

	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_SUCCESS);
	assert(*(UINT16 *)(dev.Bar0 + 0x1000) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestMsixVectorProgrammingSucceeds(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	volatile virtio_pci_common_cfg *common;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	common = (volatile virtio_pci_common_cfg *)(dev.Bar0 + 0x0000);

	st = VirtioPciModernTransportSetConfigMsixVector(&t, 3);
	assert(st == STATUS_SUCCESS);
	assert(common->msix_config == 3);

	st = VirtioPciModernTransportSetQueueMsixVector(&t, 0, 4);
	assert(st == STATUS_SUCCESS);
	assert(common->queue_msix_vector == 4);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueMsixVectorRejectInvalidQueue(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	/* Queue index 1 does not exist in FakeDevInitValid (queue_size==0). */
	st = VirtioPciModernTransportSetQueueMsixVector(&t, 1, VIRTIO_PCI_MSI_NO_VECTOR);
	assert(st == STATUS_NOT_FOUND);

	VirtioPciModernTransportUninit(&t);
}

static void TestCompatInitAcceptsRelocatedCaps(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitCompatRelocated(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);
	assert(t.CommonCfg == (volatile virtio_pci_common_cfg *)(dev.Bar0 + 0x0100));
	assert(t.NotifyBase == (volatile UINT8 *)(dev.Bar0 + 0x1200));

	*(UINT16 *)(dev.Bar0 + 0x1200) = 0xFFFFu;
	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_SUCCESS);
	assert(*(UINT16 *)(dev.Bar0 + 0x1200) == 0);

	VirtioPciModernTransportUninit(&t);
}

static void TestCompatInitAccepts32BitBar0Mmio(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	/* Memory BAR, but 32-bit type (bits [2:1]=0b00). */
	WriteLe32(&dev.Cfg[PCI_BAR0_OFF], 0x10000000u);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);
	VirtioPciModernTransportUninit(&t);
}

static void TestQueueSetupRejectUnalignedOrInvalidQueue(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;
	const UINT64 desc_pa = 0x1122334455667700ull;
	const UINT64 avail_pa = 0x1122334455668800ull;
	const UINT64 used_pa = 0x1122334455669900ull;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	/* Unaligned desc (must be 16-byte aligned). */
	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa + 1, avail_pa, used_pa);
	assert(st == STATUS_INVALID_PARAMETER);

	/* Unaligned avail (must be 2-byte aligned). */
	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa, avail_pa + 1, used_pa);
	assert(st == STATUS_INVALID_PARAMETER);

	/* Unaligned used (must be 4-byte aligned). */
	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa, avail_pa, used_pa + 2);
	assert(st == STATUS_INVALID_PARAMETER);

	/* Invalid queue index -> queue_size==0 -> NOT_FOUND. */
	st = VirtioPciModernTransportSetupQueue(&t, 1, desc_pa, avail_pa, used_pa);
	assert(st == STATUS_NOT_FOUND);

	VirtioPciModernTransportUninit(&t);
}

static void TestQueueSetupRejectNotifyOffOutOfRangeCompat(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;
	const UINT64 desc_pa = 0x1122334455667700ull;
	const UINT64 avail_pa = 0x1122334455668800ull;
	const UINT64 used_pa = 0x1122334455669900ull;

	FakeDevInitValid(&dev);
	dev.QueueNotifyOff[0] = 100;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportSetupQueue(&t, 0, desc_pa, avail_pa, used_pa);
	assert(st == STATUS_INVALID_PARAMETER);

	VirtioPciModernTransportUninit(&t);
}

static void TestNotifyRejectInvalidQueue(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportNotifyQueue(&t, 1);
	assert(st == STATUS_NOT_FOUND);

	VirtioPciModernTransportUninit(&t);
}

static void TestNotifyRejectInvalidQueueCompat(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportNotifyQueue(&t, 1);
	assert(st == STATUS_NOT_FOUND);

	VirtioPciModernTransportUninit(&t);
}

static void TestNotifyRejectNotifyOffTooLargeCompat(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.QueueNotifyOff[0] = 100;
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_COMPAT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	*(UINT16 *)(dev.Bar0 + 0x1000) = 0xBEEFu;
	st = VirtioPciModernTransportNotifyQueue(&t, 0);
	assert(st == STATUS_INVALID_PARAMETER);
	assert(*(UINT16 *)(dev.Bar0 + 0x1000) == 0xBEEFu);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigReadStable(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT8 buf[4];
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.Bar0[0x3000] = 0xAA;
	dev.Bar0[0x3001] = 0xBB;
	dev.Bar0[0x3002] = 0xCC;
	dev.Bar0[0x3003] = 0xDD;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	memset(buf, 0, sizeof(buf));
	st = VirtioPciModernTransportReadDeviceConfig(&t, 0, buf, sizeof(buf));
	assert(st == STATUS_SUCCESS);
	assert(buf[0] == 0xAA);
	assert(buf[1] == 0xBB);
	assert(buf[2] == 0xCC);
	assert(buf[3] == 0xDD);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigZeroLengthIsNoop(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	os = GetOs(&dev);

	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	/* Zero-length access should succeed even with a NULL buffer. */
	st = VirtioPciModernTransportReadDeviceConfig(&t, 0, NULL, 0);
	assert(st == STATUS_SUCCESS);
	st = VirtioPciModernTransportWriteDeviceConfig(&t, 0, NULL, 0);
	assert(st == STATUS_SUCCESS);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigReadRetriesAndGetsLatest(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT8 buf[4];
	NTSTATUS st;

	FakeDevInitValid(&dev);
	memset(dev.Bar0 + 0x3000, 0x11, sizeof(buf));

	dev.MbBumpConfigGenRemaining = 1;
	dev.MbFillDeviceCfgOnBump = TRUE;
	dev.MbFillDeviceCfgOffset = 0;
	dev.MbFillDeviceCfgLength = sizeof(buf);
	dev.MbFillDeviceCfgValue = 0x22;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	memset(buf, 0, sizeof(buf));
	st = VirtioPciModernTransportReadDeviceConfig(&t, 0, buf, sizeof(buf));
	assert(st == STATUS_SUCCESS);
	assert(buf[0] == 0x22);
	assert(buf[1] == 0x22);
	assert(buf[2] == 0x22);
	assert(buf[3] == 0x22);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigReadFailsWhenGenerationNeverStabilizes(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT8 buf[4];
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbBumpConfigGenRemaining = 100;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	memset(buf, 0, sizeof(buf));
	st = VirtioPciModernTransportReadDeviceConfig(&t, 0, buf, sizeof(buf));
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigWriteRetriesAndSucceeds(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	const UINT8 in[3] = { 0x11, 0x22, 0x33 };
	NTSTATUS st;

	FakeDevInitValid(&dev);
	memset(dev.Bar0 + 0x3000, 0, 16);
	dev.MbBumpConfigGenRemaining = 1;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportWriteDeviceConfig(&t, 1, in, sizeof(in));
	assert(st == STATUS_SUCCESS);
	assert(dev.Bar0[0x3001] == 0x11);
	assert(dev.Bar0[0x3002] == 0x22);
	assert(dev.Bar0[0x3003] == 0x33);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigWriteFailsWhenGenerationNeverStabilizes(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	const UINT8 in[3] = { 0x11, 0x22, 0x33 };
	NTSTATUS st;

	FakeDevInitValid(&dev);
	dev.MbBumpConfigGenRemaining = 100;

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, sizeof(dev.Bar0));
	assert(st == STATUS_SUCCESS);

	st = VirtioPciModernTransportWriteDeviceConfig(&t, 1, in, sizeof(in));
	assert(st == STATUS_IO_DEVICE_ERROR);

	VirtioPciModernTransportUninit(&t);
}

static void TestDeviceConfigBoundsClampedToMappedBar0(void)
{
	FAKE_DEV dev;
	VIRTIO_PCI_MODERN_OS_INTERFACE os;
	VIRTIO_PCI_MODERN_TRANSPORT t;
	UINT8 buf;
	UINT8 in;
	NTSTATUS st;

	FakeDevInitValid(&dev);
	/*
	 * Inflate the DEVICE_CFG cap length so it extends beyond the strict-mapped
	 * BAR0 window (0x4000). The transport must not allow out-of-bounds accesses
	 * just because the cap length is large.
	 */
	WriteLe32(&dev.Cfg[0x74 + 12], 0x2000);

	os = GetOs(&dev);
	st = VirtioPciModernTransportInit(&t, &os, VIRTIO_PCI_MODERN_TRANSPORT_MODE_STRICT, 0x10000000u, 0x8000);
	assert(st == STATUS_SUCCESS);
	assert(t.DeviceCfgLength == 0x1000);

	buf = 0;
	st = VirtioPciModernTransportReadDeviceConfig(&t, 0x1000, &buf, 1);
	assert(st == STATUS_BUFFER_TOO_SMALL);

	in = 0x5Au;
	st = VirtioPciModernTransportWriteDeviceConfig(&t, 0x1000, &in, 1);
	assert(st == STATUS_BUFFER_TOO_SMALL);

	VirtioPciModernTransportUninit(&t);
}

int main(void)
{
	TestInitOk();
	TestRejectInvalidMode();
	TestRejectBadVendor();
	TestRejectNonModernDeviceId();
	TestRejectBadRevision();
	TestRejectBadSubsystemVendor();
	TestRejectBadInterruptPin();
	TestRejectBar0IoSpace();
	TestRejectBar0Not64BitMmio();
	TestRejectBar0AddressMismatch();
	TestRejectMissingStatusCapList();
	TestRejectUnalignedCapPtr();
	TestRejectZeroCapPtr();
	TestRejectCapPtrBelow0x40();
	TestRejectCapNextBelow0x40();
	TestRejectWrongNotifyMultiplier();
	TestRejectWrongOffsets();
	TestCompatAllowsNonContractOffsets();
	TestRejectBar0TooSmall();
	TestRejectUnalignedCapNext();
	TestRejectCapListLoop();
	TestRejectMissingDeviceCfgCap();
	TestRejectMapMmioFailure();
	TestRejectMapMmioNullPointer();
	TestRejectSpinlockCreateFailure();
	TestNegotiateFeaturesOk();
	TestNegotiateFeaturesRejectNoVersion1();
	TestNegotiateFeaturesStrictRejectNoIndirectDesc();
	TestNegotiateFeaturesStrictDoesNotNegotiateEventIdxOffered();
	TestNegotiateFeaturesCompatDoesNotNegotiateEventIdx();
	TestNegotiateFeaturesRejectsRequiredEventIdx();
	TestNegotiateFeaturesStrictDoesNotNegotiatePackedRingOffered();
	TestNegotiateFeaturesCompatDoesNotNegotiatePackedRing();
	TestNegotiateFeaturesRejectsRequiredPackedRing();
	TestQueueSetupAndNotify();
	TestNotifyHasPreBarrier();
	TestStrictNotifyHasPreAndPostMemoryBarrier();
	TestCompatNotifyHasSelectorAndPreAndPostMemoryBarrier();
	TestMsixConfigVectorRefusedFails();
	TestQueueMsixVectorRefusedFails();
	TestMsixConfigVectorMismatchFails();
	TestQueueMsixVectorMismatchFails();
	TestMsixConfigDisableMismatchFails();
	TestQueueMsixVectorDisableMismatchFails();
	TestMsixVectorProgrammingSucceeds();
	TestQueueMsixVectorRejectInvalidQueue();
	TestCompatInitAcceptsRelocatedCaps();
	TestCompatInitAccepts32BitBar0Mmio();
	TestQueueSetupRejectUnalignedOrInvalidQueue();
	TestQueueSetupRejectNotifyOffOutOfRangeCompat();
	TestNotifyRejectInvalidQueue();
	TestNotifyRejectInvalidQueueCompat();
	TestNotifyRejectNotifyOffTooLargeCompat();
	TestDeviceConfigReadStable();
	TestDeviceConfigZeroLengthIsNoop();
	TestDeviceConfigReadRetriesAndGetsLatest();
	TestDeviceConfigReadFailsWhenGenerationNeverStabilizes();
	TestDeviceConfigWriteRetriesAndSucceeds();
	TestDeviceConfigWriteFailsWhenGenerationNeverStabilizes();
	TestDeviceConfigBoundsClampedToMappedBar0();
	printf("virtio_pci_modern_transport_tests: PASS\n");
	return 0;
}
