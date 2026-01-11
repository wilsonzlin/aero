/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "aero_virtio_pci_modern.h"

/*
 * Keep assertions active in all build configurations.
 *
 * CMake Release builds define NDEBUG, which would normally compile out
 * assert() checks. These tests are meant to be run under Release in CI, so we
 * override assert() to always evaluate.
 */
#undef assert
#define assert(expr)                                                                                                   \
    do {                                                                                                               \
        if (!(expr)) {                                                                                                 \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                \
            abort();                                                                                                   \
        }                                                                                                              \
    } while (0)

#define FAKE_MAX_QUEUES 8

typedef struct fake_queue_state {
    uint16_t size;
    uint16_t notify_off;
    uint16_t enable;
    uint64_t desc;
    uint64_t avail;
    uint64_t used;
} fake_queue_state_t;

typedef struct fake_device {
    uint8_t bar0[AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE];

    uint64_t device_features;
    uint64_t driver_features;

    uint32_t device_feature_select;
    uint32_t driver_feature_select;
    uint16_t queue_select;

    uint16_t num_queues;
    fake_queue_state_t queues[FAKE_MAX_QUEUES];

    uint8_t status;
    uint8_t config_generation;
    uint8_t isr_status;

    int flip_generation_on_device_cfg_read;
    uint8_t device_cfg_fill_after_flip;
} fake_device_t;

static fake_device_t g_dev;

static uint16_t le16_read(const uint8_t *p)
{
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static void le16_write(uint8_t *p, uint16_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)(v >> 8);
}

static uint32_t le32_read(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void le32_write(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v & 0xFF);
    p[1] = (uint8_t)((v >> 8) & 0xFF);
    p[2] = (uint8_t)((v >> 16) & 0xFF);
    p[3] = (uint8_t)(v >> 24);
}

static size_t addr_to_off(const volatile void *addr)
{
    const volatile uint8_t *p = (const volatile uint8_t *)addr;
    const volatile uint8_t *base = (const volatile uint8_t *)&g_dev.bar0[0];
    return (size_t)(p - base);
}

static void fake_reset_state(void)
{
    memset(&g_dev, 0, sizeof(g_dev));
    memset(&g_dev.bar0[0], 0, sizeof(g_dev.bar0));
}

static void fake_fill_device_cfg(uint8_t value)
{
    memset(&g_dev.bar0[AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET], value, AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_SIZE);
}

static void maybe_flip_generation_on_cfg_read(size_t off)
{
    if (!g_dev.flip_generation_on_device_cfg_read) {
        return;
    }

    if (off < AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET ||
        off >= (AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET + AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_SIZE)) {
        return;
    }

    g_dev.flip_generation_on_device_cfg_read = 0;
    g_dev.config_generation++;
    fake_fill_device_cfg(g_dev.device_cfg_fill_after_flip);
}

/*
 * These functions are linked into the library when built with
 * AERO_VIRTIO_PCI_MODERN_USE_TEST_MMIO (see aero_virtio_pci_modern.c).
 */

UCHAR AeroVirtioPciModernTestRead8(const volatile void *addr)
{
    size_t off = addr_to_off(addr);

    maybe_flip_generation_on_cfg_read(off);

    if (off == 0x14) { /* device_status */
        return g_dev.status;
    }
    if (off == 0x15) { /* config_generation */
        return g_dev.config_generation;
    }
    if (off == AERO_VIRTIO_PCI_MODERN_ISR_OFFSET) {
        uint8_t v = g_dev.isr_status;
        g_dev.isr_status = 0;
        return v;
    }

    assert(off < sizeof(g_dev.bar0));
    return g_dev.bar0[off];
}

USHORT AeroVirtioPciModernTestRead16(const volatile void *addr)
{
    size_t off = addr_to_off(addr);

    maybe_flip_generation_on_cfg_read(off);

    if (off == 0x12) { /* num_queues */
        return g_dev.num_queues;
    }
    if (off == 0x18) { /* queue_size */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return g_dev.queues[g_dev.queue_select].size;
    }
    if (off == 0x1E) { /* queue_notify_off */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return g_dev.queues[g_dev.queue_select].notify_off;
    }
    if (off == 0x1C) { /* queue_enable */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return g_dev.queues[g_dev.queue_select].enable;
    }

    assert(off + 1 < sizeof(g_dev.bar0));
    return le16_read(&g_dev.bar0[off]);
}

