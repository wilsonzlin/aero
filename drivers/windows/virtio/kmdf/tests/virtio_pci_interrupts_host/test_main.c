#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/*
 * Keep assertions active in all build configurations.
 *
 * These host tests run under Release in CI. CMake Release builds define NDEBUG,
 * which would normally compile out assert() checks. Override assert() so test
 * coverage is preserved and side-effectful expressions still execute.
 */
#undef assert
#define assert(expr)                                                                                                       \
    do {                                                                                                                   \
        if (!(expr)) {                                                                                                     \
            fprintf(stderr, "ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                                    \
            abort();                                                                                                        \
        }                                                                                                                  \
    } while (0)

#include "../../virtio_pci_interrupts.h"

/* Instrumentation hooks consumed by our stub ntddk.h READ_REGISTER_UCHAR. */
unsigned int WdfTestReadRegisterUcharCount;
volatile const UCHAR* WdfTestLastReadRegisterUcharAddress;

/* Optional instrumentation hooks for our stub ntddk.h READ/WRITE_REGISTER_USHORT. */
PFN_WDF_TEST_READ_REGISTER_USHORT WdfTestReadRegisterUshortHook;
PFN_WDF_TEST_WRITE_REGISTER_USHORT WdfTestWriteRegisterUshortHook;
/* Instrumentation hook consumed by our stub wdf.h spinlock acquire/release. */
ULONGLONG WdfTestSpinLockSequence;

typedef struct _TEST_CALLBACKS {
    WDFDEVICE ExpectedDevice;
    PVIRTIO_PCI_INTERRUPTS Interrupts;
    int ConfigCalls;
    int QueueCallsTotal;
    int QueueCallsPerIndex[64];
} TEST_CALLBACKS;

static VOID TestEvtConfigChange(_In_ WDFDEVICE Device, _In_opt_ PVOID Context)
{
    TEST_CALLBACKS* cb = (TEST_CALLBACKS*)Context;
    ULONG q;

    assert(cb != NULL);
    assert(Device == cb->ExpectedDevice);
    assert(cb->Interrupts != NULL);
    assert(cb->Interrupts->ConfigLock != NULL);
    assert(cb->Interrupts->ConfigLock->Held == TRUE);
    if (cb->Interrupts->CommonCfgLock != NULL) {
        assert(cb->Interrupts->CommonCfgLock->Held == FALSE);
    }
    if (cb->Interrupts->QueueLocks != NULL) {
        for (q = 0; q < cb->Interrupts->QueueCount; q++) {
            assert(cb->Interrupts->QueueLocks[q] != NULL);
            assert(cb->Interrupts->QueueLocks[q]->Held == FALSE);
        }
    }
    cb->ConfigCalls++;
}

static VOID TestEvtDrainQueue(_In_ WDFDEVICE Device, _In_ ULONG QueueIndex, _In_opt_ PVOID Context)
{
    TEST_CALLBACKS* cb = (TEST_CALLBACKS*)Context;
    ULONG q;

    assert(cb != NULL);
    assert(Device == cb->ExpectedDevice);
    assert(QueueIndex < 64);
    assert(cb->Interrupts != NULL);
    assert(QueueIndex < cb->Interrupts->QueueCount);
    if (cb->Interrupts->CommonCfgLock != NULL) {
        assert(cb->Interrupts->CommonCfgLock->Held == FALSE);
    }
    assert(cb->Interrupts->ConfigLock != NULL);
    assert(cb->Interrupts->ConfigLock->Held == FALSE);
    assert(cb->Interrupts->QueueLocks != NULL);
    assert(cb->Interrupts->QueueLocks[QueueIndex] != NULL);
    assert(cb->Interrupts->QueueLocks[QueueIndex]->Held == TRUE);
    for (q = 0; q < cb->Interrupts->QueueCount; q++) {
        assert(cb->Interrupts->QueueLocks[q] != NULL);
        assert(cb->Interrupts->QueueLocks[q]->Held == ((q == QueueIndex) ? TRUE : FALSE));
    }
    cb->QueueCallsTotal++;
    cb->QueueCallsPerIndex[QueueIndex]++;
}

static void ResetCallbacks(TEST_CALLBACKS* cb)
{
    memset(cb, 0, sizeof(*cb));
}

static void ResetCallbackCounters(TEST_CALLBACKS* cb)
{
    assert(cb != NULL);
    cb->ConfigCalls = 0;
    cb->QueueCallsTotal = 0;
    memset(cb->QueueCallsPerIndex, 0, sizeof(cb->QueueCallsPerIndex));
}

static void ResetRegisterReadInstrumentation(void)
{
    WdfTestReadRegisterUcharCount = 0;
    WdfTestLastReadRegisterUcharAddress = NULL;
}

/*
 * Minimal emulation of the virtio "CommonCfg queue_msix_vector" windowed register.
 *
 * In the virtio spec, queue_select chooses which queue's configuration is being
 * accessed via the queue_* fields. Real hardware stores a distinct
 * queue_msix_vector per queue, but the MMIO offset is fixed.
 *
 * Our host tests need to observe per-queue vector programming, so we virtualize
 * reads/writes to &CommonCfg->queue_msix_vector using the ntddk.h hook pointers.
 */
static volatile VIRTIO_PCI_COMMON_CFG* gTestCommonCfg;
static ULONG gTestCommonCfgQueueCount;
static USHORT gTestCommonCfgQueueVectors[64];

/*
 * Optional fault injection: override the returned value for a specific USHORT
 * register address. This lets tests validate that the helper rejects hardware
 * that does not latch MSI-X vector programming (readback mismatch).
 */
static volatile const USHORT* gTestOverrideReadRegisterUshortAddress;
static USHORT gTestOverrideReadRegisterUshortValue;

static USHORT TestReadRegisterUshort(_In_ volatile const USHORT* Register)
{
    if (gTestOverrideReadRegisterUshortAddress != NULL && Register == gTestOverrideReadRegisterUshortAddress) {
        return gTestOverrideReadRegisterUshortValue;
    }

    if (gTestCommonCfg != NULL && Register == (volatile const USHORT*)&gTestCommonCfg->queue_msix_vector) {
        USHORT q = (USHORT)gTestCommonCfg->queue_select;
        if (q < gTestCommonCfgQueueCount) {
            return gTestCommonCfgQueueVectors[q];
        }
    }

    return *Register;
}

static VOID TestWriteRegisterUshort(_Out_ volatile USHORT* Register, _In_ USHORT Value)
{
    if (gTestCommonCfg != NULL && Register == (volatile USHORT*)&gTestCommonCfg->queue_msix_vector) {
        USHORT q = (USHORT)gTestCommonCfg->queue_select;
        if (q < gTestCommonCfgQueueCount) {
            gTestCommonCfgQueueVectors[q] = Value;
        }
    }

    *Register = Value;
}

static void InstallCommonCfgQueueVectorWindowHooks(_In_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg, _In_ ULONG QueueCount)
{
    ULONG i;

    assert(CommonCfg != NULL);
    assert(QueueCount <= 64);

    gTestCommonCfg = CommonCfg;
    gTestCommonCfgQueueCount = QueueCount;
    for (i = 0; i < QueueCount; i++) {
        gTestCommonCfgQueueVectors[i] = VIRTIO_PCI_MSI_NO_VECTOR;
    }

    WdfTestReadRegisterUshortHook = TestReadRegisterUshort;
    WdfTestWriteRegisterUshortHook = TestWriteRegisterUshort;
}

static void UninstallCommonCfgQueueVectorWindowHooks(void)
{
    gTestCommonCfg = NULL;
    gTestCommonCfgQueueCount = 0;
    memset(gTestCommonCfgQueueVectors, 0, sizeof(gTestCommonCfgQueueVectors));
    WdfTestReadRegisterUshortHook = NULL;
    WdfTestWriteRegisterUshortHook = NULL;
}

static void InstallReadRegisterUshortOverride(_In_ volatile const USHORT* Address, _In_ USHORT Value)
{
    gTestOverrideReadRegisterUshortAddress = Address;
    gTestOverrideReadRegisterUshortValue = Value;
}

static void ClearReadRegisterUshortOverride(void)
{
    gTestOverrideReadRegisterUshortAddress = NULL;
    gTestOverrideReadRegisterUshortValue = 0;
}

static USHORT ReadCommonCfgQueueVector(_Inout_ volatile VIRTIO_PCI_COMMON_CFG* CommonCfg, _In_ USHORT QueueIndex)
{
    WRITE_REGISTER_USHORT(&CommonCfg->queue_select, QueueIndex);
    (VOID)READ_REGISTER_USHORT(&CommonCfg->queue_select);
    return READ_REGISTER_USHORT(&CommonCfg->queue_msix_vector);
}

static void ResetSpinLockInstrumentation(void)
{
    WdfTestSpinLockSequence = 0;
}

static void AssertInterruptLocksReleased(_In_ const VIRTIO_PCI_INTERRUPTS* Interrupts)
{
    ULONG q;

    assert(Interrupts != NULL);

    if (Interrupts->CommonCfgLock != NULL) {
        assert(Interrupts->CommonCfgLock->Held == FALSE);
    }

    if (Interrupts->ConfigLock != NULL) {
        assert(Interrupts->ConfigLock->Held == FALSE);
    }

    if (Interrupts->QueueLocks != NULL) {
        for (q = 0; q < Interrupts->QueueCount; q++) {
            assert(Interrupts->QueueLocks[q] != NULL);
            assert(Interrupts->QueueLocks[q]->Held == FALSE);
        }
    }
}

static void PrepareIntx(
    _Out_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _Out_ WDFDEVICE* DeviceOut,
    _Inout_ TEST_CALLBACKS* Callbacks,
    _In_ ULONG QueueCount,
    _Inout_ volatile UCHAR* IsrStatusRegister)
{
    WDFDEVICE dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    CM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    memset(&rawDesc, 0, sizeof(rawDesc));
    rawDesc.Type = CmResourceTypeInterrupt;
    rawDesc.Flags = 0;

    memset(&transDesc, 0, sizeof(transDesc));
    transDesc.Type = CmResourceTypeInterrupt;
    transDesc.Flags = 0;

    rawList.Count = 1;
    rawList.Descriptors = &rawDesc;
    transList.Count = 1;
    transList.Descriptors = &transDesc;

    ResetCallbacks(Callbacks);
    Callbacks->ExpectedDevice = dev;
    Callbacks->Interrupts = Interrupts;

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        Interrupts,
        &rawList,
        &transList,
        QueueCount,
        IsrStatusRegister,
        NULL,
        TestEvtConfigChange,
        TestEvtDrainQueue,
        Callbacks);
    assert(st == STATUS_SUCCESS);
    assert(Interrupts->Mode == VirtioPciInterruptModeIntx);
    assert(Interrupts->u.Intx.Interrupt != NULL);

    *DeviceOut = dev;
}

static void PrepareMsix(
    _Out_ PVIRTIO_PCI_INTERRUPTS Interrupts,
    _Out_ WDFDEVICE* DeviceOut,
    _Inout_ TEST_CALLBACKS* Callbacks,
    _In_ ULONG QueueCount,
    _In_ ULONG MessageCount,
    _Out_opt_ WDFSPINLOCK* CommonCfgLockOut)
{
    WDFDEVICE dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    CM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;
    WDFSPINLOCK commonCfgLock;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    memset(&rawDesc, 0, sizeof(rawDesc));
    rawDesc.Type = CmResourceTypeInterrupt;
    rawDesc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    rawDesc.u.MessageInterrupt.MessageCount = MessageCount;

    memset(&transDesc, 0, sizeof(transDesc));
    transDesc.Type = CmResourceTypeInterrupt;
    transDesc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    transDesc.u.MessageInterrupt.MessageCount = MessageCount;

    rawList.Count = 1;
    rawList.Descriptors = &rawDesc;
    transList.Count = 1;
    transList.Descriptors = &transDesc;

    ResetCallbacks(Callbacks);
    Callbacks->ExpectedDevice = dev;
    Callbacks->Interrupts = Interrupts;

    commonCfgLock = NULL;
    if (CommonCfgLockOut != NULL) {
        WDF_OBJECT_ATTRIBUTES lockAttributes;
        WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
        lockAttributes.ParentObject = dev;
        st = WdfSpinLockCreate(&lockAttributes, &commonCfgLock);
        assert(st == STATUS_SUCCESS);
        *CommonCfgLockOut = commonCfgLock;
    }

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        Interrupts,
        &rawList,
        &transList,
        QueueCount,
        NULL, /* ISR status register is INTx-only. */
        commonCfgLock,
        TestEvtConfigChange,
        TestEvtDrainQueue,
        Callbacks);
    assert(st == STATUS_SUCCESS);
    assert(Interrupts->Mode == VirtioPciInterruptModeMsix);
    assert(Interrupts->u.Msix.Interrupts != NULL);
    assert(Interrupts->u.Msix.UsedVectorCount >= 1);

    *DeviceOut = dev;
}

static void Cleanup(_Inout_ PVIRTIO_PCI_INTERRUPTS Interrupts, _In_ WDFDEVICE Device)
{
    VirtioPciInterruptsReleaseHardware(Interrupts);
    WdfTestDestroyDevice(Device);
}

static void TestIntxSpuriousInterrupt(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    BOOLEAN handled;

    isrStatus = 0;
    ResetRegisterReadInstrumentation();
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);

    assert(handled == FALSE);
    assert(interrupts.u.Intx.SpuriousCount == 1);
    assert(interrupts.u.Intx.Interrupt->DpcQueueCalls == 0);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == FALSE);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);

    assert(WdfTestReadRegisterUcharCount == 1);
    assert(WdfTestLastReadRegisterUcharAddress == &isrStatus);

    Cleanup(&interrupts, dev);
}

