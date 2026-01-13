/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

#include "ntddk.h"

static WDK_MMIO_READ_HANDLER g_mmio_read_handler = NULL;
static WDK_MMIO_WRITE_HANDLER g_mmio_write_handler = NULL;

static NTSTATUS g_IoConnectInterruptStatus = STATUS_SUCCESS;
static NTSTATUS g_IoConnectInterruptExStatus = STATUS_SUCCESS;
static KIRQL g_current_irql = PASSIVE_LEVEL;
/*
 * Deterministic monotonic "interrupt time" for host tests.
 *
 * Windows returns time in 100ns units. We advance it in stubs that conceptually
 * wait/sleep so loops that poll based on KeQueryInterruptTime() remain finite.
 */
static ULONGLONG g_interrupt_time_100ns = 0;
static ULONG g_dbg_print_ex_count = 0;
static ULONG g_io_connect_interrupt_count = 0;
static ULONG g_io_disconnect_interrupt_count = 0;
static ULONG g_io_connect_interrupt_ex_count = 0;
static ULONG g_io_disconnect_interrupt_ex_count = 0;
static ULONG g_ke_delay_execution_thread_count = 0;
static ULONG g_ke_stall_execution_processor_count = 0;
static ULONG g_ke_insert_queue_dpc_count = 0;
static ULONG g_ke_insert_queue_dpc_success_count = 0;
static ULONG g_ke_insert_queue_dpc_fail_count = 0;
static ULONG g_ke_remove_queue_dpc_count = 0;
static ULONG g_ke_remove_queue_dpc_success_count = 0;
static ULONG g_ke_remove_queue_dpc_fail_count = 0;

static volatile LONG* g_test_auto_complete_dpc_inflight_ptr = NULL;
static ULONG g_test_auto_complete_dpc_inflight_after_delay_calls = 0;

static WDK_TEST_KE_INSERT_QUEUE_DPC_HOOK g_test_ke_insert_queue_dpc_hook = NULL;
static PVOID g_test_ke_insert_queue_dpc_hook_ctx = NULL;

VOID WdkSetMmioHandlers(_In_opt_ WDK_MMIO_READ_HANDLER ReadHandler, _In_opt_ WDK_MMIO_WRITE_HANDLER WriteHandler)
{
    g_mmio_read_handler = ReadHandler;
    g_mmio_write_handler = WriteHandler;
}

VOID WdkTestSetIoConnectInterruptStatus(_In_ NTSTATUS Status)
{
    g_IoConnectInterruptStatus = Status;
}

VOID WdkTestSetIoConnectInterruptExStatus(_In_ NTSTATUS Status)
{
    g_IoConnectInterruptExStatus = Status;
}

BOOLEAN WdkMmioRead(_In_ const volatile VOID* Register, _In_ size_t Width, _Out_ ULONGLONG* ValueOut)
{
    if (ValueOut == NULL) {
        return FALSE;
    }

    if (g_mmio_read_handler == NULL) {
        return FALSE;
    }

    return g_mmio_read_handler(Register, Width, ValueOut);
}

BOOLEAN WdkMmioWrite(_In_ volatile VOID* Register, _In_ size_t Width, _In_ ULONGLONG Value)
{
    if (g_mmio_write_handler == NULL) {
        return FALSE;
    }

    return g_mmio_write_handler(Register, Width, Value);
}

