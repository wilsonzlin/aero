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
    _In_ ULONG MessageCount)
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

    st = VirtioPciInterruptsPrepareHardware(
        dev,
        Interrupts,
        &rawList,
        &transList,
        QueueCount,
        NULL, /* ISR status register is INTx-only. */
        NULL,
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
    PrepareMsix(&interrupts, &dev, &cb, 2, 3 /* config + 2 queues */);

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

    Cleanup(&interrupts, dev);

    /*
     * MSI-X: while reset is in progress, ISR should return TRUE but not queue
     * a DPC.
     */
    PrepareMsix(&interrupts, &dev, &cb, 2, 3);

    InterlockedExchange(&interrupts.ResetInProgress, 1);
    handled = interrupts.u.Msix.Interrupts[1]->Isr(interrupts.u.Msix.Interrupts[1], 0);
    assert(handled == TRUE);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueueCalls == 0);
    assert(interrupts.u.Msix.Interrupts[1]->DpcQueued == FALSE);
    assert(cb.ConfigCalls == 0);
    assert(cb.QueueCallsTotal == 0);

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

    Cleanup(&interrupts, dev);
}

int main(void)
{
    TestIntxSpuriousInterrupt();
    TestIntxRealInterruptDispatch();
    TestMsixDispatchAndRouting();
    TestResetInProgressGating();
    printf("virtio_pci_interrupts_host_tests: PASS\n");
    return 0;
}
