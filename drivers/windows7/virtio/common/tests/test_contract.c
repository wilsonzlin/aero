/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtio_pci_contract.h"

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

static void cfg_write_le16(uint8_t* cfg, size_t off, uint16_t v)
{
    cfg[off + 0] = (uint8_t)(v & 0xffu);
    cfg[off + 1] = (uint8_t)((v >> 8) & 0xffu);
}

static void build_pci_cfg(uint8_t cfg[0x30], uint16_t vendor, uint16_t device, uint8_t revision)
{
    memset(cfg, 0, 0x30);
    cfg_write_le16(cfg, 0x00, vendor);
    cfg_write_le16(cfg, 0x02, device);
    cfg[0x08] = revision;
}

static void test_validate_contract_v1_bus_slot_success(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/3, /*SlotNumber=*/0x10u, cfg, (ULONG)sizeof(cfg), (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3,
                                                /*SlotNumber=*/0x10u,
                                                allowed,
                                                (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_SUCCESS);
}

static void test_validate_contract_v1_bus_slot_vendor_mismatch(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1234u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/3, /*SlotNumber=*/0x11u, cfg, (ULONG)sizeof(cfg), (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3,
                                                /*SlotNumber=*/0x11u,
                                                allowed,
                                                (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_NOT_SUPPORTED);
}

static void test_validate_contract_v1_bus_slot_revision_mismatch(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x02u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/3, /*SlotNumber=*/0x12u, cfg, (ULONG)sizeof(cfg), (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3,
                                                /*SlotNumber=*/0x12u,
                                                allowed,
                                                (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_NOT_SUPPORTED);
}

static void test_validate_contract_v1_bus_slot_device_not_modern(void)
{
    uint8_t cfg[0x30];
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1af4u, 0x1000u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/3, /*SlotNumber=*/0x13u, cfg, (ULONG)sizeof(cfg), (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3, /*SlotNumber=*/0x13u, /*AllowedDeviceIds=*/NULL, 0);
    assert(st == STATUS_NOT_SUPPORTED);
}

static void test_validate_contract_v1_bus_slot_device_not_allowed(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1af4u, 0x1042u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/3, /*SlotNumber=*/0x14u, cfg, (ULONG)sizeof(cfg), (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3,
                                                /*SlotNumber=*/0x14u,
                                                allowed,
                                                (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_NOT_SUPPORTED);
}

static void test_validate_contract_v1_bus_slot_partial_read(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    NTSTATUS st;

    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(
        /*BusNumber=*/3, /*SlotNumber=*/0x15u, cfg, (ULONG)sizeof(cfg), (ULONG)((sizeof(cfg) / 2u)));

    st = AeroVirtioPciValidateContractV1BusSlot(/*BusNumber=*/3,
                                                /*SlotNumber=*/0x15u,
                                                allowed,
                                                (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_DEVICE_DATA_ERROR);
}

static void test_validate_contract_v1_pdo_property_query_fails(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    DEVICE_OBJECT pdo;
    NTSTATUS st;

    memset(&pdo, 0, sizeof(pdo));
    pdo.BusNumber = 4;
    pdo.Address = 0x20u;
    pdo.BusNumberStatus = STATUS_NOT_FOUND;

    /* Even with a valid PCI config, property query failures should fail early. */
    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/pdo.BusNumber,
                            /*SlotNumber=*/pdo.Address,
                            cfg,
                            (ULONG)sizeof(cfg),
                            (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1Pdo(&pdo, allowed, (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_DEVICE_DATA_ERROR);
}

static void test_validate_contract_v1_pdo_address_query_fails(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    DEVICE_OBJECT pdo;
    NTSTATUS st;

    memset(&pdo, 0, sizeof(pdo));
    pdo.BusNumber = 4;
    pdo.Address = 0x20u;
    pdo.AddressStatus = STATUS_NOT_FOUND;

    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/pdo.BusNumber,
                            /*SlotNumber=*/pdo.Address,
                            cfg,
                            (ULONG)sizeof(cfg),
                            (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1Pdo(&pdo, allowed, (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_DEVICE_DATA_ERROR);
}

static void test_validate_contract_v1_pdo_success(void)
{
    uint8_t cfg[0x30];
    const USHORT allowed[] = {0x1041u};
    DEVICE_OBJECT pdo;
    NTSTATUS st;

    memset(&pdo, 0, sizeof(pdo));
    pdo.BusNumber = 4;
    pdo.Address = 0x21u;

    build_pci_cfg(cfg, 0x1af4u, 0x1041u, 0x01u);
    WdkTestPciReset();
    WdkTestPciSetSlotConfig(/*BusNumber=*/pdo.BusNumber,
                            /*SlotNumber=*/pdo.Address,
                            cfg,
                            (ULONG)sizeof(cfg),
                            (ULONG)sizeof(cfg));

    st = AeroVirtioPciValidateContractV1Pdo(&pdo, allowed, (ULONG)(sizeof(allowed) / sizeof(allowed[0])));
    assert(st == STATUS_SUCCESS);
}

int main(void)
{
    test_validate_contract_v1_bus_slot_success();
    test_validate_contract_v1_bus_slot_vendor_mismatch();
    test_validate_contract_v1_bus_slot_revision_mismatch();
    test_validate_contract_v1_bus_slot_device_not_modern();
    test_validate_contract_v1_bus_slot_device_not_allowed();
    test_validate_contract_v1_bus_slot_partial_read();
    test_validate_contract_v1_pdo_property_query_fails();
    test_validate_contract_v1_pdo_address_query_fails();
    test_validate_contract_v1_pdo_success();

    return 0;
}
