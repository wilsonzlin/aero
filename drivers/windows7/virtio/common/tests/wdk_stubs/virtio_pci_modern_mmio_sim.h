/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Tiny virtio-pci modern BAR0 MMIO simulator for host-side unit tests.
 *
 * This is intentionally minimal and only models the semantics required by
 * virtio_pci_modern_miniport.c:
 *  - device_feature_select/device_feature selector behaviour
 *  - driver_feature_select/driver_feature selector behaviour
 *  - queue_select selector behaviour for queue programming
 *  - ISR read-to-clear
 */

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "virtio_pci_modern_miniport.h"

#ifdef __cplusplus
extern "C" {
#endif

#define VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES 16u
#define VIRTIO_PCI_MODERN_MMIO_SIM_MAX_STATUS_WRITES 64u
#define VIRTIO_PCI_MODERN_MMIO_SIM_MAX_COMMON_CFG_WRITES 128u
#define VIRTIO_PCI_MODERN_MMIO_SIM_MAX_COMMON_CFG_READS 256u

typedef struct VIRTIO_PCI_MODERN_MMIO_SIM_QUEUE {
    uint16_t queue_size;
    uint16_t queue_notify_off;
    uint16_t queue_enable;
    uint16_t queue_msix_vector;
    uint64_t queue_desc;
    uint64_t queue_avail;
    uint64_t queue_used;
} VIRTIO_PCI_MODERN_MMIO_SIM_QUEUE;

typedef struct VIRTIO_PCI_MODERN_MMIO_SIM {
    volatile virtio_pci_common_cfg* common_cfg;

    volatile uint8_t* notify_base;
    size_t notify_len;

    volatile uint8_t* isr_status;
    size_t isr_len;

    volatile uint8_t* device_cfg;
    size_t device_cfg_len;

    uint64_t host_features;
    uint64_t driver_features;

    uint32_t device_feature_select;
    uint32_t driver_feature_select;
    uint16_t msix_config;
    uint16_t queue_select;

    uint8_t device_status_read_override;
    uint8_t device_status_read_override_value;
    uint32_t device_status_read_override_reads_remaining; /* 0 = infinite while override enabled */

    uint8_t config_generation;
    uint8_t config_generation_step_on_read;
    uint32_t config_generation_step_reads_remaining; /* 0 = infinite while step_on_read != 0 */
    uint8_t reject_features_ok; /* if set, device clears FEATURES_OK on write */
    uint8_t ignore_queue_enable_write; /* if set, queue_enable writes are ignored (readback stays 0) */

    /*
     * MSI-X vector programming hooks.
     *
     * When the override flags are set, writes of any vector other than
     * VIRTIO_PCI_MSI_NO_VECTOR will be forced to the corresponding override
     * value to simulate devices that refuse MSI-X vector assignments.
     */
    uint8_t msix_config_write_override;
    uint16_t msix_config_write_override_value;
    uint8_t queue_msix_vector_write_override;
    uint16_t queue_msix_vector_write_override_value;

    uint16_t num_queues;
    VIRTIO_PCI_MODERN_MMIO_SIM_QUEUE queues[VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES];

    uint8_t status_writes[VIRTIO_PCI_MODERN_MMIO_SIM_MAX_STATUS_WRITES];
    size_t status_write_count;

    uint16_t common_cfg_read_offsets[VIRTIO_PCI_MODERN_MMIO_SIM_MAX_COMMON_CFG_READS];
    size_t common_cfg_read_count;

    uint16_t common_cfg_write_offsets[VIRTIO_PCI_MODERN_MMIO_SIM_MAX_COMMON_CFG_WRITES];
    size_t common_cfg_write_count;

    /*
     * Selector serialization checks (contract §1.5.0).
     *
     * Virtio-pci modern uses selector registers (e.g. queue_select) that require
     * software-side serialization. The miniport code is expected to guard these
     * accesses with Dev->CommonCfgLock.
     *
     * When enforce_queue_select_lock is set, MMIO accesses to queue_select and
     * per-queue common_cfg registers (offsets 0x16..0x34) will be checked
     * against the provided queue_select_lock. Any access observed while the
     * lock is not held increments queue_select_lock_violation_count.
     */
    const volatile KSPIN_LOCK* queue_select_lock;
    uint8_t enforce_queue_select_lock;
    size_t queue_select_lock_check_count;
    size_t queue_select_lock_violation_count;
} VIRTIO_PCI_MODERN_MMIO_SIM;

void VirtioPciModernMmioSimInit(VIRTIO_PCI_MODERN_MMIO_SIM* sim,
                               volatile virtio_pci_common_cfg* common_cfg,
                               volatile uint8_t* notify_base,
                               size_t notify_len,
                               volatile uint8_t* isr_status,
                               size_t isr_len,
                               volatile uint8_t* device_cfg,
                               size_t device_cfg_len);

void VirtioPciModernMmioSimInstall(VIRTIO_PCI_MODERN_MMIO_SIM* sim);
void VirtioPciModernMmioSimUninstall(void);

#ifdef __cplusplus
}
#endif
