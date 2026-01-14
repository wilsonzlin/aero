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

/* Last IoConnectInterruptEx parameters (CONNECT_MESSAGE_BASED) for unit tests. */
static PDEVICE_OBJECT g_last_io_connect_interrupt_ex_pdo = NULL;
static ULONG g_last_io_connect_interrupt_ex_message_count = 0;
static ULONG g_last_io_connect_interrupt_ex_sync_irql = 0;
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

static WDK_TEST_IO_CONNECT_INTERRUPT_EX_HOOK g_test_io_connect_interrupt_ex_hook = NULL;
static PVOID g_test_io_connect_interrupt_ex_hook_ctx = NULL;

static WDK_TEST_KE_INSERT_QUEUE_DPC_HOOK g_test_ke_insert_queue_dpc_hook = NULL;
static PVOID g_test_ke_insert_queue_dpc_hook_ctx = NULL;

/*
 * Controllable HalGetBusDataByOffset(PCIConfiguration) stub state.
 *
 * virtio_pci_contract.c reads the first 0x30 bytes of PCI config space and
 * expects HalGetBusDataByOffset() to return the requested length on success.
 */
typedef struct _WDK_TEST_PCI_CFG_ENTRY {
    BOOLEAN InUse;
    ULONG BusNumber;
    ULONG SlotNumber;
    UCHAR Cfg[256];
    ULONG CfgLen;
    ULONG BytesRead;
} WDK_TEST_PCI_CFG_ENTRY;

enum {
    WDK_TEST_PCI_CFG_MAX_ENTRIES = 8,
};

static WDK_TEST_PCI_CFG_ENTRY g_test_pci_cfg_entries[WDK_TEST_PCI_CFG_MAX_ENTRIES];

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

PDEVICE_OBJECT WdkTestGetLastIoConnectInterruptExPhysicalDeviceObject(VOID)
{
    return g_last_io_connect_interrupt_ex_pdo;
}

ULONG WdkTestGetLastIoConnectInterruptExMessageCount(VOID)
{
    return g_last_io_connect_interrupt_ex_message_count;
}

ULONG WdkTestGetLastIoConnectInterruptExSynchronizeIrql(VOID)
{
    return g_last_io_connect_interrupt_ex_sync_irql;
}

