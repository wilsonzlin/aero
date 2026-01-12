/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

#include "ntddk.h"

static WDK_MMIO_READ_HANDLER g_mmio_read_handler = NULL;
static WDK_MMIO_WRITE_HANDLER g_mmio_write_handler = NULL;

static NTSTATUS g_IoConnectInterruptStatus = STATUS_SUCCESS;
/*
 * Deterministic monotonic "interrupt time" for host tests.
 *
 * Windows returns time in 100ns units. We advance it in stubs that conceptually
 * wait/sleep so loops that poll based on KeQueryInterruptTime() remain finite.
 */
static ULONGLONG g_interrupt_time_100ns = 0;

VOID WdkSetMmioHandlers(_In_opt_ WDK_MMIO_READ_HANDLER ReadHandler, _In_opt_ WDK_MMIO_WRITE_HANDLER WriteHandler)
{
    g_mmio_read_handler = ReadHandler;
    g_mmio_write_handler = WriteHandler;
}

VOID WdkTestSetIoConnectInterruptStatus(_In_ NTSTATUS Status)
{
    g_IoConnectInterruptStatus = Status;
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
    free(InterruptObject);
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

    if (Dpc->Inserted != FALSE) {
        return FALSE;
    }

    Dpc->Inserted = TRUE;
    Dpc->SystemArgument1 = SystemArgument1;
    Dpc->SystemArgument2 = SystemArgument2;
    return TRUE;
}

BOOLEAN KeRemoveQueueDpc(_Inout_ PKDPC Dpc)
{
    if (Dpc == NULL) {
        return FALSE;
    }

    if (Dpc->Inserted == FALSE) {
        return FALSE;
    }

    Dpc->Inserted = FALSE;
    Dpc->SystemArgument1 = NULL;
    Dpc->SystemArgument2 = NULL;
    return TRUE;
}

KIRQL KeGetCurrentIrql(VOID)
{
    return PASSIVE_LEVEL;
}

NTSTATUS KeDelayExecutionThread(_In_ KPROCESSOR_MODE WaitMode, _In_ BOOLEAN Alertable, _In_opt_ PLARGE_INTEGER Interval)
{
    (void)WaitMode;
    (void)Alertable;

    if (Interval != NULL) {
        /*
         * Negative values are relative 100ns intervals.
         * Positive values (absolute time) are not modeled; treat as no-op.
         */
        if (Interval->QuadPart < 0) {
            g_interrupt_time_100ns += (ULONGLONG)(-Interval->QuadPart);
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
    va_start(ap, Format);
    (void)vfprintf(stderr, Format, ap);
    va_end(ap);
    return 0;
}

BOOLEAN WdkTestTriggerInterrupt(_In_ PKINTERRUPT InterruptObject)
{
    if (InterruptObject == NULL || InterruptObject->ServiceRoutine == NULL) {
        return FALSE;
    }

    return InterruptObject->ServiceRoutine(InterruptObject, InterruptObject->ServiceContext);
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
