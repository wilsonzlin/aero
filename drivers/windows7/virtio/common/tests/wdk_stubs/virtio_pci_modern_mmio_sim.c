/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include "virtio_pci_modern_mmio_sim.h"

#include <string.h>

static VIRTIO_PCI_MODERN_MMIO_SIM* g_sim = NULL;

static ULONGLONG mmio_load(const volatile VOID* p, size_t width)
{
    const volatile uint8_t* b;
    ULONGLONG v;
    size_t i;

    b = (const volatile uint8_t*)p;
    v = 0;

    for (i = 0; i < width; i++) {
        v |= ((ULONGLONG)b[i]) << (i * 8u);
    }

    return v;
}

static VOID mmio_store(volatile VOID* p, size_t width, ULONGLONG v)
{
    volatile uint8_t* b;
    size_t i;

    b = (volatile uint8_t*)p;

    for (i = 0; i < width; i++) {
        b[i] = (uint8_t)((v >> (i * 8u)) & 0xFFu);
    }
}

static BOOLEAN virtio_modern_mmio_read(const volatile VOID* Register, size_t Width, ULONGLONG* ValueOut)
{
    const volatile uint8_t* reg_u8;

    if (g_sim == NULL || Register == NULL || ValueOut == NULL) {
        return FALSE;
    }

    reg_u8 = (const volatile uint8_t*)Register;

    /* Common config. */
    if (g_sim->common_cfg != NULL) {
        const volatile uint8_t* base;
        size_t len;

        base = (const volatile uint8_t*)g_sim->common_cfg;
        len = sizeof(*g_sim->common_cfg);

        if (reg_u8 >= base && reg_u8 + Width <= base + len) {
            size_t off;

            off = (size_t)((uintptr_t)reg_u8 - (uintptr_t)base);

            switch (off) {
                case 0x00: /* device_feature_select */
                    if (Width == 4) {
                        *ValueOut = (ULONGLONG)g_sim->device_feature_select;
                        return TRUE;
                    }
                    break;
                case 0x04: /* device_feature */
                    if (Width == 4) {
                        uint32_t v32;
                        v32 = (g_sim->device_feature_select == 0) ? (uint32_t)(g_sim->host_features & 0xFFFFFFFFull)
                                                                  : (uint32_t)(g_sim->host_features >> 32);
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    }
                    break;
                case 0x08: /* driver_feature_select */
                    if (Width == 4) {
                        *ValueOut = (ULONGLONG)g_sim->driver_feature_select;
                        return TRUE;
                    }
                    break;
                case 0x0C: /* driver_feature */
                    if (Width == 4) {
                        uint32_t v32;
                        v32 = (g_sim->driver_feature_select == 0) ? (uint32_t)(g_sim->driver_features & 0xFFFFFFFFull)
                                                                  : (uint32_t)(g_sim->driver_features >> 32);
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    }
                    break;
                case 0x12: /* num_queues */
                    if (Width == 2) {
                        *ValueOut = (ULONGLONG)g_sim->num_queues;
                        return TRUE;
                    }
                    break;
                case 0x14: /* device_status */
                    if (Width == 1) {
                        if (g_sim->device_status_read_override != 0) {
                            *ValueOut = (ULONGLONG)g_sim->device_status_read_override_value;
                            if (g_sim->device_status_read_override_reads_remaining != 0) {
                                g_sim->device_status_read_override_reads_remaining--;
                                if (g_sim->device_status_read_override_reads_remaining == 0) {
                                    g_sim->device_status_read_override = 0;
                                }
                            }
                        } else {
                            *ValueOut = (ULONGLONG)mmio_load(Register, 1);
                        }
                        return TRUE;
                    }
                    break;
                case 0x15: /* config_generation */
                    if (Width == 1) {
                        uint8_t gen;
                        gen = g_sim->config_generation;
                        if (g_sim->config_generation_step_on_read != 0) {
                            g_sim->config_generation = (uint8_t)(g_sim->config_generation + 1u);
                            if (g_sim->config_generation_step_reads_remaining != 0) {
                                g_sim->config_generation_step_reads_remaining--;
                                if (g_sim->config_generation_step_reads_remaining == 0) {
                                    g_sim->config_generation_step_on_read = 0;
                                }
                            }
                        }
                        /* Keep backing memory consistent for any pass-through users. */
                        mmio_store((volatile VOID*)&g_sim->common_cfg->config_generation, 1, (ULONGLONG)g_sim->config_generation);
                        *ValueOut = (ULONGLONG)gen;
                        return TRUE;
                    }
                    break;
                case 0x16: /* queue_select */
                    if (Width == 2) {
                        *ValueOut = (ULONGLONG)g_sim->queue_select;
                        return TRUE;
                    }
                    break;
                case 0x18: /* queue_size */
                    if (Width == 2) {
                        uint16_t qsz = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            qsz = g_sim->queues[g_sim->queue_select].queue_size;
                        }
                        *ValueOut = (ULONGLONG)qsz;
                        return TRUE;
                    }
                    break;
                case 0x1C: /* queue_enable */
                    if (Width == 2) {
                        uint16_t en = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            en = g_sim->queues[g_sim->queue_select].queue_enable;
                        }
                        *ValueOut = (ULONGLONG)en;
                        return TRUE;
                    }
                    break;
                case 0x1E: /* queue_notify_off */
                    if (Width == 2) {
                        uint16_t noff = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            noff = g_sim->queues[g_sim->queue_select].queue_notify_off;
                        }
                        *ValueOut = (ULONGLONG)noff;
                        return TRUE;
                    }
                    break;
                case 0x20: /* queue_desc (lo/hi/64) */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_desc & 0xFFFFFFFFull);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    } else if (Width == 8) {
                        uint64_t v64 = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v64 = g_sim->queues[g_sim->queue_select].queue_desc;
                        }
                        *ValueOut = (ULONGLONG)v64;
                        return TRUE;
                    }
                    break;
                case 0x24: /* queue_desc_hi */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_desc >> 32);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    }
                    break;
                case 0x28: /* queue_avail */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_avail & 0xFFFFFFFFull);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    } else if (Width == 8) {
                        uint64_t v64 = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v64 = g_sim->queues[g_sim->queue_select].queue_avail;
                        }
                        *ValueOut = (ULONGLONG)v64;
                        return TRUE;
                    }
                    break;
                case 0x2C: /* queue_avail_hi */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_avail >> 32);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    }
                    break;
                case 0x30: /* queue_used */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_used & 0xFFFFFFFFull);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    } else if (Width == 8) {
                        uint64_t v64 = 0;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v64 = g_sim->queues[g_sim->queue_select].queue_used;
                        }
                        *ValueOut = (ULONGLONG)v64;
                        return TRUE;
                    }
                    break;
                case 0x34: /* queue_used_hi */
                    if (Width == 4) {
                        uint32_t v32;
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            v32 = (uint32_t)(g_sim->queues[g_sim->queue_select].queue_used >> 32);
                        } else {
                            v32 = 0;
                        }
                        *ValueOut = (ULONGLONG)v32;
                        return TRUE;
                    }
                    break;
                default:
                    /* Pass-through for non-simulated registers. */
                    *ValueOut = mmio_load(Register, Width);
                    return TRUE;
            }

            /* Unknown width for known offset -> pass-through. */
            *ValueOut = mmio_load(Register, Width);
            return TRUE;
        }
    }

    /* Device config (no special semantics; just avoid READ_REGISTER_UCHAR read-to-clear). */
    if (g_sim->device_cfg != NULL && g_sim->device_cfg_len != 0) {
        const volatile uint8_t* base = (const volatile uint8_t*)g_sim->device_cfg;
        if (reg_u8 >= base && reg_u8 + Width <= base + g_sim->device_cfg_len) {
            *ValueOut = mmio_load(Register, Width);
            return TRUE;
        }
    }

    /* ISR status (read-to-clear). */
    if (g_sim->isr_status != NULL && g_sim->isr_len != 0) {
        const volatile uint8_t* base = (const volatile uint8_t*)g_sim->isr_status;
        if (reg_u8 >= base && reg_u8 + Width <= base + g_sim->isr_len && Width == 1) {
            ULONGLONG v;
            v = mmio_load(Register, 1);
            mmio_store((volatile VOID*)Register, 1, 0);
            *ValueOut = v;
            return TRUE;
        }
    }

    /* Notify region: pass-through. */
    if (g_sim->notify_base != NULL && g_sim->notify_len != 0) {
        const volatile uint8_t* base = (const volatile uint8_t*)g_sim->notify_base;
        if (reg_u8 >= base && reg_u8 + Width <= base + g_sim->notify_len) {
            *ValueOut = mmio_load(Register, Width);
            return TRUE;
        }
    }

    return FALSE;
}