static void TestIntxRealInterruptDispatch(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    BOOLEAN handled;
    ULONG configAcquireBefore;
    ULONG configReleaseBefore;
    ULONG queueAcquireBefore[2];
    ULONG queueReleaseBefore[2];

    isrStatus = 0;
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    /* CONFIG only */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    isrStatus = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore + 1);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore + 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0]);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0]);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1]);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1]);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* QUEUE only */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    isrStatus = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0] + 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0] + 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1] + 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1] + 1);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 1);

    /* CONFIG + QUEUE */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    isrStatus = VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore + 1);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore + 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0] + 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0] + 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1] + 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1] + 1);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 1);

    Cleanup(&interrupts, dev);
}

static void TestIntxPendingStatusCoalesce(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    BOOLEAN handled;

    isrStatus = 0;
    ResetRegisterReadInstrumentation();
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;

    /* First interrupt: CONFIG only. */
    isrStatus = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    assert(interrupts.u.Intx.PendingIsrStatus == VIRTIO_PCI_ISR_CONFIG_INTERRUPT);

    /*
     * Second interrupt arrives before the DPC runs: QUEUE only.
     *
     * PendingIsrStatus should accumulate via InterlockedOr so the single DPC run
     * dispatches both config + queue processing.
     */
    isrStatus = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueueCalls == 2);
    assert(interrupts.u.Intx.PendingIsrStatus ==
        (LONG)(VIRTIO_PCI_ISR_CONFIG_INTERRUPT | VIRTIO_PCI_ISR_QUEUE_INTERRUPT));

    /* INTx ISR must read-to-ack for both interrupts. */
    assert(WdfTestReadRegisterUcharCount == 2);
    assert(WdfTestLastReadRegisterUcharAddress == &isrStatus);

    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);

    assert(interrupts.u.Intx.Interrupt->DpcQueued == FALSE);
    assert(interrupts.u.Intx.PendingIsrStatus == 0);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 1);

    Cleanup(&interrupts, dev);
}

