#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../virtio_pci_modern_transport.h"

#include "../../../../win7/virtio/virtio-core/portable/virtio_pci_cap_parser.h"

enum {
	PCI_STATUS_OFF = 0x06,
	PCI_REVISION_OFF = 0x08,
	PCI_BAR0_OFF = 0x10,
	PCI_CAP_PTR_OFF = 0x34,

	PCI_STATUS_CAP_LIST = 1u << 4,

	BAR0_LEN = 0x4000,
};

typedef struct _FAKE_DEV {
	UINT8 Cfg[256];
	UINT8 Bar0[BAR0_LEN];
} FAKE_DEV;

static void WriteLe16(UINT8 *p, UINT16 v)
{
	p[0] = (UINT8)(v & 0xffu);
	p[1] = (UINT8)((v >> 8) & 0xffu);
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

	/* PCI header */
	WriteLe16(&dev->Cfg[PCI_STATUS_OFF], PCI_STATUS_CAP_LIST);
	dev->Cfg[PCI_REVISION_OFF] = 0x01;
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
	common->queue_size = 8;
	common->queue_notify_off = 0;
	common->device_feature = 1; /* keep selectors trivial for unit tests */
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
	(void)ctx;
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

	VirtioPciModernTransportUninit(&t);
}

static void TestRejectBadRevision(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	dev.Cfg[PCI_REVISION_OFF] = 0x02;

	ExpectInitFail("bad_revision", &dev, VIRTIO_PCI_MODERN_INIT_ERR_UNSUPPORTED_REVISION);
}

static void TestRejectBar0IoSpace(void)
{
	FAKE_DEV dev;

	FakeDevInitValid(&dev);
	/* BAR0 bit0=1 => I/O */
	WriteLe32(&dev.Cfg[PCI_BAR0_OFF], 0xC001u);

	ExpectInitFail("bar0_not_mmio", &dev, VIRTIO_PCI_MODERN_INIT_ERR_BAR0_NOT_MMIO);
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

int main(void)
{
	TestInitOk();
	TestRejectBadRevision();
	TestRejectBar0IoSpace();
	TestRejectMissingStatusCapList();
	TestRejectUnalignedCapPtr();
	TestRejectWrongNotifyMultiplier();
	TestRejectWrongOffsets();
	TestRejectBar0TooSmall();
	TestRejectUnalignedCapNext();
	TestRejectMissingDeviceCfgCap();
	printf("virtio_pci_modern_transport_tests: PASS\n");
	return 0;
}
