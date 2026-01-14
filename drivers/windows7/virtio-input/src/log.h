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
 *
 *  - In diagnostics builds, the mask can also be queried/updated at runtime via
 *    IOCTL_VIOINPUT_GET_LOG_MASK / IOCTL_VIOINPUT_SET_LOG_MASK.
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

// Registry value name under the driver's "Parameters" key.
// Controls whether pending statusq writes are dropped when the virtqueue is full.
//  - 0 (default): keep the latest write pending until space is available
//  - nonzero: drop the pending write if the queue is full at submission time
#define VIOINPUT_REGVAL_STATUSQ_DROP_ON_FULL L"StatusQDropOnFull"

// Diagnostic categories (bit mask).
#define VIOINPUT_LOG_ERROR 0x00000001UL
#define VIOINPUT_LOG_IOCTL 0x00000002UL
#define VIOINPUT_LOG_QUEUE 0x00000004UL
#define VIOINPUT_LOG_VIRTQ 0x00000008UL
#define VIOINPUT_LOG_VERBOSE 0x80000000UL

/*
 * Driver-private IOCTLs (IRP_MJ_DEVICE_CONTROL).
 *
 * These are primarily intended for bring-up and test automation.
 *
 * IOCTL_VIOINPUT_QUERY_COUNTERS:
 *   - METHOD_BUFFERED
 *   - FILE_READ_ACCESS
 *   - Output: VIOINPUT_COUNTERS
 *
 * IOCTL_VIOINPUT_RESET_COUNTERS:
 *   - METHOD_BUFFERED
 *   - FILE_WRITE_ACCESS
 *   - Resets monotonic VIOINPUT_COUNTERS fields except Size/Version.
 *     Current-state depth gauges (e.g. ReadReportQueueDepth) are preserved so
 *     they continue to reflect the true driver state after reset.
 *     The corresponding *MaxDepth fields are reset to the current depth baseline.
 *
 * IOCTL_VIOINPUT_QUERY_STATE:
 *   - METHOD_BUFFERED
 *   - FILE_READ_ACCESS
 *   - Output: VIOINPUT_STATE
 *
 * IOCTL_VIOINPUT_GET_LOG_MASK (diagnostics builds only):
 *   - METHOD_BUFFERED
 *   - FILE_READ_ACCESS
 *   - Output: ULONG (current DiagnosticsMask)
 *
 * IOCTL_VIOINPUT_SET_LOG_MASK (diagnostics builds only):
 *   - METHOD_BUFFERED
 *   - FILE_WRITE_ACCESS
 *   - Input: ULONG (new DiagnosticsMask)
 */
#define IOCTL_VIOINPUT_QUERY_COUNTERS \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)
#define IOCTL_VIOINPUT_RESET_COUNTERS \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS)

#define IOCTL_VIOINPUT_QUERY_STATE \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_READ_ACCESS)

#define IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_READ_ACCESS)

#if VIOINPUT_DIAGNOSTICS
#define IOCTL_VIOINPUT_GET_LOG_MASK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_READ_ACCESS)

#define IOCTL_VIOINPUT_SET_LOG_MASK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_BUFFERED, FILE_WRITE_ACCESS)
#endif

/*
 * VIOINPUT_COUNTERS is a user-mode visible struct (queried via
 * IOCTL_VIOINPUT_QUERY_COUNTERS). It must be append-only to preserve ABI.
 *
 * CI guardrail: scripts/ci/check-win7-virtio-input-diagnostics-abi-sync.py
 * (keeps the duplicated copies in tools/hidtest/main.c and tests/guest-selftest/src/main.cpp
 * in sync with this header).
 */
#define VIOINPUT_COUNTERS_VERSION 3
#define VIOINPUT_STATE_VERSION 3
#define VIOINPUT_INTERRUPT_INFO_VERSION 1

/*
 * Minimal prefix returned by IOCTL_VIOINPUT_QUERY_COUNTERS / IOCTL_VIOINPUT_QUERY_STATE /
 * IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO.
 *
 * Tools may probe the driver with a smaller output buffer than the full
 * VIOINPUT_* structs (e.g. after a version bump). The driver should always try
 * to return at least Size + Version so callers can allocate the correct buffer
 * size and retry.
 */
typedef struct _VIOINPUT_COUNTERS_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_COUNTERS_V1_MIN, *PVIOINPUT_COUNTERS_V1_MIN;

typedef struct _VIOINPUT_STATE_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_STATE_V1_MIN, *PVIOINPUT_STATE_V1_MIN;

typedef struct _VIOINPUT_INTERRUPT_INFO_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_INTERRUPT_INFO_V1_MIN, *PVIOINPUT_INTERRUPT_INFO_V1_MIN;

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

    // Translation-layer report ring (virtio_input_device.report_ring).
    // This is an internal buffering layer between virtio event processing and
    // READ_REPORT handling. It is NOT the primary "buffered while no pending
    // READ_REPORT IRPs" queue (see PendingRing* below).
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

    // Statusq writes dropped when StatusQDropOnFull is enabled (e.g. keyboard LEDs).
    volatile LONG VirtioStatusDrops;
    // Pending READ_REPORT buffering (DEVICE_CONTEXT.PendingReportRing[]).
    // This is the main queue that accumulates reports when HIDCLASS is not
    // issuing IOCTL_HID_READ_REPORT requests fast enough.
    volatile LONG PendingRingDepth;        // Sum across report IDs.
    volatile LONG PendingRingMaxDepth;
    volatile LONG PendingRingDrops;        // Oldest report dropped on ring full.

    // Keyboard LED output reports (HID write -> statusq).
    volatile LONG LedWritesRequested;
    volatile LONG LedWritesSubmitted;
    // Dropped/ignored LED writes (e.g. statusq inactive, drop-on-full policy, or defensive translation failure).
    volatile LONG LedWritesDropped;

    // statusq activity (driver -> device).
    volatile LONG StatusQSubmits;
    volatile LONG StatusQCompletions;
    volatile LONG StatusQFull;
} VIOINPUT_COUNTERS, *PVIOINPUT_COUNTERS;