static void TestDiagnosticCounters(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    volatile LONG interruptCounter;
    volatile LONG dpcCounter;
    BOOLEAN handled;

    /* INTx: spurious interrupt should not increment counters. */
    interruptCounter = 0;
    dpcCounter = 0;
    isrStatus = 0;
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);
    interrupts.InterruptCounter = &interruptCounter;
    interrupts.DpcCounter = &dpcCounter;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == FALSE);
    assert(interruptCounter == 0);
    assert(dpcCounter == 0);
    Cleanup(&interrupts, dev);

    /* INTx: real interrupt should increment both counters when DPC runs. */
    interruptCounter = 0;
    dpcCounter = 0;
    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);
    interrupts.InterruptCounter = &interruptCounter;
    interrupts.DpcCounter = &dpcCounter;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interruptCounter == 1);
    assert(dpcCounter == 0);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    assert(dpcCounter == 1);
    Cleanup(&interrupts, dev);

    /* MSI-X: interrupt should increment both counters when DPC runs. */
    interruptCounter = 0;
    dpcCounter = 0;
    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, NULL);
    interrupts.InterruptCounter = &interruptCounter;
    interrupts.DpcCounter = &dpcCounter;
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interruptCounter == 1);
    assert(dpcCounter == 0);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    assert(dpcCounter == 1);
    Cleanup(&interrupts, dev);

    /* MSI-X: while ResetInProgress is set, ISR should still increment interrupt counter but not queue a DPC. */
    interruptCounter = 0;
    dpcCounter = 0;
    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, NULL);
    interrupts.InterruptCounter = &interruptCounter;
    interrupts.DpcCounter = &dpcCounter;
    InterlockedExchange(&interrupts.ResetInProgress, 1);
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interruptCounter == 1);
    assert(dpcCounter == 0);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == FALSE);
    Cleanup(&interrupts, dev);
}

static void TestMsixDispatchAndRouting(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;
    ULONG configAcquireBefore;
    ULONG configReleaseBefore;
    ULONG queueAcquireBefore[2];
    ULONG queueReleaseBefore[2];

    ResetRegisterReadInstrumentation();
    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 3);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);
    assert(interrupts.u.Msix.QueueVectors[0] == 1);
    assert(interrupts.u.Msix.QueueVectors[1] == 2);

    /* MSI-X ISR must not read ISR status. */
    assert(WdfTestReadRegisterUcharCount == 0);

    /* Vector 0: config only (no queue mask). */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore + 1);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore + 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0]);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0]);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1]);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1]);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* Vector 1: queue 0 only. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0] + 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0] + 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1]);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1]);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 1);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 0);

    /* Vector 2: queue 1 only. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    queueAcquireBefore[0] = interrupts.QueueLocks[0]->AcquireCalls;
    queueReleaseBefore[0] = interrupts.QueueLocks[0]->ReleaseCalls;
    queueAcquireBefore[1] = interrupts.QueueLocks[1]->AcquireCalls;
    queueReleaseBefore[1] = interrupts.QueueLocks[1]->ReleaseCalls;
    handled = interrupts.u.Msix.Interrupts[2]->Isr(interrupts.u.Msix.Interrupts[2], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[2]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0]);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0]);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1] + 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1] + 1);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 1);
    assert(cb.QueueCallsPerIndex[0] == 0);
    assert(cb.QueueCallsPerIndex[1] == 1);

    /* Still no ISR status reads in MSI-X mode. */
    assert(WdfTestReadRegisterUcharCount == 0);

    Cleanup(&interrupts, dev);
}