static BOOLEAN virtio_modern_mmio_write(volatile VOID* Register, size_t Width, ULONGLONG Value)
{
    volatile uint8_t* reg_u8;

    if (g_sim == NULL || Register == NULL) {
        return FALSE;
    }

    reg_u8 = (volatile uint8_t*)Register;

    /* Common config. */
    if (g_sim->common_cfg != NULL) {
        volatile uint8_t* base;
        size_t len;

        base = (volatile uint8_t*)g_sim->common_cfg;
        len = sizeof(*g_sim->common_cfg);

        if (reg_u8 >= base && reg_u8 + Width <= base + len) {
            size_t off;

            off = (size_t)((uintptr_t)reg_u8 - (uintptr_t)base);

            if (g_sim->common_cfg_write_count < VIRTIO_PCI_MODERN_MMIO_SIM_MAX_COMMON_CFG_WRITES) {
                g_sim->common_cfg_write_offsets[g_sim->common_cfg_write_count++] = (uint16_t)off;
            }

            switch (off) {
                case 0x00: /* device_feature_select */
                    if (Width == 4) {
                        g_sim->device_feature_select = (uint32_t)Value;
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x08: /* driver_feature_select */
                    if (Width == 4) {
                        g_sim->driver_feature_select = (uint32_t)Value;
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x0C: /* driver_feature */
                    if (Width == 4) {
                        uint64_t mask = (uint64_t)0xFFFFFFFFull;
                        uint64_t lo = (uint64_t)((uint32_t)Value);
                        if (g_sim->driver_feature_select == 0) {
                            g_sim->driver_features = (g_sim->driver_features & (~mask)) | lo;
                        } else {
                            g_sim->driver_features = (g_sim->driver_features & mask) | (lo << 32);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x12: /* num_queues (RO in spec; writable in tests to configure sim) */
                    if (Width == 2) {
                        g_sim->num_queues = (uint16_t)Value;
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x14: /* device_status */
                    if (Width == 1) {
                        uint8_t v8 = (uint8_t)Value;
                        uint8_t stored = v8;

                        if (g_sim->reject_features_ok != 0 && (stored & VIRTIO_STATUS_FEATURES_OK) != 0) {
                            stored = (uint8_t)(stored & (uint8_t)~VIRTIO_STATUS_FEATURES_OK);
                        }
                        if (g_sim->status_write_count < VIRTIO_PCI_MODERN_MMIO_SIM_MAX_STATUS_WRITES) {
                            g_sim->status_writes[g_sim->status_write_count++] = v8;
                        }
                        mmio_store(Register, Width, stored);
                        return TRUE;
                    }
                    break;
                case 0x16: /* queue_select */
                    if (Width == 2) {
                        g_sim->queue_select = (uint16_t)Value;
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x1A: /* queue_msix_vector */
                    if (Width == 2) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            g_sim->queues[g_sim->queue_select].queue_msix_vector = (uint16_t)Value;
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x1C: /* queue_enable */
                    if (Width == 2) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            if (g_sim->ignore_queue_enable_write == 0) {
                                g_sim->queues[g_sim->queue_select].queue_enable = (uint16_t)Value;
                            } else {
                                g_sim->queues[g_sim->queue_select].queue_enable = 0;
                                Value = 0;
                            }
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x20: /* queue_desc lo or 64 */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_desc;
                            g_sim->queues[g_sim->queue_select].queue_desc =
                                (old & 0xFFFFFFFF00000000ull) | (uint64_t)((uint32_t)Value);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    } else if (Width == 8) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            g_sim->queues[g_sim->queue_select].queue_desc = (uint64_t)Value;
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x24: /* queue_desc hi */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_desc;
                            g_sim->queues[g_sim->queue_select].queue_desc =
                                (old & 0x00000000FFFFFFFFull) | ((uint64_t)((uint32_t)Value) << 32);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x28: /* queue_avail lo or 64 */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_avail;
                            g_sim->queues[g_sim->queue_select].queue_avail =
                                (old & 0xFFFFFFFF00000000ull) | (uint64_t)((uint32_t)Value);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    } else if (Width == 8) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            g_sim->queues[g_sim->queue_select].queue_avail = (uint64_t)Value;
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x2C: /* queue_avail hi */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_avail;
                            g_sim->queues[g_sim->queue_select].queue_avail =
                                (old & 0x00000000FFFFFFFFull) | ((uint64_t)((uint32_t)Value) << 32);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x30: /* queue_used lo or 64 */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_used;
                            g_sim->queues[g_sim->queue_select].queue_used =
                                (old & 0xFFFFFFFF00000000ull) | (uint64_t)((uint32_t)Value);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    } else if (Width == 8) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            g_sim->queues[g_sim->queue_select].queue_used = (uint64_t)Value;
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                case 0x34: /* queue_used hi */
                    if (Width == 4) {
                        if (g_sim->queue_select < g_sim->num_queues &&
                            g_sim->queue_select < (uint16_t)VIRTIO_PCI_MODERN_MMIO_SIM_MAX_QUEUES) {
                            uint64_t old = g_sim->queues[g_sim->queue_select].queue_used;
                            g_sim->queues[g_sim->queue_select].queue_used =
                                (old & 0x00000000FFFFFFFFull) | ((uint64_t)((uint32_t)Value) << 32);
                        }
                        mmio_store(Register, Width, Value);
                        return TRUE;
                    }
                    break;
                default:
                    mmio_store(Register, Width, Value);
                    return TRUE;
            }

            mmio_store(Register, Width, Value);
            return TRUE;
        }
    }

    /* Device config / ISR / notify: pass-through (but ISR has no writes). */
    if (g_sim->device_cfg != NULL && g_sim->device_cfg_len != 0) {
        volatile uint8_t* base = (volatile uint8_t*)g_sim->device_cfg;
        if (reg_u8 >= base && reg_u8 + Width <= base + g_sim->device_cfg_len) {
            mmio_store(Register, Width, Value);
            return TRUE;
        }
    }

    if (g_sim->notify_base != NULL && g_sim->notify_len != 0) {
        volatile uint8_t* base = (volatile uint8_t*)g_sim->notify_base;
        if (reg_u8 >= base && reg_u8 + Width <= base + g_sim->notify_len) {
            mmio_store(Register, Width, Value);
            return TRUE;
        }
    }

    return FALSE;
}

void VirtioPciModernMmioSimInit(VIRTIO_PCI_MODERN_MMIO_SIM* sim,
                               volatile virtio_pci_common_cfg* common_cfg,
                               volatile uint8_t* notify_base,
                               size_t notify_len,
                               volatile uint8_t* isr_status,
                               size_t isr_len,
                               volatile uint8_t* device_cfg,
                               size_t device_cfg_len)
{
    if (sim == NULL) {
        return;
    }

    memset(sim, 0, sizeof(*sim));
    sim->common_cfg = common_cfg;
    sim->notify_base = notify_base;
    sim->notify_len = notify_len;
    sim->isr_status = isr_status;
    sim->isr_len = isr_len;
    sim->device_cfg = device_cfg;
    sim->device_cfg_len = device_cfg_len;

    /*
     * Initialise the memory backing the common cfg with a sane baseline so any
     * pass-through reads return something deterministic.
     */
    if (sim->common_cfg != NULL) {
        mmio_store((volatile VOID*)&sim->common_cfg->device_status, 1, 0);
        mmio_store((volatile VOID*)&sim->common_cfg->config_generation, 1, 0);
    }
}

void VirtioPciModernMmioSimInstall(VIRTIO_PCI_MODERN_MMIO_SIM* sim)
{
    g_sim = sim;
    WdkSetMmioHandlers(virtio_modern_mmio_read, virtio_modern_mmio_write);
}

void VirtioPciModernMmioSimUninstall(void)
{
    g_sim = NULL;
    WdkSetMmioHandlers(NULL, NULL);
}
