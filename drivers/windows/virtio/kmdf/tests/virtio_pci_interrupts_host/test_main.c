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
    int ConfigCalls;
    int QueueCallsTotal;
    int QueueCallsPerIndex[64];
} TEST_CALLBACKS;

static VOID TestEvtConfigChange(_In_ WDFDEVICE Device, _In_opt_ PVOID Context)
{
    TEST_CALLBACKS* cb = (TEST_CALLBACKS*)Context;
    assert(cb != NULL);
    assert(Device == cb->ExpectedDevice);
    cb->ConfigCalls++;
}

static VOID TestEvtDrainQueue(_In_ WDFDEVICE Device, _In_ ULONG QueueIndex, _In_opt_ PVOID Context)
{
    TEST_CALLBACKS* cb = (TEST_CALLBACKS*)Context;
    assert(cb != NULL);
    assert(Device == cb->ExpectedDevice);
    assert(QueueIndex < 64);
    cb->QueueCallsTotal++;
    cb->QueueCallsPerIndex[QueueIndex]++;
}

static void ResetCallbacks(TEST_CALLBACKS* cb)
{
    memset(cb, 0, sizeof(*cb));
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

    isrStatus = 0;
    PrepareIntx(&interrupts, &dev, &cb, 2, &isrStatus);

    /* CONFIG only */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* QUEUE only */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = VIRTIO_PCI_ISR_QUEUE_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 1);

    /* CONFIG + QUEUE */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT;
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 1);

    Cleanup(&interrupts, dev);
}

static void TestMsixDispatchAndRouting(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;

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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* Vector 1: queue 0 only. */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 1);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 0);

    /* Vector 2: queue 1 only. */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[2]->Isr(interrupts.u.Msix.Interrupts[2], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[2]);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 1);
    assert(cb.QueueCallsPerIndex[0] == 0);
    assert(cb.QueueCallsPerIndex[1] == 1);

    /* Still no ISR status reads in MSI-X mode. */
    assert(WdfTestReadRegisterUcharCount == 0);

    Cleanup(&interrupts, dev);
}

static void TestMsixLimitedVectorRouting(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;
    ULONG q;

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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
    assert(cb.ConfigCalls == 1);
    assert(cb.QueueCallsTotal == 0);

    /* Vector 1: all queues (round-robin onto the single queue vector). */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
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

static void TestMsixVectorUtilizationPartialQueueVectors(void)
{
    VIRTIO_PCI_INTERRUPTS interrupts;
    WDFDEVICE dev;
    TEST_CALLBACKS cb;
    BOOLEAN handled;

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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 2);
    assert(cb.QueueCallsPerIndex[0] == 1);
    assert(cb.QueueCallsPerIndex[1] == 0);
    assert(cb.QueueCallsPerIndex[2] == 1);
    assert(cb.QueueCallsPerIndex[3] == 0);

    /* Vector 2: queues 1 + 3. */
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[2]->Isr(interrupts.u.Msix.Interrupts[2], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[2]);
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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    handled = interrupts.u.Msix.Interrupts[0]->Isr(interrupts.u.Msix.Interrupts[0], 0);
    assert(handled == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[0]);
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

static void TestMsixProgramQueueVectorReadbackFailure(void)
{
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;
    USHORT queues[2];

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 2);

    queues[0] = 1;
    queues[1] = 2;

    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.queue_msix_vector, 0 /* wrong value */);
    st = VirtioPciProgramMsixVectors(&commonCfg, NULL, 2, 3 /* config vector */, queues);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);
    ClearReadRegisterUshortOverride();

    UninstallCommonCfgQueueVectorWindowHooks();
}

static void TestMsixProgramConfigVectorReadbackFailure(void)
{
    volatile VIRTIO_PCI_COMMON_CFG commonCfg;
    NTSTATUS st;

    memset((void*)&commonCfg, 0, sizeof(commonCfg));
    InstallCommonCfgQueueVectorWindowHooks(&commonCfg, 0);

    InstallReadRegisterUshortOverride((volatile const USHORT*)&commonCfg.msix_config, 0 /* wrong value */);
    st = VirtioPciProgramMsixVectors(&commonCfg, NULL, 0, 3 /* config vector */, NULL);
    assert(st == STATUS_DEVICE_HARDWARE_ERROR);
    ClearReadRegisterUshortOverride();

    UninstallCommonCfgQueueVectorWindowHooks();
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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    ResetRegisterReadInstrumentation();
    InterlockedExchange(&interrupts.ResetInProgress, 0);
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    assert(interrupts.u.Intx.PendingIsrStatus != 0);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    InterlockedExchange(&interrupts.ResetInProgress, 0);
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == TRUE);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    WdfTestInterruptRunDpc(interrupts.u.Msix.Interrupts[1]);
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
    ResetCallbacks(&cb);
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
    ResetCallbacks(&cb);
    cb.ExpectedDevice = dev;
    isrStatus = (UCHAR)(VIRTIO_PCI_ISR_QUEUE_INTERRUPT | VIRTIO_PCI_ISR_CONFIG_INTERRUPT);
    handled = interrupts.u.Intx.Interrupt->Isr(interrupts.u.Intx.Interrupt, 0);
    assert(handled == TRUE);
    assert(interrupts.u.Intx.Interrupt->DpcQueued == TRUE);
    WdfTestInterruptRunDpc(interrupts.u.Intx.Interrupt);
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
    TestMsixDispatchAndRouting();
    TestMsixLimitedVectorRouting();
    TestMsixLimitedVectorProgramming();
    TestMsixVectorUtilizationPartialQueueVectors();
    TestMsixPartialVectorProgramming();
    TestMsixVectorUtilizationOnePerQueueWhenPossible();
    TestMsixSingleVectorFallbackRouting();
    TestMsixProgramQueueVectorReadbackFailure();
    TestMsixProgramConfigVectorReadbackFailure();
    TestResetInProgressGating();
    TestMsixQuiesceResumeVectors();
    TestIntxQuiesceResume();
    TestMsixResumeVectorReadbackFailure();
    TestMsixQuiesceQueueVectorReadbackFailure();
    TestMsixQuiesceConfigVectorReadbackFailure();
    printf("virtio_pci_interrupts_host_tests: PASS\n");
    return 0;
}