static void TestMsixZeroQueuesConfigOnly(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    BOOLEAN handled;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 0);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 0 /* queues */, 1 /* message count */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.QueueCount == 0);
    assert(interrupts.u.Msix.UsedVectorCount == 1);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors == NULL);
    assert(interrupts.QueueLocks == NULL);

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(commonCfg.msix_config == 0);

    /* Config interrupt still dispatches config callback. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    AssertInterruptLocksReleased(&interrupts);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* Quiesce/Resume should work with no queues. */
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);
    assert(interrupts.u.Msix.Interrupts[0]->Enabled == FALSE);
    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);

    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 0);
    assert(interrupts.u.Msix.Interrupts[0]->Enabled == TRUE);
    assert(commonCfg.msix_config == 0);

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixPrepareHardwareMessageCountZeroFails(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    CM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    memset(&rawDesc, 0, sizeof(rawDesc));
    rawDesc.Type = CmResourceTypeInterrupt;
    rawDesc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    rawDesc.u.MessageInterrupt.MessageCount = 0;

    memset(&transDesc, 0, sizeof(transDesc));
    transDesc.Type = CmResourceTypeInterrupt;
    transDesc.Flags = CM_RESOURCE_INTERRUPT_MESSAGE;
    transDesc.u.MessageInterrupt.MessageCount = 0;

    rawList.Count = 1;
    rawList.Descriptors = &rawDesc;
    transList.Count = 1;
    transList.Descriptors = &transDesc;

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        &interrupts,
        &rawList,
        &transList,
        2 /* QueueCount */,
        NULL, /* ISR status register is INTx-only. */
        NULL,
        NULL,
        NULL,
        NULL);
    assert(st == STATUS_DEVICE_CONFIGURATION_ERROR);

    /* Ensure cleanup of any partially-initialized resources is safe. */
    VirtioPciInterruptsReleaseHardware(&interrupts);
    WdfTestDestroyDevice(dev);
}

static void TestPrepareHardwareMissingInterruptResourceFails(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    rawList.Count = 0;
    rawList.Descriptors = NULL;
    transList.Count = 0;
    transList.Descriptors = NULL;

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        &interrupts,
        &rawList,
        &transList,
        0 /* QueueCount */,
        NULL,
        NULL,
        NULL,
        NULL,
        NULL);
    assert(st == STATUS_RESOURCE_TYPE_NOT_FOUND);

    VirtioPciInterruptsReleaseHardware(&interrupts);
    WdfTestDestroyDevice(dev);
}

static void TestPrepareHardwareQueueCountTooLargeFails(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    CM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    memset(&rawDesc, 0, sizeof(rawDesc));
    rawDesc.Type = CmResourceTypeInterrupt;
    rawDesc.Flags = 0;

    memset(&transDesc, 0, sizeof(transDesc));
    transDesc.Type = CmResourceTypeInterrupt;
    transDesc.Flags = 0;

    rawList.Count = 1;
    rawList.Descriptors = &rawDesc;
    transList.Count = 1;
    transList.Descriptors = &transDesc;

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        &interrupts,
        &rawList,
        &transList,
        65 /* QueueCount */,
        NULL,
        NULL,
        NULL,
        NULL,
        NULL);
    assert(st == STATUS_NOT_SUPPORTED);

    VirtioPciInterruptsReleaseHardware(&interrupts);
    WdfTestDestroyDevice(dev);
}

static void TestIntxNullIsrStatusRegisterReturnsFalse(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    CM_PARTIAL_RESOURCE_DESCRIPTOR rawDesc;
    CM_PARTIAL_RESOURCE_DESCRIPTOR transDesc;
    WDFCMRESLIST__ rawList;
    WDFCMRESLIST__ transList;
    NTSTATUS st;
    BOOLEAN handled;

    dev = WdfTestCreateDevice();
    assert(dev != NULL);

    memset(&rawDesc, 0, sizeof(rawDesc));
    rawDesc.Type = CmResourceTypeInterrupt;
    rawDesc.Flags = 0;

    memset(&transDesc, 0, sizeof(transDesc));
    transDesc.Type = CmResourceTypeInterrupt;
    transDesc.Flags = 0;

    rawList.Count = 1;
    rawList.Descriptors = &rawDesc;
    transList.Count = 1;
    transList.Descriptors = &transDesc;

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        &interrupts,
        &rawList,
        &transList,
        2 /* QueueCount */,
        NULL /* IsrStatusRegister */,
        NULL,
        NULL,
        NULL,
        NULL);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.Mode == VirtioPciInterruptModeIntx);

    ResetRegisterReadInstrumentation();
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == FALSE);
    assert(WdfTestReadRegisterUcharCount == 0);
    assert(interrupts.u.Intx.Interrupt->DpcQueueCalls == 0);

    VirtioPciInterruptsReleaseHardware(&interrupts);
    WdfTestDestroyDevice(dev);
}

static void TestMsixLimitedVectorRouting(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;
    ULONG q;
    ULONG configAcquireBefore;
    ULONG configReleaseBefore;
    ULONG queueAcquireBefore[4];
    ULONG queueReleaseBefore[4];

    ResetRegisterReadInstrumentation();
    PrepareMsix(&interrupts, &dev, &cb, 4, 2 /* only config + 1 queue vector */, NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 2);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.u.Msix.QueueVectors[q] == 1);
    }

    /* MSI-X ISR must not read ISR status. */
    assert(WdfTestReadRegisterUcharCount == 0);

    /* Vector 0: config only (no queue mask). */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    for (q = 0; q < interrupts.QueueCount; q++) {
        queueAcquireBefore[q] = interrupts.QueueLocks[q]->AcquireCalls;
        queueReleaseBefore[q] = interrupts.QueueLocks[q]->ReleaseCalls;
    }
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore + 1);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore + 1);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.QueueLocks[q]->AcquireCalls == queueAcquireBefore[q]);
        assert(interrupts.QueueLocks[q]->ReleaseCalls == queueReleaseBefore[q]);
    }
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* Vector 1: all queues (round-robin onto the single queue vector). */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    for (q = 0; q < interrupts.QueueCount; q++) {
        queueAcquireBefore[q] = interrupts.QueueLocks[q]->AcquireCalls;
        queueReleaseBefore[q] = interrupts.QueueLocks[q]->ReleaseCalls;
    }
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.QueueLocks[q]->AcquireCalls == queueAcquireBefore[q] + 1);
        assert(interrupts.QueueLocks[q]->ReleaseCalls == queueReleaseBefore[q] + 1);
    }
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 4);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(cb.QueueCallsPerIndex[q] == 1);
    }

    /* Still no ISR status reads in MSI-X mode. */
    assert(WdfTestReadRegisterUcharCount == 0);

    Cleanup(&interrupts, dev);
}