typedef struct _VIOINPUT_STATE {
    ULONG Size;
    ULONG Version;

    // Values correspond to VIOINPUT_DEVICE_KIND in virtio_input.h.
    ULONG DeviceKind;

    ULONG PciRevisionId;
    ULONG PciSubsystemDeviceId;

    ULONG HardwareReady;
    ULONG InD0;
    ULONG HidActivated;
    ULONG VirtioStarted;

    UINT64 NegotiatedFeatures;

    // Whether StatusQDropOnFull is enabled for this device instance.
    ULONG StatusQDropOnFull;

    /*
     * Keyboard LED support advertised by the virtio-input device via EV_BITS(EV_LED).
     *
     * This is a 5-bit mask for EV_LED codes 0..4:
     *   bit0=NumLock, bit1=CapsLock, bit2=ScrollLock, bit3=Compose, bit4=Kana
     *
     * If 0, the device did not advertise EV_LED support (or it could not be
     * discovered) and the driver will not send LED events on statusq.
     */
    ULONG KeyboardLedSupportedMask;

    // Whether statusq is currently active (driver will emit EV_LED events).
    ULONG StatusQActive;
} VIOINPUT_STATE, *PVIOINPUT_STATE;

/*
 * Interrupt diagnostics snapshot.
 *
 * This IOCTL is intended for the Win7 guest selftest and host harness so they can
 * deterministically validate MSI-X enablement and vector routing (config vs per-queue).
 */
typedef enum _VIOINPUT_INTERRUPT_MODE {
    VioInputInterruptModeUnknown = 0,
    VioInputInterruptModeIntx = 1,
    VioInputInterruptModeMsix = 2,
} VIOINPUT_INTERRUPT_MODE;

typedef enum _VIOINPUT_INTERRUPT_MAPPING {
    VioInputInterruptMappingUnknown = 0,
    VioInputInterruptMappingAllOnVector0 = 1,
    VioInputInterruptMappingPerQueue = 2,
} VIOINPUT_INTERRUPT_MAPPING;

/* Sentinel for "no vector assigned" (mirrors virtio spec VIRTIO_PCI_MSI_NO_VECTOR). */
#define VIOINPUT_INTERRUPT_VECTOR_NONE ((USHORT)0xFFFF)

typedef struct _VIOINPUT_INTERRUPT_INFO {
    ULONG Size;
    ULONG Version;

    VIOINPUT_INTERRUPT_MODE Mode;

    /* Number of message-signaled interrupts granted by the OS (0 when INTx). */
    ULONG MessageCount;

    /* MSI-X vector routing policy chosen (all queues on vector0 vs per-queue). */
    VIOINPUT_INTERRUPT_MAPPING Mapping;

    /* Number of vectors actually used by the driver (0 when INTx). */
    USHORT UsedVectorCount;

    /* Vectors programmed into virtio-pci common cfg (message numbers). */
    USHORT ConfigVector;
    USHORT Queue0Vector; /* eventq */
    USHORT Queue1Vector; /* statusq */

    /* Optional counters (best-effort snapshot). */
    LONG IntxSpuriousCount;

    LONG TotalInterruptCount;
    LONG TotalDpcCount;
    LONG ConfigInterruptCount;
    LONG Queue0InterruptCount;
    LONG Queue1InterruptCount;
} VIOINPUT_INTERRUPT_INFO, *PVIOINPUT_INTERRUPT_INFO;

#if VIOINPUT_DIAGNOSTICS

VOID VioInputLogInitialize(_In_ PUNICODE_STRING RegistryPath);
VOID VioInputLogShutdown(VOID);

BOOLEAN VioInputLogEnabled(_In_ ULONG Mask);

ULONG VioInputLogGetMask(VOID);
ULONG VioInputLogSetMask(_In_ ULONG Mask);

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

/*
 * When diagnostics are compiled out, keep VIOINPUT_LOG() as a single statement
 * without triggering /W4 "conditional expression is constant" warnings, while
 * still "using" the varargs to avoid /W4 unused-local warnings in Release
 * builds (many callsites only pass locals for logging).
 *
 * __noop is supported by MSVC and clang-cl; it discards its arguments without
 * evaluating them or emitting code.
 */
#if defined(_MSC_VER)
#define VIOINPUT_LOG(_mask, ...) __noop((_mask), __VA_ARGS__)
#else
#define VIOINPUT_LOG(_mask, ...) (void)(_mask)
#endif

#endif

PCSTR VioInputHidIoctlToString(_In_ ULONG IoControlCode);

VOID VioInputCountersInit(_Out_ PVIOINPUT_COUNTERS Counters);
VOID VioInputCountersSnapshot(_In_ const VIOINPUT_COUNTERS* Counters, _Out_ PVIOINPUT_COUNTERS Snapshot);
VOID VioInputCountersReset(_Inout_ PVIOINPUT_COUNTERS Counters);

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
