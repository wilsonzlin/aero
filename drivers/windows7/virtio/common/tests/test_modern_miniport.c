/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_modern_miniport.h"

#include "wdk_stubs/virtio_pci_modern_mmio_sim.h"

/*
 * Keep assert() active in all build configs (Release may define NDEBUG).
 */
#undef assert
#define assert(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                  \
            abort();                                                                                                     \
        }                                                                                                                \
    } while (0)

enum {
    TEST_BAR0_SIZE = 0x1000,
    TEST_COMMON_CFG_OFF = 0x100,
    TEST_COMMON_CFG_LEN = 0x100,
    TEST_NOTIFY_CFG_OFF = 0x200,
    TEST_NOTIFY_CFG_LEN = 0x100,
    TEST_ISR_CFG_OFF = 0x300,
    TEST_ISR_CFG_LEN = 0x1,
    TEST_DEVICE_CFG_OFF = 0x400,
    TEST_DEVICE_CFG_LEN = 0x40,
    TEST_NOTIFY_OFF_MULT = 4,
};

static void cfg_write_le16(uint8_t* cfg, size_t off, uint16_t v)
{
    cfg[off + 0] = (uint8_t)(v & 0xffu);
    cfg[off + 1] = (uint8_t)((v >> 8) & 0xffu);
}

static void cfg_write_le32(uint8_t* cfg, size_t off, uint32_t v)
{
    cfg[off + 0] = (uint8_t)(v & 0xffu);
    cfg[off + 1] = (uint8_t)((v >> 8) & 0xffu);
    cfg[off + 2] = (uint8_t)((v >> 16) & 0xffu);
    cfg[off + 3] = (uint8_t)((v >> 24) & 0xffu);
}

static void build_test_pci_config(uint8_t cfg[256])
{
    memset(cfg, 0, 256);

    /* BAR0: memory BAR at 0x1000 (flags=0). */
    cfg_write_le32(cfg, 0x10, 0x1000u);

    /* PCI status: capability list present. */
    cfg_write_le16(cfg, 0x06, (uint16_t)(1u << 4));

    /* Capability list head. */
    cfg[0x34] = 0x40;

    /* Common cfg cap @ 0x40. */
    cfg[0x40 + 0] = 0x09; /* VNDR */
    cfg[0x40 + 1] = 0x50; /* next */
    cfg[0x40 + 2] = 16;   /* cap_len */
    cfg[0x40 + 3] = 1;    /* COMMON */
    cfg[0x40 + 4] = 0;    /* bar */
    cfg[0x40 + 5] = 0;    /* id */
    cfg_write_le32(cfg, 0x40 + 8, TEST_COMMON_CFG_OFF);
    cfg_write_le32(cfg, 0x40 + 12, TEST_COMMON_CFG_LEN);

    /* Notify cfg cap @ 0x50. */
    cfg[0x50 + 0] = 0x09;
    cfg[0x50 + 1] = 0x68;
    cfg[0x50 + 2] = 20; /* notify cap is 20 bytes */
    cfg[0x50 + 3] = 2;  /* NOTIFY */
    cfg[0x50 + 4] = 0;
    cfg[0x50 + 5] = 0;
    cfg_write_le32(cfg, 0x50 + 8, TEST_NOTIFY_CFG_OFF);
    cfg_write_le32(cfg, 0x50 + 12, TEST_NOTIFY_CFG_LEN);
    cfg_write_le32(cfg, 0x50 + 16, TEST_NOTIFY_OFF_MULT);

    /* ISR cfg cap @ 0x68. */
    cfg[0x68 + 0] = 0x09;
    cfg[0x68 + 1] = 0x78;
    cfg[0x68 + 2] = 16;
    cfg[0x68 + 3] = 3; /* ISR */
    cfg[0x68 + 4] = 0;
    cfg[0x68 + 5] = 0;
    cfg_write_le32(cfg, 0x68 + 8, TEST_ISR_CFG_OFF);
    cfg_write_le32(cfg, 0x68 + 12, TEST_ISR_CFG_LEN);

    /* Device cfg cap @ 0x78. */
    cfg[0x78 + 0] = 0x09;
    cfg[0x78 + 1] = 0x00;
    cfg[0x78 + 2] = 16;
    cfg[0x78 + 3] = 4; /* DEVICE */
    cfg[0x78 + 4] = 0;
    cfg[0x78 + 5] = 0;
    cfg_write_le32(cfg, 0x78 + 8, TEST_DEVICE_CFG_OFF);
    cfg_write_le32(cfg, 0x78 + 12, TEST_DEVICE_CFG_LEN);
}

