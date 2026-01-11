#pragma once

/*
 * Lightweight diagnostics for the virtio-input HID minidriver.
 *
 * Goals:
 *  - Print enough information to debug HIDCLASS enumeration failures
 *    (missing/unsupported IOCTLs, wrong descriptor sizes, etc).
 *  - Track virtqueue/report flow to debug missing input events and deadlocks
 *    (stuck READ_REPORT IRPs, ring buffer overruns, virtqueue starvation).
 *  - Be safe to call at DISPATCH_LEVEL (e.g. from a DPC).
 *
 * Build-time / run-time control:
 *  - Diagnostics compile in when VIOINPUT_DIAGNOSTICS==1 (defaults to DBG builds).
 *  - When compiled in, printing is additionally gated by a registry mask:
 *
 *      HKLM\\System\\CurrentControlSet\\Services\\<driver>\\Parameters
 *          DiagnosticsMask (REG_DWORD)
 *
 *    A value of 0 disables all logging. Combine VIOINPUT_LOG_* bits below.
 */

#include <ntddk.h>

#ifndef VIOINPUT_DIAGNOSTICS
#if DBG
#define VIOINPUT_DIAGNOSTICS 1
#else
#define VIOINPUT_DIAGNOSTICS 0
#endif
#endif

// Enable WPP by providing the usual WPP setup in the build (trace.h/trace.tmh).
#ifndef VIOINPUT_USE_WPP
#define VIOINPUT_USE_WPP 0
#endif

// Registry value under the driver's "Parameters" key.
#define VIOINPUT_REG_DIAGNOSTICS_MASK L"Parameters\\DiagnosticsMask"

// Diagnostic categories (bit mask).
#define VIOINPUT_LOG_ERROR 0x00000001UL
#define VIOINPUT_LOG_IOCTL 0x00000002UL
#define VIOINPUT_LOG_QUEUE 0x00000004UL
#define VIOINPUT_LOG_VIRTQ 0x00000008UL
#define VIOINPUT_LOG_VERBOSE 0x80000000UL

#define IOCTL_VIOINPUT_QUERY_COUNTERS \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)

#define VIOINPUT_COUNTERS_VERSION 1

typedef struct _VIOINPUT_COUNTERS {
    ULONG Size;
    ULONG Version;

    // IRP / IOCTL flow (primarily IRP_MJ_INTERNAL_DEVICE_CONTROL from HIDCLASS).
    volatile LONG IoctlTotal;
    volatile LONG IoctlUnknown;

    volatile LONG IoctlHidGetDeviceDescriptor;
    volatile LONG IoctlHidGetReportDescriptor;
    volatile LONG IoctlHidGetDeviceAttributes;
    volatile LONG IoctlHidGetCollectionInformation;
    volatile LONG IoctlHidGetCollectionDescriptor;
    volatile LONG IoctlHidFlushQueue;
    volatile LONG IoctlHidGetString;
    volatile LONG IoctlHidGetIndexedString;
    volatile LONG IoctlHidGetFeature;
    volatile LONG IoctlHidSetFeature;
    volatile LONG IoctlHidGetInputReport;
    volatile LONG IoctlHidSetOutputReport;
    volatile LONG IoctlHidReadReport;
    volatile LONG IoctlHidWriteReport;

    // READ_REPORT lifecycle.
    volatile LONG ReadReportPended;
    volatile LONG ReadReportCompleted;
    volatile LONG ReadReportCancelled;

    // Current + maximum pending READ_REPORT depth.
    volatile LONG ReadReportQueueDepth;
    volatile LONG ReadReportQueueMaxDepth;

    // Translated HID reports buffered while there are no pending READ_REPORT IRPs.
    volatile LONG ReportRingDepth;
    volatile LONG ReportRingMaxDepth;
    volatile LONG ReportRingDrops;
    volatile LONG ReportRingOverruns;

    // Virtqueue / interrupt side.
    volatile LONG VirtioInterrupts;
    volatile LONG VirtioDpcs;
    volatile LONG VirtioEvents;
    volatile LONG VirtioEventDrops;
    volatile LONG VirtioEventOverruns;

    // Current virtqueue depth (buffers posted - buffers completed), if tracked.
    volatile LONG VirtioQueueDepth;
    volatile LONG VirtioQueueMaxDepth;
} VIOINPUT_COUNTERS, *PVIOINPUT_COUNTERS;

#if VIOINPUT_DIAGNOSTICS

VOID VioInputLogInitialize(_In_ PUNICODE_STRING RegistryPath);
VOID VioInputLogShutdown(VOID);

BOOLEAN VioInputLogEnabled(_In_ ULONG Mask);

VOID VioInputLogPrint(
    _In_ ULONG Mask,
    _In_z_ PCSTR Function,
    _In_ ULONG Line,
    _In_z_ _Printf_format_string_ PCSTR Format,
    ...);

#define VIOINPUT_LOG(_mask, ...) VioInputLogPrint((_mask), __FUNCTION__, __LINE__, __VA_ARGS__)

#else

__forceinline VOID VioInputLogInitialize(_In_ PUNICODE_STRING RegistryPath)
{
    UNREFERENCED_PARAMETER(RegistryPath);
}

__forceinline VOID VioInputLogShutdown(VOID)
{
}

__forceinline BOOLEAN VioInputLogEnabled(_In_ ULONG Mask)
{
    UNREFERENCED_PARAMETER(Mask);
    return FALSE;
}

#define VIOINPUT_LOG(_mask, ...) \
    do {                        \
        UNREFERENCED_PARAMETER(_mask); \
    } while (0)

#endif

PCSTR VioInputHidIoctlToString(_In_ ULONG IoControlCode);

VOID VioInputCountersInit(_Out_ PVIOINPUT_COUNTERS Counters);
VOID VioInputCountersSnapshot(_In_ const VIOINPUT_COUNTERS* Counters, _Out_ PVIOINPUT_COUNTERS Snapshot);

#if VIOINPUT_DIAGNOSTICS
static __forceinline VOID VioInputCounterInc(_Inout_ volatile LONG* Counter)
{
    (VOID)InterlockedIncrement(Counter);
}

static __forceinline VOID VioInputCounterDec(_Inout_ volatile LONG* Counter)
{
    (VOID)InterlockedDecrement(Counter);
}

static __forceinline VOID VioInputCounterSet(_Inout_ volatile LONG* Counter, _In_ LONG Value)
{
    (VOID)InterlockedExchange(Counter, Value);
}

static __forceinline VOID VioInputCounterMaxUpdate(_Inout_ volatile LONG* MaxValue, _In_ LONG Value)
{
    LONG current;

    for (;;) {
        current = *MaxValue;
        if (Value <= current) {
            return;
        }

        if (InterlockedCompareExchange(MaxValue, Value, current) == current) {
            return;
        }
    }
}
#else
static __forceinline VOID VioInputCounterInc(_Inout_ volatile LONG* Counter)
{
    UNREFERENCED_PARAMETER(Counter);
}

static __forceinline VOID VioInputCounterDec(_Inout_ volatile LONG* Counter)
{
    UNREFERENCED_PARAMETER(Counter);
}

static __forceinline VOID VioInputCounterSet(_Inout_ volatile LONG* Counter, _In_ LONG Value)
{
    UNREFERENCED_PARAMETER(Counter);
    UNREFERENCED_PARAMETER(Value);
}

static __forceinline VOID VioInputCounterMaxUpdate(_Inout_ volatile LONG* MaxValue, _In_ LONG Value)
{
    UNREFERENCED_PARAMETER(MaxValue);
    UNREFERENCED_PARAMETER(Value);
}
#endif