NTSTATUS IoConnectInterrupt(_Out_ PKINTERRUPT* InterruptObject,
                            _In_ PKSERVICE_ROUTINE ServiceRoutine,
                            _In_ PVOID ServiceContext,
                            _In_opt_ PVOID SpinLock,
                            _In_ ULONG Vector,
                            _In_ KIRQL Irql,
                            _In_ KIRQL SynchronizeIrql,
                            _In_ KINTERRUPT_MODE InterruptMode,
                            _In_ BOOLEAN ShareVector,
                            _In_ KAFFINITY ProcessorEnableMask,
                            _In_ BOOLEAN FloatingSave)
{
    KINTERRUPT* intr;

    (void)SpinLock;
    (void)Vector;
    (void)Irql;
    (void)SynchronizeIrql;
    (void)InterruptMode;
    (void)ShareVector;
    (void)ProcessorEnableMask;
    (void)FloatingSave;

    if (InterruptObject == NULL || ServiceRoutine == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    g_io_connect_interrupt_count++;

    if (!NT_SUCCESS(g_IoConnectInterruptStatus)) {
        return g_IoConnectInterruptStatus;
    }

    intr = (KINTERRUPT*)calloc(1, sizeof(*intr));
    if (intr == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    intr->ServiceRoutine = ServiceRoutine;
    intr->ServiceContext = ServiceContext;
    intr->Vector = Vector;
    intr->Irql = Irql;
    intr->SynchronizeIrql = SynchronizeIrql;
    intr->InterruptMode = InterruptMode;
    intr->ShareVector = ShareVector;
    intr->ProcessorEnableMask = ProcessorEnableMask;
    *InterruptObject = intr;

    return STATUS_SUCCESS;
}

VOID IoDisconnectInterrupt(_In_ PKINTERRUPT InterruptObject)
{
    g_io_disconnect_interrupt_count++;
    free(InterruptObject);
}

PVOID ExAllocatePoolWithTag(_In_ POOL_TYPE PoolType, _In_ size_t NumberOfBytes, _In_ ULONG Tag)
{
    (void)PoolType;
    (void)Tag;
    return calloc(1, NumberOfBytes);
}

VOID ExFreePoolWithTag(_In_ PVOID P, _In_ ULONG Tag)
{
    (void)Tag;
    free(P);
}

typedef struct _WDK_MESSAGE_INTERRUPT_CONNECTION {
    IO_INTERRUPT_MESSAGE_INFO* MessageInfo;
} WDK_MESSAGE_INTERRUPT_CONNECTION;

NTSTATUS IoConnectInterruptEx(_Inout_ PIO_CONNECT_INTERRUPT_PARAMETERS Parameters)
{
    ULONG i;
    ULONG messageCount;
    IO_INTERRUPT_MESSAGE_INFO* info;
    WDK_MESSAGE_INTERRUPT_CONNECTION* connection;

    if (Parameters == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    g_io_connect_interrupt_ex_count++;

    if (!NT_SUCCESS(g_IoConnectInterruptExStatus)) {
        return g_IoConnectInterruptExStatus;
    }

    if (Parameters->Version != CONNECT_MESSAGE_BASED) {
        return STATUS_NOT_SUPPORTED;
    }

    messageCount = Parameters->MessageBased.MessageCount;
    if (messageCount == 0 || Parameters->MessageBased.ServiceRoutine == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Allocate the message info structure (ANYSIZE_ARRAY pattern). */
    info = (IO_INTERRUPT_MESSAGE_INFO*)calloc(1,
                                              sizeof(*info) + (size_t)(messageCount - 1) * sizeof(info->MessageInfo[0]));
    if (info == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    info->MessageCount = messageCount;

    for (i = 0; i < messageCount; i++) {
        KINTERRUPT* intr = (KINTERRUPT*)calloc(1, sizeof(*intr));
        if (intr == NULL) {
            for (ULONG j = 0; j < i; j++) {
                free(info->MessageInfo[j].InterruptObject);
            }
            free(info);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        intr->MessageServiceRoutine = Parameters->MessageBased.ServiceRoutine;
        intr->ServiceContext = Parameters->MessageBased.ServiceContext;
        intr->Vector = i;
        intr->Irql = (KIRQL)0;
        intr->SynchronizeIrql = (KIRQL)0;
        intr->InterruptMode = LevelSensitive;
        intr->ShareVector = FALSE;
        intr->ProcessorEnableMask = 1;

        info->MessageInfo[i].InterruptObject = intr;
        info->MessageInfo[i].MessageData = i;
    }

    connection = (WDK_MESSAGE_INTERRUPT_CONNECTION*)calloc(1, sizeof(*connection));
    if (connection == NULL) {
        for (i = 0; i < messageCount; i++) {
            free(info->MessageInfo[i].InterruptObject);
        }
        free(info);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    connection->MessageInfo = info;

    Parameters->MessageBased.MessageInfo = info;
    Parameters->MessageBased.ConnectionContext = connection;

    return STATUS_SUCCESS;
}

VOID IoDisconnectInterruptEx(_In_ PIO_DISCONNECT_INTERRUPT_PARAMETERS Parameters)
{
    WDK_MESSAGE_INTERRUPT_CONNECTION* connection;

    if (Parameters == NULL) {
        return;
    }

    g_io_disconnect_interrupt_ex_count++;

    if (Parameters->Version != CONNECT_MESSAGE_BASED) {
        return;
    }

    connection = (WDK_MESSAGE_INTERRUPT_CONNECTION*)Parameters->MessageBased.ConnectionContext;
    if (connection == NULL) {
        return;
    }

    if (connection->MessageInfo != NULL) {
        for (ULONG i = 0; i < connection->MessageInfo->MessageCount; i++) {
            free(connection->MessageInfo->MessageInfo[i].InterruptObject);
        }
        free(connection->MessageInfo);
        connection->MessageInfo = NULL;
    }

    free(connection);
}

VOID KeInitializeDpc(_Out_ PKDPC Dpc, _In_ PKDEFERRED_ROUTINE DeferredRoutine, _In_opt_ PVOID DeferredContext)
{
    if (Dpc == NULL) {
        return;
    }

    Dpc->DeferredRoutine = DeferredRoutine;
    Dpc->DeferredContext = DeferredContext;
    Dpc->SystemArgument1 = NULL;
    Dpc->SystemArgument2 = NULL;
    Dpc->Inserted = FALSE;
}

BOOLEAN KeInsertQueueDpc(_Inout_ PKDPC Dpc, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2)
{
    if (Dpc == NULL) {
        return FALSE;
    }

    g_ke_insert_queue_dpc_count++;

    if (g_test_ke_insert_queue_dpc_hook != NULL) {
        g_test_ke_insert_queue_dpc_hook(Dpc, SystemArgument1, SystemArgument2, g_test_ke_insert_queue_dpc_hook_ctx);
    }

    if (Dpc->Inserted != FALSE) {
        g_ke_insert_queue_dpc_fail_count++;
        return FALSE;
    }

    Dpc->Inserted = TRUE;
    Dpc->SystemArgument1 = SystemArgument1;
    Dpc->SystemArgument2 = SystemArgument2;
    g_ke_insert_queue_dpc_success_count++;
    return TRUE;
}

BOOLEAN KeRemoveQueueDpc(_Inout_ PKDPC Dpc)
{
    if (Dpc == NULL) {
        return FALSE;
    }

    g_ke_remove_queue_dpc_count++;

    if (Dpc->Inserted == FALSE) {
        g_ke_remove_queue_dpc_fail_count++;
        return FALSE;
    }

    Dpc->Inserted = FALSE;
    Dpc->SystemArgument1 = NULL;
    Dpc->SystemArgument2 = NULL;
    g_ke_remove_queue_dpc_success_count++;
    return TRUE;
}

KIRQL KeGetCurrentIrql(VOID)
{
    return g_current_irql;
}

VOID WdkTestSetCurrentIrql(_In_ KIRQL Irql)
{
    g_current_irql = Irql;
}

ULONG WdkTestGetDbgPrintExCount(VOID)
{
    return g_dbg_print_ex_count;
}

VOID WdkTestResetDbgPrintExCount(VOID)
{
    g_dbg_print_ex_count = 0;
}

VOID WdkTestAutoCompleteDpcInFlightAfterDelayCalls(_Inout_ volatile LONG* DpcInFlight, _In_ ULONG DelayCallCount)
{
    g_test_auto_complete_dpc_inflight_ptr = DpcInFlight;
    g_test_auto_complete_dpc_inflight_after_delay_calls = DelayCallCount;
}

VOID WdkTestClearAutoCompleteDpcInFlight(VOID)
{
    g_test_auto_complete_dpc_inflight_ptr = NULL;
    g_test_auto_complete_dpc_inflight_after_delay_calls = 0;
}

VOID WdkTestSetKeInsertQueueDpcHook(_In_opt_ WDK_TEST_KE_INSERT_QUEUE_DPC_HOOK Hook, _In_opt_ PVOID Context)
{
    g_test_ke_insert_queue_dpc_hook = Hook;
    g_test_ke_insert_queue_dpc_hook_ctx = Context;
}

VOID WdkTestClearKeInsertQueueDpcHook(VOID)
{
    g_test_ke_insert_queue_dpc_hook = NULL;
    g_test_ke_insert_queue_dpc_hook_ctx = NULL;
}

NTSTATUS KeDelayExecutionThread(_In_ KPROCESSOR_MODE WaitMode, _In_ BOOLEAN Alertable, _In_opt_ PLARGE_INTEGER Interval)
{
    (void)WaitMode;
    (void)Alertable;

    g_ke_delay_execution_thread_count++;

    if (Interval != NULL) {
        /*
         * Negative values are relative 100ns intervals.
         * Positive values (absolute time) are not modeled; treat as no-op.
         */
        if (Interval->QuadPart < 0) {
            g_interrupt_time_100ns += (ULONGLONG)(-Interval->QuadPart);
        }
    }

    if (g_test_auto_complete_dpc_inflight_ptr != NULL && g_test_auto_complete_dpc_inflight_after_delay_calls != 0) {
        g_test_auto_complete_dpc_inflight_after_delay_calls--;
        if (g_test_auto_complete_dpc_inflight_after_delay_calls == 0) {
            __atomic_store_n((LONG*)g_test_auto_complete_dpc_inflight_ptr, 0, __ATOMIC_SEQ_CST);
            g_test_auto_complete_dpc_inflight_ptr = NULL;
        }
    }

    return STATUS_SUCCESS;
}

ULONGLONG KeQueryInterruptTime(VOID)
{
    /*
     * If nothing advances time (e.g. a tight poll loop), still ensure forward
     * progress so such loops terminate deterministically. This mirrors the fact
     * that time always advances on a real system.
     */
    g_interrupt_time_100ns += 1000ull; /* 100us */
    return g_interrupt_time_100ns;
}

ULONG DbgPrintEx(_In_ ULONG ComponentId, _In_ ULONG Level, _In_ const char* Format, ...)
{
    va_list ap;

    (void)ComponentId;
    (void)Level;

    if (Format == NULL) {
        return 0;
    }

    /* Keep output available when running tests with --output-on-failure. */
    g_dbg_print_ex_count++;
    va_start(ap, Format);
    (void)vfprintf(stderr, Format, ap);
    va_end(ap);
    return 0;
}

VOID WdkTestOnKeStallExecutionProcessor(_In_ ULONG Microseconds)
{
    g_ke_stall_execution_processor_count++;
    g_interrupt_time_100ns += (ULONGLONG)Microseconds * 10ull;
}

ULONG WdkTestGetKeDelayExecutionThreadCount(VOID)
{
    return g_ke_delay_execution_thread_count;
}

VOID WdkTestResetKeDelayExecutionThreadCount(VOID)
{
    g_ke_delay_execution_thread_count = 0;
}

ULONG WdkTestGetKeStallExecutionProcessorCount(VOID)
{
    return g_ke_stall_execution_processor_count;
}

VOID WdkTestResetKeStallExecutionProcessorCount(VOID)
{
    g_ke_stall_execution_processor_count = 0;
}

ULONG WdkTestGetIoConnectInterruptCount(VOID)
{
    return g_io_connect_interrupt_count;
}

VOID WdkTestResetIoConnectInterruptCount(VOID)
{
    g_io_connect_interrupt_count = 0;
}

ULONG WdkTestGetIoDisconnectInterruptCount(VOID)
{
    return g_io_disconnect_interrupt_count;
}

VOID WdkTestResetIoDisconnectInterruptCount(VOID)
{
    g_io_disconnect_interrupt_count = 0;
}

ULONG WdkTestGetIoConnectInterruptExCount(VOID)
{
    return g_io_connect_interrupt_ex_count;
}

VOID WdkTestResetIoConnectInterruptExCount(VOID)
{
    g_io_connect_interrupt_ex_count = 0;
}

ULONG WdkTestGetIoDisconnectInterruptExCount(VOID)
{
    return g_io_disconnect_interrupt_ex_count;
}

VOID WdkTestResetIoDisconnectInterruptExCount(VOID)
{
    g_io_disconnect_interrupt_ex_count = 0;
}

ULONG WdkTestGetKeInsertQueueDpcCount(VOID)
{
    return g_ke_insert_queue_dpc_count;
}

ULONG WdkTestGetKeInsertQueueDpcSuccessCount(VOID)
{
    return g_ke_insert_queue_dpc_success_count;
}

ULONG WdkTestGetKeInsertQueueDpcFailCount(VOID)
{
    return g_ke_insert_queue_dpc_fail_count;
}

VOID WdkTestResetKeInsertQueueDpcCounts(VOID)
{
    g_ke_insert_queue_dpc_count = 0;
    g_ke_insert_queue_dpc_success_count = 0;
    g_ke_insert_queue_dpc_fail_count = 0;
}

ULONG WdkTestGetKeRemoveQueueDpcCount(VOID)
{
    return g_ke_remove_queue_dpc_count;
}

ULONG WdkTestGetKeRemoveQueueDpcSuccessCount(VOID)
{
    return g_ke_remove_queue_dpc_success_count;
}

ULONG WdkTestGetKeRemoveQueueDpcFailCount(VOID)
{
    return g_ke_remove_queue_dpc_fail_count;
}

VOID WdkTestResetKeRemoveQueueDpcCounts(VOID)
{
    g_ke_remove_queue_dpc_count = 0;
    g_ke_remove_queue_dpc_success_count = 0;
    g_ke_remove_queue_dpc_fail_count = 0;
}

BOOLEAN WdkTestTriggerInterrupt(_In_ PKINTERRUPT InterruptObject)
{
    if (InterruptObject == NULL || InterruptObject->ServiceRoutine == NULL) {
        return FALSE;
    }

    return InterruptObject->ServiceRoutine(InterruptObject, InterruptObject->ServiceContext);
}

BOOLEAN WdkTestTriggerMessageInterrupt(_In_ PIO_INTERRUPT_MESSAGE_INFO MessageInfo, _In_ ULONG MessageId)
{
    PKINTERRUPT intr;

    if (MessageInfo == NULL) {
        return FALSE;
    }

    if (MessageId >= MessageInfo->MessageCount) {
        return FALSE;
    }

    intr = MessageInfo->MessageInfo[MessageId].InterruptObject;
    if (intr == NULL || intr->MessageServiceRoutine == NULL) {
        return FALSE;
    }

    return intr->MessageServiceRoutine(intr, intr->ServiceContext, MessageId);
}

BOOLEAN WdkTestRunQueuedDpc(_Inout_ PKDPC Dpc)
{
    PKDEFERRED_ROUTINE routine;
    PVOID context;
    PVOID arg1;
    PVOID arg2;

    if (Dpc == NULL) {
        return FALSE;
    }

    if (Dpc->Inserted == FALSE) {
        return FALSE;
    }

    routine = Dpc->DeferredRoutine;
    context = Dpc->DeferredContext;
    arg1 = Dpc->SystemArgument1;
    arg2 = Dpc->SystemArgument2;

    Dpc->Inserted = FALSE;
    Dpc->SystemArgument1 = NULL;
    Dpc->SystemArgument2 = NULL;

    if (routine == NULL) {
        return FALSE;
    }

    routine(Dpc, context, arg1, arg2);
    return TRUE;
}