static void setup_device(VIRTIO_PCI_DEVICE* dev, uint8_t* bar0, uint8_t pci_cfg[256])
{
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, TEST_BAR0_SIZE);

    st = VirtioPciModernMiniportInit(dev, (PUCHAR)bar0, TEST_BAR0_SIZE, pci_cfg, 256);
    assert(st == STATUS_SUCCESS);
}

static void test_init_ok(void)
{
    uint8_t* bar0;
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    bar0 = (uint8_t*)calloc(1, TEST_BAR0_SIZE);
    assert(bar0 != NULL);

    build_test_pci_config(pci_cfg);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, TEST_BAR0_SIZE, pci_cfg, 256);
    assert(st == STATUS_SUCCESS);

    assert(dev.CommonCfgOffset == TEST_COMMON_CFG_OFF);
    assert(dev.CommonCfgLength == TEST_COMMON_CFG_LEN);
    assert((const void*)dev.CommonCfg == (const void*)(bar0 + TEST_COMMON_CFG_OFF));

    assert(dev.NotifyOffset == TEST_NOTIFY_CFG_OFF);
    assert(dev.NotifyLength == TEST_NOTIFY_CFG_LEN);
    assert((const void*)dev.NotifyBase == (const void*)(bar0 + TEST_NOTIFY_CFG_OFF));
    assert(dev.NotifyOffMultiplier == TEST_NOTIFY_OFF_MULT);

    assert(dev.IsrOffset == TEST_ISR_CFG_OFF);
    assert(dev.IsrLength == TEST_ISR_CFG_LEN);
    assert((const void*)dev.IsrStatus == (const void*)(bar0 + TEST_ISR_CFG_OFF));

    assert(dev.DeviceCfgOffset == TEST_DEVICE_CFG_OFF);
    assert(dev.DeviceCfgLength == TEST_DEVICE_CFG_LEN);
    assert((const void*)dev.DeviceCfg == (const void*)(bar0 + TEST_DEVICE_CFG_OFF));

    free(bar0);
}

static void test_init_invalid_cfg_too_small_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 0x20);
    assert(st == STATUS_BUFFER_TOO_SMALL);
}

static void test_init_invalid_missing_cap_list_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* Clear PCI status cap-list bit. */
    pci_cfg[0x06] = 0;
    pci_cfg[0x07] = 0;

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_notify_multiplier_zero_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* notify_off_multiplier field is at notify cap + 16. */
    cfg_write_le32(pci_cfg, 0x50 + 16, 0);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_common_cfg_not_in_bar0_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* Provide BAR1 address so cap parser accepts bar=1. */
    cfg_write_le32(pci_cfg, 0x14, 0x2000u);

    /* Set common_cfg cap's bar field to 1. */
    pci_cfg[0x40 + 4] = 1;

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_cap_out_of_range_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* Move common cfg window near the end so it overflows BAR0. */
    cfg_write_le32(pci_cfg, 0x40 + 8, TEST_BAR0_SIZE - 0x20);
    cfg_write_le32(pci_cfg, 0x40 + 12, 0x38); /* sizeof(virtio_pci_common_cfg) */

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_notify_len_too_small_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* notify cfg length < sizeof(UINT16) should be rejected. */
    cfg_write_le32(pci_cfg, 0x50 + 12, 1);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_bar0_missing_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* BAR0 address missing => cap parser should fail. */
    cfg_write_le32(pci_cfg, 0x10, 0);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_64bit_bar0_succeeds(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* BAR0 as 64-bit memory BAR @ 0x1000. */
    cfg_write_le32(pci_cfg, 0x10, 0x1004u); /* memType=0x2 (64-bit), base=0x1000 */
    cfg_write_le32(pci_cfg, 0x14, 0);       /* high dword */

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_SUCCESS);
    assert(dev.CommonCfgOffset == TEST_COMMON_CFG_OFF);
    assert(dev.NotifyOffMultiplier == TEST_NOTIFY_OFF_MULT);
}

