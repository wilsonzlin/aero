#pragma once
/*
 * Shared Virtio PCI (modern) interrupt helper for Windows 7 KMDF drivers.
 *
 * Canonical implementation: `drivers/windows/virtio/kmdf/virtio_pci_interrupts.{c,h}`.
 *
 * Contract notes (Aero Win7 virtio transport):
 * - `virtio_pci_common_cfg` layout MUST match `drivers/win7/virtio/virtio-core/include/virtio_spec.h`.
 * - INTx ISR MUST read the ISR status byte (read-to-ack) and return FALSE for
 *   spurious interrupts (status == 0) to avoid shared-line storms.
 * - MSI/MSI-X ISRs must not depend on ISR status.
 *
 * Concurrency notes (MSI-X multi-vector):
 * - KMDF may execute multiple interrupt DPCs concurrently on different CPUs.
 * - Queue draining is protected by per-queue spinlocks (allocated by this helper).
 * - Any code sequence that writes `common_cfg.queue_select` and then accesses
 *   queue-specific fields MUST be serialized with a global "CommonCfg lock".
 *   This helper accepts an optional CommonCfg spinlock handle and uses it for
 *   MSI-X vector programming and vector clearing.
 */

#include <ntddk.h>
#include <wdf.h>

#include "virtio_spec.h"

#define VIRTIO_PCI_ISR_QUEUE_INTERRUPT  0x01
#define VIRTIO_PCI_ISR_CONFIG_INTERRUPT 0x02

/* Virtio spec sentinel for "no MSI-X vector assigned". */
#define VIRTIO_PCI_MSI_NO_VECTOR ((USHORT)0xFFFF)

typedef virtio_pci_common_cfg VIRTIO_PCI_COMMON_CFG;
typedef Pvirtio_pci_common_cfg PVIRTIO_PCI_COMMON_CFG;

typedef enum _VIRTIO_PCI_INTERRUPT_MODE {
    VirtioPciInterruptModeUnknown = 0,
    VirtioPciInterruptModeIntx,
    VirtioPciInterruptModeMsix,
} VIRTIO_PCI_INTERRUPT_MODE;

typedef VOID EVT_VIRTIO_PCI_CONFIG_CHANGE(_In_ WDFDEVICE Device, _In_opt_ PVOID Context);

typedef VOID EVT_VIRTIO_PCI_DRAIN_QUEUE(
    _In_ WDFDEVICE Device,
    _In_ ULONG QueueIndex,
    _In_opt_ PVOID Context);

typedef struct _VIRTIO_PCI_INTERRUPTS {
    VIRTIO_PCI_INTERRUPT_MODE Mode;

    ULONG QueueCount;
    volatile UCHAR* IsrStatusRegister;

    /* Optional global lock used to serialize `queue_select` sequences. */
    WDFSPINLOCK CommonCfgLock;

    /* Reset/quiesce coordination (DPC paths must bail out while non-zero). */
    volatile LONG ResetInProgress;

    EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange;
    EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue;
    PVOID CallbackContext;

    /*
     * Optional diagnostic counters.
     *
     * When non-NULL, these are incremented from the ISR / DPC paths. Pointers
     * must reference non-paged memory (e.g. a field in the KMDF device context).
     */
    volatile LONG* InterruptCounter;
    volatile LONG* DpcCounter;

    WDFSPINLOCK ConfigLock;
    WDFSPINLOCK* QueueLocks;
    WDFMEMORY QueueLocksMemory;

    union {
        struct {
            WDFINTERRUPT Interrupt;
            volatile LONG PendingIsrStatus;
            volatile LONG SpuriousCount;
        } Intx;

        struct {
            ULONG MessageCount;
            USHORT UsedVectorCount;
            USHORT ConfigVector;
            WDFINTERRUPT* Interrupts;
            WDFMEMORY InterruptsMemory;
            USHORT* QueueVectors;
            WDFMEMORY QueueVectorsMemory;
        } Msix;
    } u;
} VIRTIO_PCI_INTERRUPTS, *PVIRTIO_PCI_INTERRUPTS;

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsPrepareHardware(
    _In_ WDFDEVICE Device,
    _Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated,
    _In_ ULONG QueueCount,
    _In_ volatile UCHAR* IsrStatusRegister,
    _In_opt_ WDFSPINLOCK CommonCfgLock,
    _In_opt_ EVT_VIRTIO_PCI_CONFIG_CHANGE* EvtConfigChange,
    _In_opt_ EVT_VIRTIO_PCI_DRAIN_QUEUE* EvtDrainQueue,
    _In_opt_ PVOID CallbackContext);

VOID VirtioPciInterruptsReleaseHardware(_Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciProgramMsixVectors(
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg,
    _In_opt_ WDFSPINLOCK CommonCfgLock,
    _In_ ULONG QueueCount,
    _In_ USHORT ConfigVector,
    _In_reads_(QueueCount) const USHORT* QueueVectors);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsProgramMsixVectors(
    _In_ const PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg);

/*
 * PASSIVE_LEVEL helper for resetting/reconfiguring a virtio device while MSI-X
 * DPCs may be active.
 *
 * Sequence:
 *   - Set ResetInProgress (DPCs bail out).
 *   - Disable OS interrupt delivery (WdfInterruptDisable).
 *   - If MSI-X: clear device routing (msix_config/queue_msix_vector = 0xFFFF).
 *   - Synchronize with in-flight DPCs (ConfigLock + per-queue locks).
 *
 * Callers must still ensure their device/queue state is otherwise quiesced.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsQuiesce(
    _Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_opt_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg);

/*
 * Re-enables interrupts after VirtioPciInterruptsQuiesce + device reset.
 *
 * For MSI-X this re-programs vectors using the stored Interrupts->u.Msix.*
 * mapping and then enables OS delivery.
 */
_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS VirtioPciInterruptsResume(
    _Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _In_opt_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg);