ULONG AeroVirtioPciModernTestRead32(const volatile void *addr)
{
    size_t off = addr_to_off(addr);

    maybe_flip_generation_on_cfg_read(off);

    if (off == 0x04) { /* device_feature */
        if (g_dev.device_feature_select == 0) {
            return (uint32_t)(g_dev.device_features & 0xFFFFFFFFull);
        }
        if (g_dev.device_feature_select == 1) {
            return (uint32_t)(g_dev.device_features >> 32);
        }
        return 0;
    }

    if (off == 0x0C) { /* driver_feature */
        if (g_dev.driver_feature_select == 0) {
            return (uint32_t)(g_dev.driver_features & 0xFFFFFFFFull);
        }
        if (g_dev.driver_feature_select == 1) {
            return (uint32_t)(g_dev.driver_features >> 32);
        }
        return 0;
    }

    if (off == 0x20) { /* queue_desc_lo */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].desc & 0xFFFFFFFFull);
    }
    if (off == 0x24) { /* queue_desc_hi */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].desc >> 32);
    }
    if (off == 0x28) { /* queue_avail_lo */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].avail & 0xFFFFFFFFull);
    }
    if (off == 0x2C) { /* queue_avail_hi */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].avail >> 32);
    }
    if (off == 0x30) { /* queue_used_lo */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].used & 0xFFFFFFFFull);
    }
    if (off == 0x34) { /* queue_used_hi */
        if (g_dev.queue_select >= g_dev.num_queues) {
            return 0;
        }
        return (uint32_t)(g_dev.queues[g_dev.queue_select].used >> 32);
    }

    assert(off + 3 < sizeof(g_dev.bar0));
    return le32_read(&g_dev.bar0[off]);
}

void AeroVirtioPciModernTestWrite8(volatile void *addr, UCHAR value)
{
    size_t off = addr_to_off(addr);

    assert(off < sizeof(g_dev.bar0));

    if (off == 0x14) { /* device_status */
        if (value == 0) {
            /* Reset device state (minimal model needed for tests). */
            g_dev.status = 0;
            g_dev.driver_features = 0;
            g_dev.device_feature_select = 0;
            g_dev.driver_feature_select = 0;
            g_dev.queue_select = 0;
            g_dev.isr_status = 0;
            for (size_t i = 0; i < FAKE_MAX_QUEUES; i++) {
                g_dev.queues[i].enable = 0;
                g_dev.queues[i].desc = 0;
                g_dev.queues[i].avail = 0;
                g_dev.queues[i].used = 0;
            }
        } else {
            g_dev.status = value;

            /*
             * Model FEATURES_OK acceptance: if the driver accepted any feature
             * not offered by the device, clear FEATURES_OK.
             */
            if ((value & VIRTIO_STATUS_FEATURES_OK) != 0) {
                if ((g_dev.driver_features & ~g_dev.device_features) != 0) {
                    g_dev.status &= (uint8_t)~VIRTIO_STATUS_FEATURES_OK;
                }
            }
        }
        return;
    }

    g_dev.bar0[off] = value;
}

void AeroVirtioPciModernTestWrite16(volatile void *addr, USHORT value)
{
    size_t off = addr_to_off(addr);

    assert(off + 1 < sizeof(g_dev.bar0));

    if (off == 0x16) { /* queue_select */
        g_dev.queue_select = value;
        return;
    }
    if (off == 0x1C) { /* queue_enable */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].enable = value;
        }
        return;
    }

    le16_write(&g_dev.bar0[off], value);
}