static void test_init_invalid_missing_device_cfg_cap_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* Make the "device cfg" capability an unknown cfg_type so the parser ignores it. */
    pci_cfg[0x78 + 3] = 0;

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_unaligned_cap_ptr_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /* Capability pointer must be dword-aligned. */
    pci_cfg[0x34] = 0x41;

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_common_cfg_len_too_small_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    cfg_write_le32(pci_cfg, 0x40 + 12, (uint32_t)sizeof(virtio_pci_common_cfg) - 1u);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_init_invalid_64bit_bar_in_last_slot_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    NTSTATUS st;

    build_test_pci_config(pci_cfg);
    memset(bar0, 0, sizeof(bar0));

    /*
     * BAR5 marked as 64-bit memory BAR (memType==0x2) without a following upper
     * dword slot. VirtioPciParseBarsFromConfig should reject this.
     */
    cfg_write_le32(pci_cfg, 0x10 + (5u * 4u), 0x5004u);

    st = VirtioPciModernMiniportInit(&dev, (PUCHAR)bar0, sizeof(bar0), pci_cfg, 256);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);
}

static void test_read_device_features(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint64_t host_features;
    uint64_t got;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Ensure both halves are non-zero so selector semantics are exercised. */
    host_features = 0x11223344ull | (0xaabbccddull << 32);
    host_features |= VIRTIO_F_VERSION_1; /* required by negotiate */
    sim.host_features = host_features;

    VirtioPciModernMmioSimInstall(&sim);

    got = VirtioPciReadDeviceFeatures(&dev);
    assert(got == host_features);

    VirtioPciModernMmioSimUninstall();
}