static void TestMsixLimitedVectorProgramming(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG q;
    ULONG commonCfgLockAcquireBefore;
    ULONG commonCfgLockReleaseBefore;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 2 /* config + 1 queue vector */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 2);
    assert(interrupts.u.Msix.ConfigVector == 0);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.u.Msix.QueueVectors[q] == 1);
    }

    commonCfgLockAcquireBefore = commonCfgLock->AcquireCalls;
    commonCfgLockReleaseBefore = commonCfgLock->ReleaseCalls;

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    /* Program should serialize the queue_select programming sequence. */
    assert(commonCfgLock->AcquireCalls == commonCfgLockAcquireBefore + 1);
    assert(commonCfgLock->ReleaseCalls == commonCfgLockReleaseBefore + 1);

    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixLimitedVectorQuiesceResumeVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG i;
    ULONG q;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 2 /* config + 1 queue vector */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 2);
    assert(interrupts.u.Msix.ConfigVector == 0);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.u.Msix.QueueVectors[q] == 1);
    }

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    /* Precondition: OS interrupt delivery enabled before quiesce. */
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
    }

    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);

    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }
    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == VIRTIO_PCI_MSI_NO_VECTOR);
    }

    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 0);

    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 1);
    }
    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixVectorUtilizationPartialQueueVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;
    ULONG q;
    ULONG configAcquireBefore;
    ULONG configReleaseBefore;
    ULONG queueAcquireBefore[4];
    ULONG queueReleaseBefore[4];

    ResetRegisterReadInstrumentation();
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 3 /* config + 2 queue vectors */, NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 3);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);

    /* Queues should be spread across vectors 1..2 (round-robin). */
    assert(interrupts.u.Msix.QueueVectors[0] == 1);
    assert(interrupts.u.Msix.QueueVectors[1] == 2);
    assert(interrupts.u.Msix.QueueVectors[2] == 1);
    assert(interrupts.u.Msix.QueueVectors[3] == 2);

    /* MSI-X ISR must not read ISR status. */
    assert(WdfTestReadRegisterUcharCount == 0);

    /* Vector 1: queues 0 + 2. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    for (q = 0; q < interrupts.QueueCount; q++) {
        queueAcquireBefore[q] = interrupts.QueueLocks[q]->AcquireCalls;
        queueReleaseBefore[q] = interrupts.QueueLocks[q]->ReleaseCalls;
    }
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0] + 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0] + 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1]);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1]);
    assert(interrupts.QueueLocks[2]->AcquireCalls == queueAcquireBefore[2] + 1);
    assert(interrupts.QueueLocks[2]->ReleaseCalls == queueReleaseBefore[2] + 1);
    assert(interrupts.QueueLocks[3]->AcquireCalls == queueAcquireBefore[3]);
    assert(interrupts.QueueLocks[3]->ReleaseCalls == queueReleaseBefore[3]);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 0);
    assert(cb.QueueCallsPerIndex[2] == 1);
    assert(cb.QueueCallsPerIndex[3] == 0);

    /* Vector 2: queues 1 + 3. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    for (q = 0; q < interrupts.QueueCount; q++) {
        queueAcquireBefore[q] = interrupts.QueueLocks[q]->AcquireCalls;
        queueReleaseBefore[q] = interrupts.QueueLocks[q]->ReleaseCalls;
    }
    handled = interrupts.u.Msix.Interrupts[2]->Isr(interrupts.u.Msix.Interrupts[2], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[2]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore);
    assert(interrupts.QueueLocks[0]->AcquireCalls == queueAcquireBefore[0]);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == queueReleaseBefore[0]);
    assert(interrupts.QueueLocks[1]->AcquireCalls == queueAcquireBefore[1] + 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == queueReleaseBefore[1] + 1);
    assert(interrupts.QueueLocks[2]->AcquireCalls == queueAcquireBefore[2]);
    assert(interrupts.QueueLocks[2]->ReleaseCalls == queueReleaseBefore[2]);
    assert(interrupts.QueueLocks[3]->AcquireCalls == queueAcquireBefore[3] + 1);
    assert(interrupts.QueueLocks[3]->ReleaseCalls == queueReleaseBefore[3] + 1);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 0);
    assert(cb.QueueCallsPerIndex[1] == 1);
    assert(cb.QueueCallsPerIndex[2] == 0);
    assert(cb.QueueCallsPerIndex[3] == 1);

    /* Still no ISR status reads in MSI-X mode. */
    assert(WdfTestReadRegisterUcharCount == 0);

    Cleanup(&interrupts, dev);
}

static void TestMsixPartialVectorProgramming(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG q;
    ULONG commonCfgLockAcquireBefore;
    ULONG commonCfgLockReleaseBefore;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 3 /* config + 2 queue vectors */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 3);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);
    assert(interrupts.u.Msix.QueueVectors[0] == 1);
    assert(interrupts.u.Msix.QueueVectors[1] == 2);
    assert(interrupts.u.Msix.QueueVectors[2] == 1);
    assert(interrupts.u.Msix.QueueVectors[3] == 2);

    commonCfgLockAcquireBefore = commonCfgLock->AcquireCalls;
    commonCfgLockReleaseBefore = commonCfgLock->ReleaseCalls;

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    /* Program should serialize the queue_select programming sequence. */
    assert(commonCfgLock->AcquireCalls == commonCfgLockAcquireBefore + 1);
    assert(commonCfgLock->ReleaseCalls == commonCfgLockReleaseBefore + 1);

    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixPartialVectorQuiesceResumeVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG i;
    ULONG q;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 3 /* config + 2 queue vectors */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 3);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors[0] == 1);
    assert(interrupts.u.Msix.QueueVectors[1] == 2);
    assert(interrupts.u.Msix.QueueVectors[2] == 1);
    assert(interrupts.u.Msix.QueueVectors[3] == 2);

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    /* Precondition: OS interrupt delivery enabled before quiesce. */
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
    }

    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);

    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }
    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == VIRTIO_PCI_MSI_NO_VECTOR);
    }

    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 0);

    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 1);
    }
    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixVectorUtilizationOnePerQueueWhenPossible(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;

    PrepareMsix(&interrupts, &dev, &cb, 3 /* queues */, 6 /* message count >= 1 + queues */, NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 4);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);
    assert(interrupts.u.Msix.QueueVectors[0] == 1);
    assert(interrupts.u.Msix.QueueVectors[1] == 2);
    assert(interrupts.u.Msix.QueueVectors[2] == 3);

    Cleanup(&interrupts, dev);
}