void AeroVirtioPciModernTestWrite32(volatile void *addr, ULONG value)
{
    size_t off = addr_to_off(addr);

    assert(off + 3 < sizeof(g_dev.bar0));

    if (off == 0x00) { /* device_feature_select */
        g_dev.device_feature_select = value;
        return;
    }
    if (off == 0x08) { /* driver_feature_select */
        g_dev.driver_feature_select = value;
        return;
    }
    if (off == 0x0C) { /* driver_feature */
        if (g_dev.driver_feature_select == 0) {
            g_dev.driver_features &= 0xFFFFFFFF00000000ull;
            g_dev.driver_features |= (uint64_t)value;
        } else if (g_dev.driver_feature_select == 1) {
            g_dev.driver_features &= 0x00000000FFFFFFFFull;
            g_dev.driver_features |= ((uint64_t)value << 32);
        }
        return;
    }

    if (off == 0x20) { /* queue_desc_lo */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].desc &= 0xFFFFFFFF00000000ull;
            g_dev.queues[g_dev.queue_select].desc |= (uint64_t)value;
        }
        return;
    }
    if (off == 0x24) { /* queue_desc_hi */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].desc &= 0x00000000FFFFFFFFull;
            g_dev.queues[g_dev.queue_select].desc |= ((uint64_t)value << 32);
        }
        return;
    }
    if (off == 0x28) { /* queue_avail_lo */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].avail &= 0xFFFFFFFF00000000ull;
            g_dev.queues[g_dev.queue_select].avail |= (uint64_t)value;
        }
        return;
    }
    if (off == 0x2C) { /* queue_avail_hi */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].avail &= 0x00000000FFFFFFFFull;
            g_dev.queues[g_dev.queue_select].avail |= ((uint64_t)value << 32);
        }
        return;
    }
    if (off == 0x30) { /* queue_used_lo */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].used &= 0xFFFFFFFF00000000ull;
            g_dev.queues[g_dev.queue_select].used |= (uint64_t)value;
        }
        return;
    }
    if (off == 0x34) { /* queue_used_hi */
        if (g_dev.queue_select < g_dev.num_queues) {
            g_dev.queues[g_dev.queue_select].used &= 0x00000000FFFFFFFFull;
            g_dev.queues[g_dev.queue_select].used |= ((uint64_t)value << 32);
        }
        return;
    }

    le32_write(&g_dev.bar0[off], value);
}

void AeroVirtioPciModernTestBarrier(void) {}

void AeroVirtioPciModernTestStallExecutionProcessor(ULONG microseconds)
{
    (void)microseconds;
}

static void test_init_from_bar0(void)
{
    AERO_VIRTIO_PCI_MODERN_DEVICE dev;
    NTSTATUS status;

    fake_reset_state();

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);
    assert(dev.CommonCfg == (volatile virtio_pci_common_cfg *)(g_dev.bar0 + AERO_VIRTIO_PCI_MODERN_COMMON_CFG_OFFSET));
    assert(dev.NotifyBase == (volatile UCHAR *)(g_dev.bar0 + AERO_VIRTIO_PCI_MODERN_NOTIFY_OFFSET));
    assert(dev.IsrStatus == (volatile UCHAR *)(g_dev.bar0 + AERO_VIRTIO_PCI_MODERN_ISR_OFFSET));
    assert(dev.DeviceCfg == (volatile UCHAR *)(g_dev.bar0 + AERO_VIRTIO_PCI_MODERN_DEVICE_CFG_OFFSET));
    assert(dev.NotifyOffMultiplier == AERO_VIRTIO_PCI_MODERN_NOTIFY_OFF_MULTIPLIER);

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE - 1);
    assert(status == STATUS_INVALID_PARAMETER);
}

static void test_feature_negotiation(void)
{
    AERO_VIRTIO_PCI_MODERN_DEVICE dev;
    NTSTATUS status;
    uint64_t negotiated;

    fake_reset_state();
    g_dev.device_features = VIRTIO_F_VERSION_1 | (1ull << 5) | (1ull << 10);
    g_dev.num_queues = 1;
    g_dev.queues[0].size = 8;
    g_dev.queues[0].notify_off = 0;

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);

    negotiated = 0;
    status = AeroVirtioNegotiateFeatures(&dev, /*required*/ (1ull << 5), /*wanted*/ (1ull << 10), &negotiated);
    assert(status == STATUS_SUCCESS);
    assert(negotiated == (VIRTIO_F_VERSION_1 | (1ull << 5) | (1ull << 10)));
    assert((g_dev.status & VIRTIO_STATUS_FEATURES_OK) != 0);

    /* Required feature missing -> negotiation must fail and set FAILED. */
    fake_reset_state();
    g_dev.device_features = VIRTIO_F_VERSION_1;

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);

    negotiated = 0;
    status = AeroVirtioNegotiateFeatures(&dev, /*required*/ (1ull << 5), /*wanted*/ 0, &negotiated);
    assert(status == STATUS_NOT_SUPPORTED);
    assert((g_dev.status & VIRTIO_STATUS_FAILED) != 0);
}