VOID WdkTestResetLastIoConnectInterruptExParams(VOID)
{
    g_last_io_connect_interrupt_ex_pdo = NULL;
    g_last_io_connect_interrupt_ex_message_count = 0;
    g_last_io_connect_interrupt_ex_sync_irql = 0;
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

    g_last_io_connect_interrupt_ex_pdo = Parameters->MessageBased.PhysicalDeviceObject;
    g_last_io_connect_interrupt_ex_message_count = Parameters->MessageBased.MessageCount;
    g_last_io_connect_interrupt_ex_sync_irql = Parameters->MessageBased.SynchronizeIrql;

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
        intr->Irql = (KIRQL)Parameters->MessageBased.SynchronizeIrql;
        intr->SynchronizeIrql = (KIRQL)Parameters->MessageBased.SynchronizeIrql;
        intr->InterruptMode = LevelSensitive;
        intr->ShareVector = FALSE;
        intr->ProcessorEnableMask = 1;

        info->MessageInfo[i].InterruptObject = intr;
        /*
         * Simulate realistic MSI/MSI-X message data values (APIC vectors), which
         * are not the same as the MSI-X table entry indices ("message numbers").
         *
         * Unit tests for virtio MSI-X routing must ensure production code does
         * not accidentally treat MessageData as a virtio MSI-X vector index.
         */
        info->MessageInfo[i].MessageData = 0x50u + i;
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

    if (g_test_io_connect_interrupt_ex_hook != NULL) {
        g_test_io_connect_interrupt_ex_hook(Parameters, g_test_io_connect_interrupt_ex_hook_ctx);
    }

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

VOID WdkTestSetIoConnectInterruptExHook(_In_opt_ WDK_TEST_IO_CONNECT_INTERRUPT_EX_HOOK Hook, _In_opt_ PVOID Context)
{
    g_test_io_connect_interrupt_ex_hook = Hook;
    g_test_io_connect_interrupt_ex_hook_ctx = Context;
}

VOID WdkTestClearIoConnectInterruptExHook(VOID)
{
    g_test_io_connect_interrupt_ex_hook = NULL;
    g_test_io_connect_interrupt_ex_hook_ctx = NULL;
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

VOID WdkTestPciReset(VOID)
{
    memset(g_test_pci_cfg_entries, 0, sizeof(g_test_pci_cfg_entries));
}

VOID WdkTestPciSetSlotConfig(_In_ ULONG BusNumber,
                             _In_ ULONG SlotNumber,
                             _In_reads_bytes_(CfgLen) const VOID* Cfg,
                             _In_ ULONG CfgLen,
                             _In_ ULONG BytesRead)
{
    size_t i;
    WDK_TEST_PCI_CFG_ENTRY* slot = NULL;
    ULONG copy_len;

    if (Cfg == NULL || CfgLen == 0) {
        return;
    }

    /* Update existing entry if present. */
    for (i = 0; i < (size_t)WDK_TEST_PCI_CFG_MAX_ENTRIES; i++) {
        if (g_test_pci_cfg_entries[i].InUse != FALSE && g_test_pci_cfg_entries[i].BusNumber == BusNumber &&
            g_test_pci_cfg_entries[i].SlotNumber == SlotNumber) {
            slot = &g_test_pci_cfg_entries[i];
            break;
        }
    }

    /* Otherwise allocate a new entry. */
    if (slot == NULL) {
        for (i = 0; i < (size_t)WDK_TEST_PCI_CFG_MAX_ENTRIES; i++) {
            if (g_test_pci_cfg_entries[i].InUse == FALSE) {
                slot = &g_test_pci_cfg_entries[i];
                break;
            }
        }
    }

    if (slot == NULL) {
        /* Test suite exceeded stub capacity. */
        abort();
    }

    memset(slot, 0, sizeof(*slot));
    slot->InUse = TRUE;
    slot->BusNumber = BusNumber;
    slot->SlotNumber = SlotNumber;

    copy_len = CfgLen;
    if (copy_len > (ULONG)sizeof(slot->Cfg)) {
        copy_len = (ULONG)sizeof(slot->Cfg);
    }

    memcpy(slot->Cfg, Cfg, copy_len);
    slot->CfgLen = copy_len;
    slot->BytesRead = BytesRead;
}

ULONG HalGetBusDataByOffset(BUS_DATA_TYPE BusDataType,
                            ULONG BusNumber,
                            ULONG SlotNumber,
                            PVOID Buffer,
                            ULONG Offset,
                            ULONG Length)
{
    size_t i;
    WDK_TEST_PCI_CFG_ENTRY* slot = NULL;
    ULONG available;
    ULONG bytes_to_copy;

    (void)BusDataType;

    if (Buffer == NULL) {
        return 0;
    }

    for (i = 0; i < (size_t)WDK_TEST_PCI_CFG_MAX_ENTRIES; i++) {
        if (g_test_pci_cfg_entries[i].InUse != FALSE && g_test_pci_cfg_entries[i].BusNumber == BusNumber &&
            g_test_pci_cfg_entries[i].SlotNumber == SlotNumber) {
            slot = &g_test_pci_cfg_entries[i];
            break;
        }
    }

    if (slot == NULL) {
        return 0;
    }

    if (Offset >= slot->CfgLen) {
        return 0;
    }

    available = slot->CfgLen - Offset;
    bytes_to_copy = slot->BytesRead;
    if (bytes_to_copy > Length) {
        bytes_to_copy = Length;
    }
    if (bytes_to_copy > available) {
        bytes_to_copy = available;
    }

    memcpy(Buffer, slot->Cfg + Offset, bytes_to_copy);
    return bytes_to_copy;
}

NTSTATUS IoGetDeviceProperty(PDEVICE_OBJECT DeviceObject,
                             DEVICE_REGISTRY_PROPERTY DeviceProperty,
                             ULONG BufferLength,
                             PVOID PropertyBuffer,
                             ULONG* ResultLength)
{
    ULONG v;
    ULONG len;
    NTSTATUS st;

    if (DeviceObject == NULL || ResultLength == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    v = 0;
    len = (ULONG)sizeof(ULONG);
    st = STATUS_SUCCESS;

    switch (DeviceProperty) {
    case DevicePropertyBusNumber:
        v = DeviceObject->BusNumber;
        st = DeviceObject->BusNumberStatus;
        if (DeviceObject->BusNumberResultLength != 0) {
            len = DeviceObject->BusNumberResultLength;
        }
        break;
    case DevicePropertyAddress:
        v = DeviceObject->Address;
        st = DeviceObject->AddressStatus;
        if (DeviceObject->AddressResultLength != 0) {
            len = DeviceObject->AddressResultLength;
        }
        break;
    default:
        *ResultLength = 0;
        return STATUS_NOT_SUPPORTED;
    }

    *ResultLength = len;

    if (!NT_SUCCESS(st)) {
        return st;
    }

    if (PropertyBuffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (BufferLength < len) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    /* Only the ULONG-sized bus/address values are modeled by this stub. */
    {
        ULONG copy_len;
        copy_len = len;
        if (copy_len > (ULONG)sizeof(v)) {
            copy_len = (ULONG)sizeof(v);
        }
        memcpy(PropertyBuffer, &v, copy_len);
    }
    return STATUS_SUCCESS;
}

static char* WdkDbgPrintExSanitizeFormatString(const char* Format)
{
    const char* needle = "%!STATUS!";
    const char* repl = "0x%08x";
    const char* p;
    size_t count;
    size_t needle_len;
    size_t repl_len;
    size_t new_len;
    char* out;
    char* w;

    if (Format == NULL) {
        return NULL;
    }

    needle_len = strlen(needle);
    repl_len = strlen(repl);

    count = 0;
    p = Format;
    while ((p = strstr(p, needle)) != NULL) {
        count++;
        p += needle_len;
    }

    if (count == 0) {
        return NULL;
    }

    new_len = strlen(Format) + count * (repl_len - needle_len) + 1;
    out = (char*)malloc(new_len);
    if (out == NULL) {
        return NULL;
    }

    w = out;
    p = Format;
    while (*p != '\0') {
        if (strncmp(p, needle, needle_len) == 0) {
            memcpy(w, repl, repl_len);
            w += repl_len;
            p += needle_len;
        } else {
            *w++ = *p++;
        }
    }
    *w = '\0';
    return out;
}

ULONG DbgPrintEx(_In_ ULONG ComponentId, _In_ ULONG Level, _In_ const char* Format, ...)
{
    va_list ap;
    char* sanitized_fmt;

    (void)ComponentId;
    (void)Level;

    if (Format == NULL) {
        return 0;
    }

    /* Keep output available when running tests with --output-on-failure. */
    g_dbg_print_ex_count++;
    va_start(ap, Format);
    sanitized_fmt = NULL;
    if (strstr(Format, "%!STATUS!") != NULL) {
        sanitized_fmt = WdkDbgPrintExSanitizeFormatString(Format);
        if (sanitized_fmt == NULL) {
            /* Unknown / WDK-specific format string: avoid UB in vfprintf. */
            (void)fputs(Format, stderr);
            va_end(ap);
            return 0;
        }
    }
    (void)vfprintf(stderr, sanitized_fmt != NULL ? sanitized_fmt : Format, ap);
    va_end(ap);
    free(sanitized_fmt);
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
    KIRQL old_irql;

    if (InterruptObject == NULL || InterruptObject->ServiceRoutine == NULL) {
        return FALSE;
    }

    /*
     * In Windows, the ISR runs at DIRQL (approximated here by the interrupt's
     * configured IRQL). Many code paths change behaviour based on KeGetCurrentIrql(),
     * so model that for host tests.
     */
    old_irql = g_current_irql;
    g_current_irql = InterruptObject->Irql;
    BOOLEAN claimed = InterruptObject->ServiceRoutine(InterruptObject, InterruptObject->ServiceContext);
    g_current_irql = old_irql;
    return claimed;
}

BOOLEAN WdkTestTriggerMessageInterrupt(_In_ PIO_INTERRUPT_MESSAGE_INFO MessageInfo, _In_ ULONG MessageId)
{
    PKINTERRUPT intr;
    KIRQL old_irql;
    BOOLEAN claimed;

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

    /*
     * In Windows, the message-based ISR runs at DIRQL. Model this by temporarily
     * raising KeGetCurrentIrql() to the interrupt's IRQL while calling the ISR.
     */
    old_irql = g_current_irql;
    g_current_irql = intr->Irql;
    claimed = intr->MessageServiceRoutine(intr, intr->ServiceContext, MessageId);
    g_current_irql = old_irql;
    return claimed;
}

BOOLEAN WdkTestRunQueuedDpc(_Inout_ PKDPC Dpc)
{
    PKDEFERRED_ROUTINE routine;
    PVOID context;
    PVOID arg1;
    PVOID arg2;
    KIRQL old_irql;

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

    /*
     * DPCs run at DISPATCH_LEVEL. Some production code uses KeGetCurrentIrql()
     * checks to select safe wait/synchronization primitives, so emulate that for
     * host tests.
     */
    old_irql = g_current_irql;
    g_current_irql = DISPATCH_LEVEL;
    routine(Dpc, context, arg1, arg2);
    g_current_irql = old_irql;
    return TRUE;
}