static void TestMsixSingleVectorFallbackRouting(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    BOOLEAN handled;
    ULONG q;
    ULONG configAcquireBefore;
    ULONG configReleaseBefore;
    ULONG queueAcquireBefore[4];
    ULONG queueReleaseBefore[4];

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    ResetRegisterReadInstrumentation();
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 1 /* message count */, NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 1);
    assert(interrupts.u.Msix.ConfigVector == 0);
    assert(interrupts.u.Msix.QueueVectors != NULL);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.u.Msix.QueueVectors[q] == 0);
    }

    /* MSI-X ISR must not read ISR status. */
    assert(WdfTestReadRegisterUcharCount == 0);

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    /* Vector 0: config + all queues. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    configAcquireBefore = interrupts.ConfigLock->AcquireCalls;
    configReleaseBefore = interrupts.ConfigLock->ReleaseCalls;
    for (q = 0; q < interrupts.QueueCount; q++) {
        queueAcquireBefore[q] = interrupts.QueueLocks[q]->AcquireCalls;
        queueReleaseBefore[q] = interrupts.QueueLocks[q]->ReleaseCalls;
    }
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    AssertInterruptLocksReleased(&interrupts);
    assert(interrupts.ConfigLock->AcquireCalls == configAcquireBefore + 1);
    assert(interrupts.ConfigLock->ReleaseCalls == configReleaseBefore + 1);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.QueueLocks[q]->AcquireCalls == queueAcquireBefore[q] + 1);
        assert(interrupts.QueueLocks[q]->ReleaseCalls == queueReleaseBefore[q] + 1);
    }
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 4);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(cb.QueueCallsPerIndex[q] == 1);
    }

    /* Still no ISR status reads in MSI-X mode. */
    assert(WdfTestReadRegisterUcharCount == 0);

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixSingleVectorQuiesceResumeVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG q;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 4);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 4 /* queues */, 1 /* message count */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.u.Msix.UsedVectorCount == 1);
    assert(interrupts.u.Msix.ConfigVector == 0);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(interrupts.u.Msix.QueueVectors[q] == 0);
    }

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(commonCfg.msix_config == 0);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == 0);
    }

    /* Quiesce must clear routing to NO_VECTOR. */
    ResetSpinLockInstrumentation();
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);
    assert(interrupts.u.Msix.Interrupts[0]->Enabled == FALSE);
    assert(interrupts.u.Msix.Interrupts[0]->DisableCalls == 1);
    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == VIRTIO_PCI_MSI_NO_VECTOR);
    }

    /* Resume must restore routing and re-enable delivery. */
    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 0);
    assert(interrupts.u.Msix.Interrupts[0]->Enabled == TRUE);
    assert(interrupts.u.Msix.Interrupts[0]->EnableCalls == 1);
    assert(commonCfg.msix_config == 0);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == 0);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixProgramQueueVectorReadbackFailure(void)
{
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    USHORT queues[2];

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 2);

    queues[0] = 1;
    queues[1] = 2;

    /* Device rejects queue vector programming by returning VIRTIO_PCI_MSI_NO_VECTOR. */
    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.queue_msix_vector, VIRTIO_PCI_MSI_NO_VECTOR);
    st = VirtioPciProgramMsixVectors(&commonCfg, NULL, 2, 3 /* config vector */, queues);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);
    assert(commonCfg.msix_config == 3);
    ClearReadRegisterUshortOverride();

    /* Only the first queue should have been attempted. */
    assert(ReadCommonCfgQueueVector(&commonCfg, 0) == 1);
    assert(ReadCommonCfgQueueVector(&commonCfg, 1) == VIRTIO_PCI_MSI_NO_VECTOR);

    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixProgramConfigVectorReadbackFailure(void)
{
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 0);

    /* Device rejects config vector programming by returning VIRTIO_PCI_MSI_NO_VECTOR. */
    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.msix_config, VIRTIO_PCI_MSI_NO_VECTOR);
    st = VirtioPciProgramMsixVectors(&commonCfg, NULL, 0, 3 /* config vector */, NULL);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);
    assert(commonCfg.msix_config == 3);
    ClearReadRegisterUshortOverride();

    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixProgramVectorsInvalidParameters(void)
{
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));

    st = VirtioPciProgramMsixVectors(NULL, NULL, 0, 0, NULL);
    assert(st == STATUS_INVALID_PARAMETER);

    st = VirtioPciProgramMsixVectors(&commonCfg, NULL, 1 /* queueCount */, 0 /* configVector */, NULL /* queueVectors */);
    assert(st == STATUS_INVALID_PARAMETER);
}

static void TestInterruptsProgramMsixVectorsNonMsixIsNoop(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    NTSTATUS st;

    isrStatus = 0;
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, NULL);
    assert(st == STATUS_SUCCESS);

    Cleanup(&interrupts, dev);
}

static void TestResetInProgressGating(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    BOOLEAN handled;

    /*
     * INTx: even while reset is in progress, ISR must still read-to-ack (and not
     * queue a DPC).
     */
    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    ResetRegisterReadInstrumentation();
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueueCalls == 0);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == FALSE);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);
    assert(WdfTestReadRegisterUcharCount == 1);
    assert(interrupts.ConfigLock->AcquireCalls == 0);
    assert(interrupts.ConfigLock->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 0);

    /*
     * INTx DPC gating: if a DPC is already queued when reset begins, the DPC
     * must bail out without dispatching callbacks and must clear the pending ISR
     * status snapshot.
     */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    ResetRegisterReadInstrumentation();
    InterlockedExchange(&interrupts.ResetInProgress, 0);
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    assert(interrupts.u.Intx.PendingIsrStatus != 0);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);
    assert(interrupts.u.Intx.PendingIsrStatus == 0);
    assert(WdfTestReadRegisterUcharCount == 1);
    assert(interrupts.ConfigLock->AcquireCalls == 0);
    assert(interrupts.ConfigLock->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 0);

    Cleanup(&interrupts, dev);

    /*
     * MSI-X: while reset is in progress, ISR should return TRUE but not queue
     * a DPC.
     */
    PrepareMsix(&interrupts, &dev, &cb, 2, 3, NULL);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueueCalls == 0);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == FALSE);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);
    assert(interrupts.ConfigLock->AcquireCalls == 0);
    assert(interrupts.ConfigLock->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 0);

    /*
     * MSI-X DPC gating: if reset begins after the ISR queues a DPC, the DPC must
     * still bail out before invoking callbacks.
     */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    InterlockedExchange(&interrupts.ResetInProgress, 0);
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == TRUE);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    AssertInterruptLocksReleased(&interrupts);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);
    assert(interrupts.ConfigLock->AcquireCalls == 0);
    assert(interrupts.ConfigLock->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 0);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 0);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 0);

    Cleanup(&interrupts, dev);
}

static void TestMsixQuiesceResumeVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG i;
    ULONG q;
    ULONG commonCfgLockAcquireBefore;
    ULONG commonCfgLockReleaseBefore;
    BOOLEAN handled;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 2);

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    /* Establish a known vector mapping and program the device. */
    interrupts.u.Msix.ConfigVector = 0;
    interrupts.u.Msix.QueueVectors[0] = 1;
    interrupts.u.Msix.QueueVectors[1] = 2;

    st = VirtioPciInterruptsProgramMsixVectors(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    /* Precondition: OS interrupt delivery enabled before quiesce. */
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
    }

    commonCfgLockAcquireBefore = commonCfgLock->AcquireCalls;
    commonCfgLockReleaseBefore = commonCfgLock->ReleaseCalls;
    ResetSpinLockInstrumentation();

    /* Quiesce: gate DPCs, disable OS delivery, clear device routing, sync locks. */
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    assert(InterlockedCompareExchange(&interrupts.ResetInProgress, 0, 0) != 0);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }

    /* CommonCfg lock should serialize MSI-X vector clearing. */
    assert(commonCfgLock->AcquireCalls == commonCfgLockAcquireBefore + 1);
    assert(commonCfgLock->ReleaseCalls == commonCfgLockReleaseBefore + 1);

    /* Quiesce must synchronize with config + per-queue locks. */
    assert(interrupts.ConfigLock->AcquireCalls == 1);
    assert(interrupts.ConfigLock->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 1);
    assert(commonCfgLock->LastAcquireSequence < interrupts.ConfigLock->LastAcquireSequence);
    assert(interrupts.ConfigLock->LastAcquireSequence < interrupts.QueueLocks[0]->LastAcquireSequence);
    assert(interrupts.QueueLocks[0]->LastAcquireSequence < interrupts.QueueLocks[1]->LastAcquireSequence);

    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == VIRTIO_PCI_MSI_NO_VECTOR);
    }

    /* ResetInProgress gating: ISR returns TRUE but does not queue a DPC. */
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueueCalls == 0);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == FALSE);

    /* Resume: should restore routing and re-enable OS delivery. */
    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);

    assert(InterlockedCompareExchange(&interrupts.ResetInProgress, 0, 0) == 0);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 1);
    }

    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    for (q = 0; q < interrupts.QueueCount; q++) {
        assert(ReadCommonCfgQueueVector(&commonCfg, (USHORT)q) == interrupts.u.Msix.QueueVectors[q]);
    }

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixQuiesceWithoutCommonCfgReturnsError(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    WDFSPINLOCK commonCfgLock;
    NTSTATUS st;
    ULONG i;

    commonCfgLock = NULL;
    PrepareMsix(&interrupts, &dev, &cb, 2 /* queues */, 3 /* config + 2 queues */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    assert(interrupts.ResetInProgress == 0);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == TRUE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 0);
    }

    ResetSpinLockInstrumentation();
    st = VirtioPciInterruptsQuiesce(&interrupts, NULL);
    assert(st == STATUS_INVALID_PARAMETER);

    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }

    /* No CommonCfg means no vector-clearing lock acquisition. */
    assert(commonCfgLock->AcquireCalls == 0);
    assert(commonCfgLock->ReleaseCalls == 0);

    /* Quiesce should still synchronize with config + per-queue locks. */
    assert(interrupts.ConfigLock->AcquireCalls == 1);
    assert(interrupts.ConfigLock->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 1);

    Cleanup(&interrupts, dev);
}

static void TestMsixResumeWithoutCommonCfgReturnsError(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    WDFSPINLOCK commonCfgLock;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    ULONG i;

    commonCfgLock = NULL;
    memset((void*)&commonCfg, 0, sizeof(commonCfg));

    PrepareMsix(&interrupts, &dev, &cb, 2 /* queues */, 3 /* config + 2 queues */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 0);
    }

    st = VirtioPciInterruptsResume(&interrupts, NULL);
    assert(st == STATUS_INVALID_PARAMETER);

    /* Resume failure must not re-enable interrupts or clear ResetInProgress. */
    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 0);
    }

    Cleanup(&interrupts, dev);
}

static void TestIntxQuiesceResume(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile UCHAR isrStatus;
    NTSTATUS st;
    BOOLEAN handled;

    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    assert(interrupts.ResetInProgress == 0);
    assert(interrupts.u.Intx.Interrupt->Enabled == TRUE);

    ResetSpinLockInstrumentation();
    st = VirtioPciInterruptsQuiesce(&interrupts, NULL);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);
    assert(interrupts.u.Intx.Interrupt->Enabled == FALSE);
    assert(interrupts.u.Intx.Interrupt->DisableCalls == 1);

    /* Quiesce must synchronize with the ConfigLock and per-queue locks. */
    assert(interrupts.ConfigLock->AcquireCalls == 1);
    assert(interrupts.ConfigLock->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[0]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[0]->ReleaseCalls == 1);
    assert(interrupts.QueueLocks[1]->AcquireCalls == 1);
    assert(interrupts.QueueLocks[1]->ReleaseCalls == 1);
    assert(interrupts.ConfigLock->LastAcquireSequence < interrupts.QueueLocks[0]->LastAcquireSequence);
    assert(interrupts.QueueLocks[0]->LastAcquireSequence < interrupts.QueueLocks[1]->LastAcquireSequence);

    /*
     * While quiesced/resetting, ISR must still read-to-ack but must not queue a
     * DPC (ResetInProgress gating).
     */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    ResetRegisterReadInstrumentation();
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == FALSE);
    assert(interrupts.u.Intx.Interrupt->DpcQueueCalls == 0);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);
    assert(WdfTestReadRegisterUcharCount == 1);

    st = VirtioPciInterruptsResume(&interrupts, NULL);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 0);
    assert(interrupts.u.Intx.Interrupt->Enabled == TRUE);
    assert(interrupts.u.Intx.Interrupt->EnableCalls == 1);

    /* After resume, interrupts should dispatch again. */
    ResetCallbackCounters(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    AssertInterruptLocksReleased(&interrupts);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 2);

    Cleanup(&interrupts, dev);
}

