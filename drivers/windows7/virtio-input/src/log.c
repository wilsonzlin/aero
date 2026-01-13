#include "log.h"

// IOCTL_HID_* definitions (Win7 WDK).
#include <hidport.h>

#if VIOINPUT_DIAGNOSTICS

#include <ntstrsafe.h>
#include <stdarg.h>

// Global diagnostic mask (read-mostly). Accessed at DISPATCH_LEVEL.
static volatile ULONG g_VioInputDiagnosticsMask =
    VIOINPUT_LOG_ERROR | VIOINPUT_LOG_IOCTL | VIOINPUT_LOG_QUEUE | VIOINPUT_LOG_VIRTQ;

static __forceinline PCSTR VioInputMaskToCategory(_In_ ULONG Mask)
{
    if ((Mask & VIOINPUT_LOG_ERROR) != 0) {
        return "ERROR";
    }
    if ((Mask & VIOINPUT_LOG_IOCTL) != 0) {
        return "IOCTL";
    }
    if ((Mask & VIOINPUT_LOG_QUEUE) != 0) {
        return "QUEUE";
    }
    if ((Mask & VIOINPUT_LOG_VIRTQ) != 0) {
        return "VIRTQ";
    }
    return "GEN";
}

VOID VioInputLogInitialize(_In_ PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;
    ULONG mask = (ULONG)g_VioInputDiagnosticsMask;
    RTL_QUERY_REGISTRY_TABLE table[2];

    RtlZeroMemory(table, sizeof(table));

    table[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[0].Name = VIOINPUT_REG_DIAGNOSTICS_MASK;
    table[0].EntryContext = &mask;
    table[0].DefaultType = REG_DWORD;
    table[0].DefaultData = &mask;
    table[0].DefaultLength = sizeof(mask);

    status = RtlQueryRegistryValues(RTL_REGISTRY_ABSOLUTE, RegistryPath->Buffer, table, NULL, NULL);
    if (NT_SUCCESS(status)) {
        InterlockedExchange((volatile LONG*)&g_VioInputDiagnosticsMask, (LONG)mask);
    }

    // Always print the resulting mask in checked builds to aid bring-up.
    DbgPrintEx(
        DPFLTR_IHVDRIVER_ID,
        DPFLTR_INFO_LEVEL,
        "[vioinput] DiagnosticsMask=0x%08X (query status=%!STATUS!)\n",
        (ULONG)g_VioInputDiagnosticsMask,
        status);
}

VOID VioInputLogShutdown(VOID)
{
}

BOOLEAN VioInputLogEnabled(_In_ ULONG Mask)
{
    const ULONG enabled = (ULONG)g_VioInputDiagnosticsMask;
    const ULONG categories = VIOINPUT_LOG_IOCTL | VIOINPUT_LOG_QUEUE | VIOINPUT_LOG_VIRTQ;

    // Error messages are considered important enough to not depend on the category bits.
    // If the caller includes VIOINPUT_LOG_ERROR, only require that error logging is enabled.
    if ((Mask & VIOINPUT_LOG_ERROR) != 0) {
        return (enabled & VIOINPUT_LOG_ERROR) != 0;
    }

    // Verbose messages require explicit opt-in via VIOINPUT_LOG_VERBOSE.
    if (((Mask & VIOINPUT_LOG_VERBOSE) != 0) && ((enabled & VIOINPUT_LOG_VERBOSE) == 0)) {
        return FALSE;
    }

    // For non-error messages, require the corresponding category bit(s).
    if ((Mask & categories) != 0) {
        return (enabled & Mask & categories) != 0;
    }

    // Fallback: any matching bit enables the message.
    return (enabled & Mask) != 0;
}

ULONG VioInputLogGetMask(VOID)
{
    return (ULONG)InterlockedCompareExchange((volatile LONG*)&g_VioInputDiagnosticsMask, 0, 0);
}

ULONG VioInputLogSetMask(_In_ ULONG Mask)
{
    return (ULONG)InterlockedExchange((volatile LONG*)&g_VioInputDiagnosticsMask, (LONG)Mask);
}

VOID VioInputLogPrint(
    _In_ ULONG Mask,
    _In_z_ PCSTR Function,
    _In_ ULONG Line,
    _In_z_ _Printf_format_string_ PCSTR Format,
    ...)
{
    CHAR prefix[192];
    NTSTATUS status;
    va_list args;
    ULONG level;

    if (!VioInputLogEnabled(Mask)) {
        return;
    }

    level = ((Mask & VIOINPUT_LOG_ERROR) != 0) ? DPFLTR_ERROR_LEVEL : DPFLTR_INFO_LEVEL;

    status = RtlStringCbPrintfA(
        prefix,
        sizeof(prefix),
        "[vioinput][%s][%s:%lu] ",
        VioInputMaskToCategory(Mask),
        Function,
        Line);
    if (!NT_SUCCESS(status)) {
        // Prefix buffer should never be too small, but don't fail logging if it is.
        prefix[0] = '\0';
    }

    va_start(args, Format);
    vDbgPrintExWithPrefix(prefix, DPFLTR_IHVDRIVER_ID, level, Format, args);
    va_end(args);
}

#endif

VOID VioInputCountersInit(_Out_ PVIOINPUT_COUNTERS Counters)
{
    RtlZeroMemory(Counters, sizeof(*Counters));
    Counters->Size = sizeof(*Counters);
    Counters->Version = VIOINPUT_COUNTERS_VERSION;
}

VOID VioInputCountersSnapshot(_In_ const VIOINPUT_COUNTERS* Counters, _Out_ PVIOINPUT_COUNTERS Snapshot)
{
    // A best-effort snapshot for debugging. All fields are 32-bit and read atomically.
    RtlCopyMemory(Snapshot, Counters, sizeof(*Snapshot));
}

VOID VioInputCountersReset(_Inout_ PVIOINPUT_COUNTERS Counters)
{
    LONG readDepth;
    LONG ringDepth;
    LONG virtioDepth;
    LONG pendingDepth;

    if (Counters == NULL) {
        return;
    }

    /*
     * Reset the monotonic counters, but preserve the "current depth" fields.
     *
     * ReadReportQueueDepth / ReportRingDepth / PendingRingDepth / VirtioQueueDepth
     * are intended to
     * reflect instantaneous state. Zeroing them while there are pending IRPs or
     * buffered reports can cause confusing negative values after subsequent
     * decrements. Instead, keep the current depths and reset the corresponding
     * maxima to the current value.
     *
     * This is a best-effort diagnostic reset; counters may change concurrently.
     */
    readDepth = Counters->ReadReportQueueDepth;
    ringDepth = Counters->ReportRingDepth;
    pendingDepth = Counters->PendingRingDepth;
    virtioDepth = Counters->VirtioQueueDepth;

    InterlockedExchange(&Counters->IoctlTotal, 0);
    InterlockedExchange(&Counters->IoctlUnknown, 0);

    InterlockedExchange(&Counters->IoctlHidGetDeviceDescriptor, 0);
    InterlockedExchange(&Counters->IoctlHidGetReportDescriptor, 0);
    InterlockedExchange(&Counters->IoctlHidGetDeviceAttributes, 0);
    InterlockedExchange(&Counters->IoctlHidGetCollectionInformation, 0);
    InterlockedExchange(&Counters->IoctlHidGetCollectionDescriptor, 0);
    InterlockedExchange(&Counters->IoctlHidFlushQueue, 0);
    InterlockedExchange(&Counters->IoctlHidGetString, 0);
    InterlockedExchange(&Counters->IoctlHidGetIndexedString, 0);
    InterlockedExchange(&Counters->IoctlHidGetFeature, 0);
    InterlockedExchange(&Counters->IoctlHidSetFeature, 0);
    InterlockedExchange(&Counters->IoctlHidGetInputReport, 0);
    InterlockedExchange(&Counters->IoctlHidSetOutputReport, 0);
    InterlockedExchange(&Counters->IoctlHidReadReport, 0);
    InterlockedExchange(&Counters->IoctlHidWriteReport, 0);

    InterlockedExchange(&Counters->ReadReportPended, 0);
    InterlockedExchange(&Counters->ReadReportCompleted, 0);
    InterlockedExchange(&Counters->ReadReportCancelled, 0);

    InterlockedExchange(&Counters->ReadReportQueueMaxDepth, readDepth);

    InterlockedExchange(&Counters->ReportRingMaxDepth, ringDepth);
    InterlockedExchange(&Counters->ReportRingDrops, 0);
    InterlockedExchange(&Counters->ReportRingOverruns, 0);

    InterlockedExchange(&Counters->VirtioInterrupts, 0);
    InterlockedExchange(&Counters->VirtioDpcs, 0);
    InterlockedExchange(&Counters->VirtioEvents, 0);
    InterlockedExchange(&Counters->VirtioEventDrops, 0);
    InterlockedExchange(&Counters->VirtioEventOverruns, 0);
    InterlockedExchange(&Counters->VirtioStatusDrops, 0);

    InterlockedExchange(&Counters->VirtioQueueMaxDepth, virtioDepth);
    InterlockedExchange(&Counters->VirtioStatusDrops, 0);

    InterlockedExchange(&Counters->PendingRingMaxDepth, pendingDepth);
    InterlockedExchange(&Counters->PendingRingDrops, 0);

    InterlockedExchange(&Counters->LedWritesRequested, 0);
    InterlockedExchange(&Counters->LedWritesSubmitted, 0);
    InterlockedExchange(&Counters->LedWritesDropped, 0);

    InterlockedExchange(&Counters->StatusQSubmits, 0);
    InterlockedExchange(&Counters->StatusQCompletions, 0);
    InterlockedExchange(&Counters->StatusQFull, 0);
}

PCSTR VioInputHidIoctlToString(_In_ ULONG IoControlCode)
{
    switch (IoControlCode) {
        case IOCTL_HID_GET_DEVICE_DESCRIPTOR:
            return "IOCTL_HID_GET_DEVICE_DESCRIPTOR";
        case IOCTL_HID_GET_REPORT_DESCRIPTOR:
            return "IOCTL_HID_GET_REPORT_DESCRIPTOR";
        case IOCTL_HID_GET_DEVICE_ATTRIBUTES:
            return "IOCTL_HID_GET_DEVICE_ATTRIBUTES";
#ifdef IOCTL_HID_GET_COLLECTION_INFORMATION
        case IOCTL_HID_GET_COLLECTION_INFORMATION:
            return "IOCTL_HID_GET_COLLECTION_INFORMATION";
#endif
#ifdef IOCTL_HID_GET_COLLECTION_DESCRIPTOR
        case IOCTL_HID_GET_COLLECTION_DESCRIPTOR:
            return "IOCTL_HID_GET_COLLECTION_DESCRIPTOR";
#endif
#ifdef IOCTL_HID_FLUSH_QUEUE
        case IOCTL_HID_FLUSH_QUEUE:
            return "IOCTL_HID_FLUSH_QUEUE";
#endif
        case IOCTL_HID_GET_STRING:
            return "IOCTL_HID_GET_STRING";
        case IOCTL_HID_GET_INDEXED_STRING:
            return "IOCTL_HID_GET_INDEXED_STRING";
        case IOCTL_HID_READ_REPORT:
            return "IOCTL_HID_READ_REPORT";
        case IOCTL_HID_WRITE_REPORT:
            return "IOCTL_HID_WRITE_REPORT";
        case IOCTL_HID_GET_FEATURE:
            return "IOCTL_HID_GET_FEATURE";
        case IOCTL_HID_SET_FEATURE:
            return "IOCTL_HID_SET_FEATURE";
#ifdef IOCTL_HID_GET_INPUT_REPORT
        case IOCTL_HID_GET_INPUT_REPORT:
            return "IOCTL_HID_GET_INPUT_REPORT";
#endif
#ifdef IOCTL_HID_SET_OUTPUT_REPORT
        case IOCTL_HID_SET_OUTPUT_REPORT:
            return "IOCTL_HID_SET_OUTPUT_REPORT";
#endif
        case IOCTL_HID_ACTIVATE_DEVICE:
            return "IOCTL_HID_ACTIVATE_DEVICE";
        case IOCTL_HID_DEACTIVATE_DEVICE:
            return "IOCTL_HID_DEACTIVATE_DEVICE";
#ifdef IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST
        case IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST:
            return "IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST";
#endif
        default:
            return "IOCTL_HID_<unknown>";
    }
}