static void test_status_helpers(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    VirtioPciModernMmioSimInstall(&sim);

    VirtioPciSetStatus(&dev, 0x12);
    assert(VirtioPciGetStatus(&dev) == 0x12);

    VirtioPciAddStatus(&dev, 0x04);
    assert(VirtioPciGetStatus(&dev) == (uint8_t)(0x12 | 0x04));

    VirtioPciFailDevice(&dev);
    assert((VirtioPciGetStatus(&dev) & VIRTIO_STATUS_FAILED) != 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_write_driver_features_direct(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint64_t features;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    VirtioPciModernMmioSimInstall(&sim);

    features = 0x01234567ull | (0x89abcdefull << 32);
    VirtioPciWriteDriverFeatures(&dev, features);
    assert(sim.driver_features == features);

    VirtioPciModernMmioSimUninstall();
}

static void test_negotiate_features_missing_required_fails(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t negotiated;
    uint64_t required;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Device only offers VERSION_1, not the extra required bit. */
    sim.host_features = VIRTIO_F_VERSION_1;
    sim.num_queues = 1;

    VirtioPciModernMmioSimInstall(&sim);

    required = 1ull << 0;
    negotiated = 0xdeadbeefull;
    st = VirtioPciNegotiateFeatures(&dev, required, /*wanted=*/0, &negotiated);
    assert(st == STATUS_NOT_SUPPORTED);
    assert(negotiated == 0);

    /* Status write sequence: reset -> ACK -> ACK|DRIVER -> ...|FAILED. */
    assert(sim.status_write_count >= 4);
    assert(sim.status_writes[0] == 0);
    assert(sim.status_writes[1] == VIRTIO_STATUS_ACKNOWLEDGE);
    assert(sim.status_writes[2] == (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER));
    assert((sim.status_writes[sim.status_write_count - 1] & VIRTIO_STATUS_FAILED) != 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_negotiate_features_requires_version_1(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t negotiated;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Device offers no VERSION_1 bit -> negotiation must fail even if Required=0. */
    sim.host_features = 0;
    sim.num_queues = 1;

    VirtioPciModernMmioSimInstall(&sim);

    negotiated = 0xdeadbeefull;
    st = VirtioPciNegotiateFeatures(&dev, /*Required=*/0, /*Wanted=*/0, &negotiated);
    assert(st == STATUS_NOT_SUPPORTED);
    assert(negotiated == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_negotiate_features_version_1_only_succeeds(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t negotiated;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Only VERSION_1 is advertised; Required=0 should still negotiate VERSION_1. */
    sim.host_features = VIRTIO_F_VERSION_1;
    sim.num_queues = 1;

    VirtioPciModernMmioSimInstall(&sim);

    negotiated = 0;
    st = VirtioPciNegotiateFeatures(&dev, /*Required=*/0, /*Wanted=*/0, &negotiated);
    assert(st == STATUS_SUCCESS);
    assert(negotiated == VIRTIO_F_VERSION_1);
    assert(sim.driver_features == VIRTIO_F_VERSION_1);

    VirtioPciModernMmioSimUninstall();
}

static void test_negotiate_features_success_and_status_sequence(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t negotiated;
    uint64_t required;
    uint64_t wanted;
    uint64_t expected;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    required = 1ull << 0;
    wanted = (1ull << 1) | (1ull << 40);

    sim.host_features = VIRTIO_F_VERSION_1 | required | wanted;
    sim.num_queues = 2;

    VirtioPciModernMmioSimInstall(&sim);

    negotiated = 0;
    st = VirtioPciNegotiateFeatures(&dev, required, wanted, &negotiated);
    assert(st == STATUS_SUCCESS);

    expected = (sim.host_features & wanted) | required | VIRTIO_F_VERSION_1;
    assert(negotiated == expected);
    assert(sim.driver_features == expected);

    assert(sim.status_write_count >= 4);
    assert(sim.status_writes[0] == 0);
    assert(sim.status_writes[1] == VIRTIO_STATUS_ACKNOWLEDGE);
    assert(sim.status_writes[2] == (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER));
    assert(sim.status_writes[3] ==
           (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK));

    /* FEATURES_OK must remain set when read back. */
    assert((VirtioPciGetStatus(&dev) & VIRTIO_STATUS_FEATURES_OK) != 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_negotiate_features_device_rejects_features_ok(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t negotiated;
    uint64_t required;
    uint64_t wanted;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    required = 1ull << 0;
    wanted = 1ull << 1;

    sim.host_features = VIRTIO_F_VERSION_1 | required | wanted;
    sim.num_queues = 1;
    sim.reject_features_ok = 1;

    VirtioPciModernMmioSimInstall(&sim);

    negotiated = 0;
    st = VirtioPciNegotiateFeatures(&dev, required, wanted, &negotiated);
    assert(st == STATUS_NOT_SUPPORTED);
    assert(negotiated == 0);

    /* Driver attempted to set FEATURES_OK but device cleared it before readback. */
    assert(sim.status_write_count == 5);
    assert(sim.status_writes[0] == 0);
    assert(sim.status_writes[1] == VIRTIO_STATUS_ACKNOWLEDGE);
    assert(sim.status_writes[2] == (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER));
    assert((sim.status_writes[3] & VIRTIO_STATUS_FEATURES_OK) != 0);
    assert((VirtioPciGetStatus(&dev) & VIRTIO_STATUS_FEATURES_OK) == 0);
    assert((sim.status_writes[4] & VIRTIO_STATUS_FAILED) != 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_setup_queue_programs_addresses_and_enables(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;
    uint64_t desc;
    uint64_t avail;
    uint64_t used;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[0].queue_size = 8;
    sim.queues[1].queue_size = 16;

    VirtioPciModernMmioSimInstall(&sim);

    desc = 0x1111222233334444ull;
    avail = 0x5555666677778888ull;
    used = 0x9999aaaabbbbccccull;

    st = VirtioPciSetupQueue(&dev, 1, desc, avail, used);
    assert(st == STATUS_SUCCESS);

    assert(sim.queues[1].queue_desc == desc);
    assert(sim.queues[1].queue_avail == avail);
    assert(sim.queues[1].queue_used == used);
    assert(sim.queues[1].queue_enable == 1);

    VirtioPciModernMmioSimUninstall();
}

static void test_setup_queue_enable_readback_failure(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 1;
    sim.queues[0].queue_size = 8;
    sim.ignore_queue_enable_write = 1;

    VirtioPciModernMmioSimInstall(&sim);

    st = VirtioPciSetupQueue(&dev, 0, 0x1111, 0x2222, 0x3333);
    assert(st == STATUS_IO_DEVICE_ERROR);
    assert(sim.queues[0].queue_enable == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_get_num_queues_and_queue_size(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[0].queue_size = 8;
    sim.queues[1].queue_size = 16;

    VirtioPciModernMmioSimInstall(&sim);

    assert(VirtioPciGetNumQueues(&dev) == 2);
    assert(VirtioPciGetQueueSize(&dev, 0) == 8);
    assert(VirtioPciGetQueueSize(&dev, 1) == 16);

    VirtioPciModernMmioSimUninstall();
}

static void test_setup_queue_not_found_when_size_zero(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[1].queue_size = 0;

    VirtioPciModernMmioSimInstall(&sim);

    st = VirtioPciSetupQueue(&dev, 1, 0x1000, 0x2000, 0x3000);
    assert(st == STATUS_NOT_FOUND);

    VirtioPciModernMmioSimUninstall();
}

static void test_disable_queue_clears_enable(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 1;
    sim.queues[0].queue_size = 8;

    VirtioPciModernMmioSimInstall(&sim);

    st = VirtioPciSetupQueue(&dev, 0, 0x1111, 0x2222, 0x3333);
    assert(st == STATUS_SUCCESS);
    assert(sim.queues[0].queue_enable == 1);

    VirtioPciDisableQueue(&dev, 0);
    assert(sim.queues[0].queue_enable == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_read_device_config_success(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint8_t buf[17];
    size_t i;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    /* Fill device-specific config space with a known pattern. */
    for (i = 0; i < TEST_DEVICE_CFG_LEN; i++) {
        bar0[TEST_DEVICE_CFG_OFF + i] = (uint8_t)(0xA0u + i);
    }

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Stable config_generation -> read should succeed. */
    sim.config_generation = 5;
    sim.config_generation_step_on_read = 0;

    VirtioPciModernMmioSimInstall(&sim);

    memset(buf, 0, sizeof(buf));
    st = VirtioPciReadDeviceConfig(&dev, /*Offset=*/1, buf, (ULONG)sizeof(buf));
    assert(st == STATUS_SUCCESS);

    for (i = 0; i < sizeof(buf); i++) {
        assert(buf[i] == bar0[TEST_DEVICE_CFG_OFF + 1 + i]);
    }

    VirtioPciModernMmioSimUninstall();
}

static void test_read_device_config_generation_retry_succeeds(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint8_t buf[8];
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    for (size_t i = 0; i < TEST_DEVICE_CFG_LEN; i++) {
        bar0[TEST_DEVICE_CFG_OFF + i] = (uint8_t)(0x55u ^ (uint8_t)i);
    }

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /*
     * Force a generation mismatch on the first attempt, then stabilize so the
     * retry succeeds.
     *
     * Two generation reads occur per attempt (gen0 + gen1), so step twice.
     */
    sim.config_generation = 0;
    sim.config_generation_step_on_read = 1;
    sim.config_generation_step_reads_remaining = 2;

    VirtioPciModernMmioSimInstall(&sim);

    memset(buf, 0, sizeof(buf));
    st = VirtioPciReadDeviceConfig(&dev, 0, buf, (ULONG)sizeof(buf));
    assert(st == STATUS_SUCCESS);
    for (size_t i = 0; i < sizeof(buf); i++) {
        assert(buf[i] == bar0[TEST_DEVICE_CFG_OFF + i]);
    }

    VirtioPciModernMmioSimUninstall();
}

static void test_read_device_config_invalid_range(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint8_t buf[2];
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    VirtioPciModernMmioSimInstall(&sim);

    st = VirtioPciReadDeviceConfig(&dev, TEST_DEVICE_CFG_LEN - 1, buf, 2);
    assert(st == STATUS_INVALID_PARAMETER);

    VirtioPciModernMmioSimUninstall();
}

static void test_read_device_config_generation_mismatch_times_out(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint8_t buf[8];
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    for (size_t i = 0; i < TEST_DEVICE_CFG_LEN; i++) {
        bar0[TEST_DEVICE_CFG_OFF + i] = (uint8_t)i;
    }

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /*
     * Make config_generation change on every read so gen0 != gen1 every attempt
     * and the helper eventually returns STATUS_IO_TIMEOUT.
     */
    sim.config_generation = 0;
    sim.config_generation_step_on_read = 1;

    VirtioPciModernMmioSimInstall(&sim);

    memset(buf, 0, sizeof(buf));
    st = VirtioPciReadDeviceConfig(&dev, /*Offset=*/0, buf, (ULONG)sizeof(buf));
    assert(st == STATUS_IO_TIMEOUT);

    VirtioPciModernMmioSimUninstall();
}

static void test_get_queue_notify_address_respects_multiplier(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    volatile uint16_t* addr1;
    volatile uint16_t* addr2;
    volatile uint16_t* expected;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[1].queue_size = 16;
    sim.queues[1].queue_notify_off = 7;

    VirtioPciModernMmioSimInstall(&sim);

    addr1 = NULL;
    addr2 = NULL;

    st = VirtioPciGetQueueNotifyAddress(&dev, 1, &addr1);
    assert(st == STATUS_SUCCESS);
    st = VirtioPciGetQueueNotifyAddress(&dev, 1, &addr2);
    assert(st == STATUS_SUCCESS);

    expected = (volatile uint16_t*)((volatile uint8_t*)dev.NotifyBase + (7u * TEST_NOTIFY_OFF_MULT));
    assert(addr1 == expected);
    assert(addr2 == expected);

    /* Notify writes through the calculated address. */
    *(volatile uint16_t*)expected = 0;
    VirtioPciNotifyQueue(&dev, 1);
    assert(*(volatile uint16_t*)expected == 1);

    VirtioPciModernMmioSimUninstall();
}

static void test_get_queue_notify_address_errors(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    volatile uint16_t* addr;
    NTSTATUS st;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[0].queue_size = 0;
    sim.queues[0].queue_notify_off = 0;

    sim.queues[1].queue_size = 8;
    /* Make notify offset overflow the notify window. */
    sim.queues[1].queue_notify_off = (uint16_t)((dev.NotifyLength / TEST_NOTIFY_OFF_MULT) + 1u);

    VirtioPciModernMmioSimInstall(&sim);

    addr = (volatile uint16_t*)0x1;
    st = VirtioPciGetQueueNotifyAddress(&dev, 0, &addr);
    assert(st == STATUS_NOT_FOUND);
    assert(addr == NULL);

    addr = (volatile uint16_t*)0x1;
    st = VirtioPciGetQueueNotifyAddress(&dev, 1, &addr);
    assert(st == STATUS_IO_DEVICE_ERROR);
    assert(addr == NULL);

    VirtioPciModernMmioSimUninstall();
}

static void test_read_isr_read_to_clear(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    uint8_t v;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    VirtioPciModernMmioSimInstall(&sim);

    bar0[TEST_ISR_CFG_OFF] = 0x3;
    v = VirtioPciReadIsr(&dev);
    assert(v == 0x3);
    assert(bar0[TEST_ISR_CFG_OFF] == 0);
    assert(VirtioPciReadIsr(&dev) == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_notify_queue_populates_and_uses_cache(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    volatile UINT16* cache[2];
    volatile uint16_t* addr_a;
    volatile uint16_t* addr_b;

    setup_device(&dev, bar0, pci_cfg);

    cache[0] = NULL;
    cache[1] = NULL;
    dev.QueueNotifyAddrCache = (volatile UINT16**)cache;
    dev.QueueNotifyAddrCacheCount = 2;

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 2;
    sim.queues[1].queue_size = 16;
    sim.queues[1].queue_notify_off = 3;

    VirtioPciModernMmioSimInstall(&sim);

    addr_a = (volatile uint16_t*)((volatile uint8_t*)dev.NotifyBase + (3u * TEST_NOTIFY_OFF_MULT));
    addr_b = (volatile uint16_t*)((volatile uint8_t*)dev.NotifyBase + (4u * TEST_NOTIFY_OFF_MULT));

    *addr_a = 0;
    VirtioPciNotifyQueue(&dev, 1);
    assert(cache[1] == addr_a);
    assert(*addr_a == 1);

    /* Change device state; cached pointer should still be used. */
    sim.queues[1].queue_notify_off = 4;
    *addr_a = 0;
    *addr_b = 0;

    VirtioPciNotifyQueue(&dev, 1);
    assert(*addr_a == 1);
    assert(*addr_b == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_notify_queue_does_not_write_when_queue_missing(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;
    volatile uint16_t* addr;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.num_queues = 1;
    sim.queues[0].queue_size = 0;
    sim.queues[0].queue_notify_off = 0;

    VirtioPciModernMmioSimInstall(&sim);

    addr = (volatile uint16_t*)dev.NotifyBase;
    *addr = 0x1234u;
    VirtioPciNotifyQueue(&dev, 0);
    assert(*addr == 0x1234u);

    VirtioPciModernMmioSimUninstall();
}

static void test_reset_device_times_out_passive_level(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Device never reports status==0 even after the driver writes 0. */
    sim.device_status_read_override = 1;
    sim.device_status_read_override_value = 1;

    VirtioPciModernMmioSimInstall(&sim);

    WdkTestResetDbgPrintExCount();
    WdkTestResetKeDelayExecutionThreadCount();
    WdkTestResetKeStallExecutionProcessorCount();
    WdkTestSetCurrentIrql(PASSIVE_LEVEL);

    VirtioPciResetDevice(&dev);

    assert(sim.status_write_count == 1);
    assert(sim.status_writes[0] == 0);
    assert(WdkTestGetDbgPrintExCount() == 1);
    assert(WdkTestGetKeDelayExecutionThreadCount() != 0);
    assert(WdkTestGetKeStallExecutionProcessorCount() == 0);

    WdkTestSetCurrentIrql(PASSIVE_LEVEL);
    VirtioPciModernMmioSimUninstall();
}

static void test_reset_device_times_out_dispatch_level(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    sim.device_status_read_override = 1;
    sim.device_status_read_override_value = 1;

    VirtioPciModernMmioSimInstall(&sim);

    WdkTestResetDbgPrintExCount();
    WdkTestResetKeDelayExecutionThreadCount();
    WdkTestResetKeStallExecutionProcessorCount();
    WdkTestSetCurrentIrql(DISPATCH_LEVEL);

    VirtioPciResetDevice(&dev);

    assert(sim.status_write_count == 1);
    assert(sim.status_writes[0] == 0);
    assert(WdkTestGetDbgPrintExCount() == 1);
    assert(WdkTestGetKeDelayExecutionThreadCount() == 0);
    assert(WdkTestGetKeStallExecutionProcessorCount() != 0);
    /* Ensure the "high IRQL budget" stays small (should be ~100 stalls today). */
    assert(WdkTestGetKeStallExecutionProcessorCount() <= 200);

    WdkTestSetCurrentIrql(PASSIVE_LEVEL);
    VirtioPciModernMmioSimUninstall();
}

static void test_reset_device_fast_path(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /* Device reports reset as synchronous: device_status reads as 0 immediately. */
    sim.device_status_read_override = 1;
    sim.device_status_read_override_value = 0;

    VirtioPciModernMmioSimInstall(&sim);

    WdkTestResetDbgPrintExCount();
    WdkTestResetKeDelayExecutionThreadCount();
    WdkTestResetKeStallExecutionProcessorCount();
    WdkTestSetCurrentIrql(PASSIVE_LEVEL);

    VirtioPciResetDevice(&dev);

    assert(sim.status_write_count == 1);
    assert(sim.status_writes[0] == 0);
    assert(WdkTestGetDbgPrintExCount() == 0);
    assert(WdkTestGetKeDelayExecutionThreadCount() == 0);
    assert(WdkTestGetKeStallExecutionProcessorCount() == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_reset_device_clears_after_delay_passive_level(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /*
     * Make the device appear "stuck" for the initial readback + one poll
     * iteration, then allow reads to reflect the written status (0) so the loop
     * exits successfully without printing an error.
     */
    sim.device_status_read_override = 1;
    sim.device_status_read_override_value = 1;
    sim.device_status_read_override_reads_remaining = 2;

    VirtioPciModernMmioSimInstall(&sim);

    WdkTestResetDbgPrintExCount();
    WdkTestResetKeDelayExecutionThreadCount();
    WdkTestResetKeStallExecutionProcessorCount();
    WdkTestSetCurrentIrql(PASSIVE_LEVEL);

    VirtioPciResetDevice(&dev);

    assert(sim.status_write_count == 1);
    assert(sim.status_writes[0] == 0);
    assert(WdkTestGetDbgPrintExCount() == 0);
    assert(WdkTestGetKeDelayExecutionThreadCount() == 1);
    assert(WdkTestGetKeStallExecutionProcessorCount() == 0);

    VirtioPciModernMmioSimUninstall();
}

static void test_reset_device_clears_after_stall_dispatch_level(void)
{
    uint8_t bar0[TEST_BAR0_SIZE];
    uint8_t pci_cfg[256];
    VIRTIO_PCI_DEVICE dev;
    VIRTIO_PCI_MODERN_MMIO_SIM sim;

    setup_device(&dev, bar0, pci_cfg);

    VirtioPciModernMmioSimInit(&sim,
                               dev.CommonCfg,
                               (volatile uint8_t*)dev.NotifyBase,
                               dev.NotifyLength,
                               (volatile uint8_t*)dev.IsrStatus,
                               dev.IsrLength,
                               (volatile uint8_t*)dev.DeviceCfg,
                               dev.DeviceCfgLength);

    /*
     * Initial readback is non-zero to force the elevated IRQL path, but
     * subsequent reads reflect the written status (0) so the loop exits after a
     * single stall.
     */
    sim.device_status_read_override = 1;
    sim.device_status_read_override_value = 1;
    sim.device_status_read_override_reads_remaining = 1;

    VirtioPciModernMmioSimInstall(&sim);

    WdkTestResetDbgPrintExCount();
    WdkTestResetKeDelayExecutionThreadCount();
    WdkTestResetKeStallExecutionProcessorCount();
    WdkTestSetCurrentIrql(DISPATCH_LEVEL);

    VirtioPciResetDevice(&dev);

    assert(sim.status_write_count == 1);
    assert(sim.status_writes[0] == 0);
    assert(WdkTestGetDbgPrintExCount() == 0);
    assert(WdkTestGetKeDelayExecutionThreadCount() == 0);
    assert(WdkTestGetKeStallExecutionProcessorCount() == 1);

    WdkTestSetCurrentIrql(PASSIVE_LEVEL);
    VirtioPciModernMmioSimUninstall();
}

int main(void)
{
    test_init_ok();
    test_init_invalid_cfg_too_small_fails();
    test_init_invalid_missing_cap_list_fails();
    test_init_invalid_notify_multiplier_zero_fails();
    test_init_invalid_common_cfg_not_in_bar0_fails();
    test_init_invalid_cap_out_of_range_fails();
    test_init_invalid_notify_len_too_small_fails();
    test_init_invalid_bar0_missing_fails();
    test_init_64bit_bar0_succeeds();
    test_init_invalid_missing_device_cfg_cap_fails();
    test_init_invalid_unaligned_cap_ptr_fails();
    test_init_invalid_common_cfg_len_too_small_fails();
    test_init_invalid_64bit_bar_in_last_slot_fails();
    test_read_device_features();
    test_status_helpers();
    test_write_driver_features_direct();
    test_negotiate_features_missing_required_fails();
    test_negotiate_features_requires_version_1();
    test_negotiate_features_version_1_only_succeeds();
    test_negotiate_features_success_and_status_sequence();
    test_negotiate_features_device_rejects_features_ok();
    test_setup_queue_programs_addresses_and_enables();
    test_setup_queue_enable_readback_failure();
    test_get_num_queues_and_queue_size();
    test_setup_queue_not_found_when_size_zero();
    test_disable_queue_clears_enable();
    test_read_device_config_success();
    test_read_device_config_generation_retry_succeeds();
    test_read_device_config_invalid_range();
    test_read_device_config_generation_mismatch_times_out();
    test_get_queue_notify_address_respects_multiplier();
    test_get_queue_notify_address_errors();
    test_read_isr_read_to_clear();
    test_notify_queue_populates_and_uses_cache();
    test_notify_queue_does_not_write_when_queue_missing();
    test_reset_device_fast_path();
    test_reset_device_clears_after_delay_passive_level();
    test_reset_device_clears_after_stall_dispatch_level();
    test_reset_device_times_out_passive_level();
    test_reset_device_times_out_dispatch_level();

    printf("virtio_pci_modern_miniport_tests: PASS\n");
    return 0;
}