static volatile const USHORT* gTestReadUshortFailAddress;

static USHORT TestReadRegisterUshortFailOnce(_In_ volatile const USHORT* Register)
{
    if (gTestReadUshortFailAddress != NULL && Register == gTestReadUshortFailAddress) {
        return VIRTIO_PCI_MSI_NO_VECTOR;
    }

    return *Register;
}

static void TestMsixResumeVectorReadbackFailure(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    WDFSPINLOCK commonCfgLock;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;

    commonCfgLock = NULL;
    memset((void*)&commonCfg, 0, sizeof(commonCfg));

    PrepareMsix(&interrupts, &dev, &cb, 2, 3, &commonCfgLock);
    assert(commonCfgLock != NULL);

    /* Quiesce puts us in the normal "reset in progress" state. */
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);

    /* Simulate a device that rejects MSI-X vector programming via readback. */
    gTestReadUshortFailAddress = (volatile const USHORT*)&commonCfg.msix_config;
    WdfTestReadRegisterUshortHook = TestReadRegisterUshortFailOnce;
    WdfTestWriteRegisterUshortHook = NULL;

    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);

    /* Resume failure must not re-enable interrupts or clear ResetInProgress. */
    assert(interrupts.ResetInProgress == 1);
    for (ULONG i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 0);
    }

    gTestReadUshortFailAddress = NULL;
    WdfTestReadRegisterUshortHook = NULL;

    Cleanup(&interrupts, dev);
}

static void TestMsixResumeQueueVectorReadbackFailure(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    WDFSPINLOCK commonCfgLock;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    ULONG i;

    commonCfgLock = NULL;
    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 2);

    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, &commonCfgLock);
    assert(commonCfgLock != NULL);

    /* Quiesce puts us in the normal "reset in progress" state. */
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_SUCCESS);
    assert(interrupts.ResetInProgress == 1);

    /* Simulate a device that rejects MSI-X queue vector programming via readback. */
    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.queue_msix_vector, VIRTIO_PCI_MSI_NO_VECTOR);
    st = VirtioPciInterruptsResume(&interrupts, &commonCfg);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);

    /* Resume failure must not re-enable interrupts or clear ResetInProgress. */
    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->EnableCalls == 0);
    }

    /*
     * Resume should have successfully programmed msix_config before failing on
     * the first queue vector.
     */
    ClearReadRegisterUshortOverride();
    assert(commonCfg.msix_config == interrupts.u.Msix.ConfigVector);
    assert(ReadCommonCfgQueueVector(&commonCfg, 0) == interrupts.u.Msix.QueueVectors[0]);
    assert(ReadCommonCfgQueueVector(&commonCfg, 1) == VIRTIO_PCI_MSI_NO_VECTOR);

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixQuiesceQueueVectorReadbackFailure(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    ULONG i;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 2);

    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */, NULL);

    /*
     * VirtioPciInterruptsQuiesce clears device routing and validates that the
     * device reads back VIRTIO_PCI_MSI_NO_VECTOR. Emulate a device that fails
     * to clear queue_msix_vector.
     */
    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.queue_msix_vector, 0 /* wrong value */);
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);

    /* Even on failure, quiesce should still have disabled interrupts. */
    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }

    ClearReadRegisterUshortOverride();

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixQuiesceConfigVectorReadbackFailure(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    ULONG i;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 1);

    PrepareMsix(&interrupts, &dev, &cb, 1, 2 /* config + 1 queue */, NULL);

    /*
     * VirtioPciInterruptsQuiesce clears device routing and validates that the
     * device reads back VIRTIO_PCI_MSI_NO_VECTOR. Emulate a device that fails
     * to clear msix_config.
     */
    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.msix_config, 0 /* wrong value */);
    st = VirtioPciInterruptsQuiesce(&interrupts, &commonCfg);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);

    /* Even on failure, quiesce should still have disabled interrupts. */
    assert(interrupts.ResetInProgress == 1);
    for (i = 0; i < interrupts.u.Msix.UsedVectorCount; i++) {
        assert(interrupts.u.Msix.Interrupts[i]->Enabled == FALSE);
        assert(interrupts.u.Msix.Interrupts[i]->DisableCalls == 1);
    }

    /*
     * The write should still have been attempted (it is not rolled back), even
     * though our readback fault injection made validation fail.
     */
    assert(commonCfg.msix_config == VIRTIO_PCI_MSI_NO_VECTOR);

    ClearReadRegisterUshortOverride();

    Cleanup(&interrupts, dev);
    UninstallCommonCfgQueueVectorWindowHooks();
}

int main(void)
{
    TestIntxSpuriousInterrupt();
    TestIntxRealInterruptDispatch();
    TestIntxPendingStatusCoalesce();
    TestDiagnosticCounters();
    TestMsixDispatchAndRouting();
    TestMsixZeroQueuesConfigOnly();
    TestMsixPrepareHardwareMessageCountZeroFails();
    TestPrepareHardwareMissingInterruptResourceFails();
    TestPrepareHardwareQueueCountTooLargeFails();
    TestIntxNullIsrStatusRegisterReturnsFalse();
    TestMsixLimitedVectorRouting();
    TestMsixLimitedVectorProgramming();
    TestMsixLimitedVectorQuiesceResumeVectors();
    TestMsixVectorUtilizationPartialQueueVectors();
    TestMsixPartialVectorProgramming();
    TestMsixPartialVectorQuiesceResumeVectors();
    TestMsixVectorUtilizationOnePerQueueWhenPossible();
    TestMsixSingleVectorFallbackRouting();
    TestMsixSingleVectorQuiesceResumeVectors();
    TestMsixProgramQueueVectorReadbackFailure();
    TestMsixProgramConfigVectorReadbackFailure();
    TestMsixProgramVectorsInvalidParameters();
    TestInterruptsProgramMsixVectorsNonMsixIsNoop();
    TestResetInProgressGating();
    TestMsixQuiesceResumeVectors();
    TestMsixQuiesceWithoutCommonCfgReturnsError();
    TestIntxQuiesceResume();
    TestMsixResumeVectorReadbackFailure();
    TestMsixResumeQueueVectorReadbackFailure();
    TestMsixQuiesceQueueVectorReadbackFailure();
    TestMsixQuiesceConfigVectorReadbackFailure();
    TestMsixResumeWithoutCommonCfgReturnsError();
    printf("virtio_pci_interrupts_host_tests: PASS\n");
    return 0;
}