static void test_queue_setup_and_notify(void)
{
    AERO_VIRTIO_PCI_MODERN_DEVICE dev;
    NTSTATUS status;
    uint16_t q_size;
    uint16_t q_notify_off;
    uint16_t doorbell_value;

    fake_reset_state();
    g_dev.device_features = VIRTIO_F_VERSION_1;
    g_dev.num_queues = 2;
    g_dev.queues[0].size = 8;
    g_dev.queues[0].notify_off = 0;
    g_dev.queues[1].size = 16;
    g_dev.queues[1].notify_off = 1;

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);

    assert(AeroVirtioGetNumQueues(&dev) == 2);

    q_size = 0;
    q_notify_off = 0;
    status = AeroVirtioQueryQueue(&dev, 1, &q_size, &q_notify_off);
    assert(status == STATUS_SUCCESS);
    assert(q_size == 16);
    assert(q_notify_off == 1);

    status = AeroVirtioSetupQueue(&dev,
                                 1,
                                 /*desc*/ 0x1122334455667788ull,
                                 /*avail*/ 0x0102030405060708ull,
                                 /*used*/ 0x8877665544332211ull);
    assert(status == STATUS_SUCCESS);
    assert(g_dev.queues[1].enable == 1);
    assert(g_dev.queues[1].desc == 0x1122334455667788ull);
    assert(g_dev.queues[1].avail == 0x0102030405060708ull);
    assert(g_dev.queues[1].used == 0x8877665544332211ull);

    AeroVirtioNotifyQueue(&dev, 1, q_notify_off);
    doorbell_value = le16_read(&g_dev.bar0[AERO_VIRTIO_PCI_MODERN_NOTIFY_OFFSET + 4]);
    assert(doorbell_value == 1);
}

static void test_isr_read_to_ack(void)
{
    AERO_VIRTIO_PCI_MODERN_DEVICE dev;
    NTSTATUS status;
    uint8_t v;

    fake_reset_state();

    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);

    g_dev.isr_status = (uint8_t)(VIRTIO_PCI_ISR_QUEUE | VIRTIO_PCI_ISR_CONFIG);

    v = AeroVirtioReadIsr(&dev);
    assert(v == (uint8_t)(VIRTIO_PCI_ISR_QUEUE | VIRTIO_PCI_ISR_CONFIG));

    v = AeroVirtioReadIsr(&dev);
    assert(v == 0);
}

static void test_device_cfg_generation_retry(void)
{
    AERO_VIRTIO_PCI_MODERN_DEVICE dev;
    NTSTATUS status;
    uint8_t buf[16];

    fake_reset_state();
    status = AeroVirtioPciModernInitFromBar0(&dev, g_dev.bar0, AERO_VIRTIO_PCI_MODERN_BAR0_REQUIRED_SIZE);
    assert(status == STATUS_SUCCESS);

    g_dev.config_generation = 1;
    fake_fill_device_cfg(0x11);

    g_dev.flip_generation_on_device_cfg_read = 1;
    g_dev.device_cfg_fill_after_flip = 0x22;

    memset(buf, 0, sizeof(buf));
    status = AeroVirtioReadDeviceConfig(&dev, 0, buf, (ULONG)sizeof(buf));
    assert(status == STATUS_SUCCESS);
    assert(g_dev.config_generation == 2);
    for (size_t i = 0; i < sizeof(buf); i++) {
        assert(buf[i] == 0x22);
    }
}

int main(void)
{
    test_init_from_bar0();
    test_feature_negotiation();
    test_queue_setup_and_notify();
    test_isr_read_to_ack();
    test_device_cfg_generation_retry();
    return 0;
}
