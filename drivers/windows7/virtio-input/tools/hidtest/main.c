#define _CRT_SECURE_NO_WARNINGS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winioctl.h>

// Build (MSVC):
//   cl /nologo /W4 /D_CRT_SECURE_NO_WARNINGS main.c /link setupapi.lib hid.lib
//
// Build (MinGW-w64):
//   gcc -municode -Wall -Wextra -O2 -o hidtest.exe main.c -lsetupapi -lhid

#include <setupapi.h>
#include <hidsdi.h>
#include <hidpi.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <stddef.h>
#include <string.h>
#include <wchar.h>

#pragma comment(lib, "setupapi.lib")
#pragma comment(lib, "hid.lib")

#ifndef FILE_DEVICE_HID
// Some SDKs/headers don't define FILE_DEVICE_HID. The HID class IOCTLs used by
// HidD_* are historically defined under device type 0x0000000B.
#define FILE_DEVICE_HID 0x0000000B
#endif

#ifndef HID_CTL_CODE
#define HID_CTL_CODE(id) CTL_CODE(FILE_DEVICE_HID, (id), METHOD_NEITHER, FILE_ANY_ACCESS)
#endif

#ifndef IOCTL_HID_GET_REPORT_DESCRIPTOR
// WDK `hidclass.h` defines IOCTL_HID_GET_REPORT_DESCRIPTOR as a HID_CTL_CODE.
// Some SDK-only environments don't ship `hidclass.h`, so provide a fallback.
//
// On Windows 7, the function code is 1 (pairs with IOCTL_HID_GET_DEVICE_DESCRIPTOR=0,
// IOCTL_HID_READ_REPORT=2, IOCTL_HID_WRITE_REPORT=3, etc).
#define IOCTL_HID_GET_REPORT_DESCRIPTOR HID_CTL_CODE(1)
#endif

#ifndef IOCTL_HID_GET_COLLECTION_DESCRIPTOR
/*
 * IOCTL_HID_GET_COLLECTION_DESCRIPTOR is not present in some header sets (e.g.
 * older WDKs). When it exists, it's a HID class IOCTL using the same METHOD_NEITHER
 * transfer method as the other IOCTL_HID_* codes.
 *
 * Some header sets appear to disagree on the function code. We provide a
 * best-effort primary definition here and attempt a small set of fallbacks at
 * runtime (see IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT).
 */
#define IOCTL_HID_GET_COLLECTION_DESCRIPTOR HID_CTL_CODE(12)
#endif

#ifndef IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT
// Alternate function code observed in some header sets.
#define IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT HID_CTL_CODE(11)
#endif

#ifndef IOCTL_HID_GET_DEVICE_DESCRIPTOR
#define IOCTL_HID_GET_DEVICE_DESCRIPTOR HID_CTL_CODE(0)
#endif

#ifndef IOCTL_HID_GET_STRING
// WDK `hidclass.h` defines IOCTL_HID_GET_STRING as a HID_CTL_CODE (function code 4).
#define IOCTL_HID_GET_STRING HID_CTL_CODE(4)
#endif

#ifndef IOCTL_HID_GET_INDEXED_STRING
// WDK `hidclass.h` defines IOCTL_HID_GET_INDEXED_STRING as a HID_CTL_CODE (function code 5).
#define IOCTL_HID_GET_INDEXED_STRING HID_CTL_CODE(5)
#endif

#ifndef IOCTL_HID_WRITE_REPORT
#define IOCTL_HID_WRITE_REPORT HID_CTL_CODE(3)
#endif

#ifndef IOCTL_HID_READ_REPORT
#define IOCTL_HID_READ_REPORT HID_CTL_CODE(2)
#endif

#ifndef IOCTL_HID_SET_OUTPUT_REPORT
// WDK `hidclass.h` defines IOCTL_HID_SET_OUTPUT_REPORT as a HID_CTL_CODE (function code 9).
#define IOCTL_HID_SET_OUTPUT_REPORT HID_CTL_CODE(9)
#endif

#ifndef IOCTL_HID_GET_INPUT_REPORT
// WDK `hidclass.h` defines IOCTL_HID_GET_INPUT_REPORT as a HID_CTL_CODE (function code 10).
#define IOCTL_HID_GET_INPUT_REPORT HID_CTL_CODE(10)
#endif

// Historical/alternate function code seen in some header sets. If our primary
// definition fails at runtime, we try this as a fallback.
#ifndef IOCTL_HID_GET_REPORT_DESCRIPTOR_ALT
#define IOCTL_HID_GET_REPORT_DESCRIPTOR_ALT HID_CTL_CODE(103)
#endif

#ifndef HID_REPORT_DESCRIPTOR_TYPE
#define HID_REPORT_DESCRIPTOR_TYPE 0x22
#endif

#define VIRTIO_INPUT_VID 0x1AF4
#define VIRTIO_INPUT_PID_KEYBOARD 0x0001
#define VIRTIO_INPUT_PID_MOUSE 0x0002
#define VIRTIO_INPUT_PID_TABLET 0x0003
// Legacy/alternate product IDs (e.g. older builds that reused the PCI virtio IDs).
#define VIRTIO_INPUT_PID_MODERN 0x1052
#define VIRTIO_INPUT_PID_TRANSITIONAL 0x1011

// Current Aero virtio-input Win7 driver exposes *separate* keyboard/mouse HID
// devices, each with its own report descriptor.
//
// Keep these expectations in sync with:
//   - drivers/windows7/virtio-input/src/descriptor.c
// CI guardrail:
//   - scripts/ci/check-win7-virtio-input-hid-descriptor-sync.py
// Keyboard report descriptor includes both the keyboard+LED collection (ReportID 1)
// and Consumer Control/media keys (ReportID 3). Total: 104 bytes.
#define VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN 104
// Mouse report descriptor advertises 8 buttons (no padding bits) and includes
// a Consumer/AC Pan field for horizontal scrolling. Total: 57 bytes.
#define VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN 57
// Tablet (absolute pointer) report descriptor advertises 8 buttons and absolute X/Y. Total: 47 bytes.
#define VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN 47
#define VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN 9
#define VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN 2
// Consumer Control/media keys input report (ReportID=3) is 2 bytes: [id][bits].
#define VIRTIO_INPUT_EXPECTED_CONSUMER_INPUT_LEN 2
// Mouse input report (ReportID=2) is 6 bytes: [id][buttons][x][y][wheel][AC Pan].
#define VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN 6
// Tablet input report (ReportID=4) is 6 bytes: [id][buttons][x_lo][x_hi][y_lo][y_hi].
#define VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN 6

/*
 * Aero virtio-input driver diagnostics (see `src/log.h` in the driver sources).
 *
 * These are not standard HID IOCTLs; they are regular DeviceIoControl IOCTLs
 * (not IOCTL_HID_*) forwarded by HIDCLASS to the underlying minidriver.
 *
 * Keep the IOCTL definitions + VIOINPUT_* structs below in sync with `src/log.h`.
 * CI guardrail:
 *   - scripts/ci/check-win7-virtio-input-diagnostics-abi-sync.py
 */
#ifndef IOCTL_VIOINPUT_QUERY_COUNTERS
#define IOCTL_VIOINPUT_QUERY_COUNTERS \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)
#endif

#ifndef IOCTL_VIOINPUT_RESET_COUNTERS
#define IOCTL_VIOINPUT_RESET_COUNTERS \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS)
#endif

#ifndef IOCTL_VIOINPUT_QUERY_STATE
#define IOCTL_VIOINPUT_QUERY_STATE \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_READ_ACCESS)
#endif

#ifndef IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO
#define IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_READ_ACCESS)
#endif

#ifndef IOCTL_VIOINPUT_GET_LOG_MASK
#define IOCTL_VIOINPUT_GET_LOG_MASK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_READ_ACCESS)
#endif

#ifndef IOCTL_VIOINPUT_SET_LOG_MASK
#define IOCTL_VIOINPUT_SET_LOG_MASK \
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_BUFFERED, FILE_WRITE_ACCESS)
#endif
#define VIOINPUT_COUNTERS_VERSION 3
#define VIOINPUT_STATE_VERSION 3
#define VIOINPUT_INTERRUPT_INFO_VERSION 1

typedef struct VIOINPUT_COUNTERS_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_COUNTERS_V1_MIN;

typedef struct VIOINPUT_STATE_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_STATE_V1_MIN;

typedef struct VIOINPUT_INTERRUPT_INFO_V1_MIN {
    ULONG Size;
    ULONG Version;
} VIOINPUT_INTERRUPT_INFO_V1_MIN;

typedef struct _VIOINPUT_COUNTERS {
    ULONG Size;
    ULONG Version;

    LONG IoctlTotal;
    LONG IoctlUnknown;

    LONG IoctlHidGetDeviceDescriptor;
    LONG IoctlHidGetReportDescriptor;
    LONG IoctlHidGetDeviceAttributes;
    LONG IoctlHidGetCollectionInformation;
    LONG IoctlHidGetCollectionDescriptor;
    LONG IoctlHidFlushQueue;
    LONG IoctlHidGetString;
    LONG IoctlHidGetIndexedString;
    LONG IoctlHidGetFeature;
    LONG IoctlHidSetFeature;
    LONG IoctlHidGetInputReport;
    LONG IoctlHidSetOutputReport;
    LONG IoctlHidReadReport;
    LONG IoctlHidWriteReport;

    LONG ReadReportPended;
    LONG ReadReportCompleted;
    LONG ReadReportCancelled;

    LONG ReadReportQueueDepth;
    LONG ReadReportQueueMaxDepth;

    LONG ReportRingDepth;
    LONG ReportRingMaxDepth;
    LONG ReportRingDrops;
    LONG ReportRingOverruns;

    LONG VirtioInterrupts;
    LONG VirtioDpcs;
    LONG VirtioEvents;
    LONG VirtioEventDrops;
    LONG VirtioEventOverruns;

    LONG VirtioQueueDepth;
    LONG VirtioQueueMaxDepth;

    LONG VirtioStatusDrops;
    LONG PendingRingDepth;
    LONG PendingRingMaxDepth;
    LONG PendingRingDrops;
    LONG LedWritesRequested;
    LONG LedWritesSubmitted;
    LONG LedWritesDropped;

    LONG StatusQSubmits;
    LONG StatusQCompletions;
    LONG StatusQFull;
} VIOINPUT_COUNTERS;

typedef struct VIOINPUT_STATE {
    ULONG Size;
    ULONG Version;
    ULONG DeviceKind;
    ULONG PciRevisionId;
    ULONG PciSubsystemDeviceId;
    ULONG HardwareReady;
    ULONG InD0;
    ULONG HidActivated;
    ULONG VirtioStarted;
    ULONGLONG NegotiatedFeatures;
    ULONG StatusQDropOnFull;
    ULONG KeyboardLedSupportedMask;
    ULONG StatusQActive;
} VIOINPUT_STATE;

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

#define VIOINPUT_INTERRUPT_VECTOR_NONE ((USHORT)0xFFFF)

typedef struct _VIOINPUT_INTERRUPT_INFO {
    ULONG Size;
    ULONG Version;

    VIOINPUT_INTERRUPT_MODE Mode;
    ULONG MessageCount;
    VIOINPUT_INTERRUPT_MAPPING Mapping;
    USHORT UsedVectorCount;

    USHORT ConfigVector;
    USHORT Queue0Vector;
    USHORT Queue1Vector;

    LONG IntxSpuriousCount;

    LONG TotalInterruptCount;
    LONG TotalDpcCount;
    LONG ConfigInterruptCount;
    LONG Queue0InterruptCount;
    LONG Queue1InterruptCount;
} VIOINPUT_INTERRUPT_INFO;

enum {
    VIOINPUT_DEVICE_KIND_UNKNOWN = 0,
    VIOINPUT_DEVICE_KIND_KEYBOARD = 1,
    VIOINPUT_DEVICE_KIND_MOUSE = 2,
    VIOINPUT_DEVICE_KIND_TABLET = 3,
};

#pragma pack(push, 1)
typedef struct HID_DESCRIPTOR_MIN {
    BYTE bLength;
    BYTE bDescriptorType;
    USHORT bcdHID;
    BYTE bCountry;
    BYTE bNumDescriptors;
    struct {
        BYTE bReportType;
        USHORT wDescriptorLength;
    } DescriptorList[1];
} HID_DESCRIPTOR_MIN;
#pragma pack(pop)

typedef struct OPTIONS {
    int list_only;
    int selftest;
    int json;
    int query_state;
    int query_interrupt_info;
    int query_counters;
    int reset_counters;
    int have_vid;
    int have_pid;
    int have_index;
    int have_duration;
    int have_count;
    int get_log_mask;
    int have_set_log_mask;
    int have_led_mask;
    int led_via_hidd;
    int have_led_ioctl_set_output;
    int led_cycle;
    int led_spam;
    int ioctl_bad_xfer_packet;
    int ioctl_bad_write_report;
    int ioctl_bad_read_xfer_packet;
    int ioctl_bad_read_report;
    int ioctl_bad_set_output_xfer_packet;
    int ioctl_bad_set_output_report;
    int ioctl_bad_get_report_descriptor;
    int ioctl_bad_get_collection_descriptor;
    int ioctl_bad_get_device_descriptor;
    int ioctl_bad_get_string;
    int ioctl_bad_get_indexed_string;
    int ioctl_bad_get_string_out;
    int ioctl_bad_get_indexed_string_out;
    int ioctl_bad_get_input_xfer_packet;
    int ioctl_bad_get_input_report;
    int ioctl_query_counters_short;
    int ioctl_query_state_short;
    int ioctl_query_interrupt_info_short;
    int ioctl_get_input_report;
    int hidd_get_input_report;
    int hidd_bad_set_output_report;
    int dump_desc;
    int dump_collection_desc;
    int query_counters_json;
    int query_interrupt_info_json;
    int quiet;
    int want_keyboard;
    int want_mouse;
    int want_consumer;
    int want_tablet;
    USHORT vid;
    USHORT pid;
    DWORD index;
    DWORD duration_secs;
    DWORD count;
    DWORD set_log_mask;
    DWORD led_spam_count;
    BYTE led_mask;
    BYTE led_ioctl_set_output_mask;
} OPTIONS;

typedef struct SELECTED_DEVICE {
    HANDLE handle;
    DWORD desired_access;
    WCHAR *path;
    HIDD_ATTRIBUTES attr;
    int attr_valid;
    HIDP_CAPS caps;
    int caps_valid;
    DWORD report_desc_len;
    int report_desc_valid;
    DWORD hid_report_desc_len;
    int hid_report_desc_valid;
} SELECTED_DEVICE;

// Forward decls (used by --selftest helpers).
static void free_selected_device(SELECTED_DEVICE *dev);
static int enumerate_hid_devices(const OPTIONS *opt, SELECTED_DEVICE *out);
static int run_selftest_json(const OPTIONS *opt);
static int query_collection_descriptor_length(HANDLE handle, DWORD *len_out, DWORD *err_out, DWORD *ioctl_out);

static volatile LONG g_stop_requested = 0;
static HANDLE g_stop_event = NULL;

static BOOL WINAPI console_ctrl_handler(DWORD ctrl_type)
{
    if (ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT) {
        InterlockedExchange(&g_stop_requested, 1);
        if (g_stop_event != NULL) {
            SetEvent(g_stop_event);
        }
        return TRUE;
    }

    return FALSE;
}

static int is_virtio_input_device(const HIDD_ATTRIBUTES *attr)
{
    if (attr == NULL) {
        return 0;
    }

    if (attr->VendorID != VIRTIO_INPUT_VID) {
        return 0;
    }

    return (attr->ProductID == VIRTIO_INPUT_PID_KEYBOARD) ||
           (attr->ProductID == VIRTIO_INPUT_PID_MOUSE) ||
           (attr->ProductID == VIRTIO_INPUT_PID_TABLET) ||
           (attr->ProductID == VIRTIO_INPUT_PID_MODERN) ||
           (attr->ProductID == VIRTIO_INPUT_PID_TRANSITIONAL);
}

static void print_win32_error_w(const wchar_t *prefix, DWORD err)
{
    wchar_t *msg = NULL;
    DWORD flags = FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
                  FORMAT_MESSAGE_IGNORE_INSERTS;
    DWORD len = FormatMessageW(flags, NULL, err, 0, (LPWSTR)&msg, 0, NULL);
    if (len == 0 || msg == NULL) {
        wprintf(L"%ls: error %lu\n", prefix, err);
        return;
    }

    while (len > 0 && (msg[len - 1] == L'\r' || msg[len - 1] == L'\n')) {
        msg[len - 1] = L'\0';
        len--;
    }
    wprintf(L"%ls: %ls (error %lu)\n", prefix, msg, err);
    LocalFree(msg);
}

static void print_win32_error_file_w(FILE *f, const wchar_t *prefix, DWORD err)
{
    wchar_t *msg = NULL;
    DWORD flags = FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
                  FORMAT_MESSAGE_IGNORE_INSERTS;
    DWORD len;

    if (f == NULL) {
        f = stderr;
    }

    len = FormatMessageW(flags, NULL, err, 0, (LPWSTR)&msg, 0, NULL);
    if (len == 0 || msg == NULL) {
        fwprintf(f, L"%ls: error %lu\n", prefix, err);
        return;
    }

    while (len > 0 && (msg[len - 1] == L'\r' || msg[len - 1] == L'\n')) {
        msg[len - 1] = L'\0';
        len--;
    }
    fwprintf(f, L"%ls: %ls (error %lu)\n", prefix, msg, err);
    LocalFree(msg);
}

static void print_last_error_w(const wchar_t *prefix)
{
    print_win32_error_w(prefix, GetLastError());
}

static void print_last_error_file_w(FILE *f, const wchar_t *prefix)
{
    print_win32_error_file_w(f, prefix, GetLastError());
}

static int parse_u16_hex(const wchar_t *s, USHORT *out)
{
    wchar_t *end = NULL;
    unsigned long v;

    if (s == NULL || out == NULL) {
        return 0;
    }

    v = wcstoul(s, &end, 0);
    if (end == s || *end != L'\0' || v > 0xFFFFUL) {
        return 0;
    }
    *out = (USHORT)v;
    return 1;
}

static int parse_u32_hex(const wchar_t *s, DWORD *out)
{
    wchar_t *end = NULL;
    unsigned long v;

    if (s == NULL || out == NULL) {
        return 0;
    }

    v = wcstoul(s, &end, 0);
    if (end == s || *end != L'\0' || v > 0xFFFFFFFFUL) {
        return 0;
    }
    *out = (DWORD)v;
    return 1;
}

static int parse_u32_dec(const wchar_t *s, DWORD *out)
{
    wchar_t *end = NULL;
    unsigned long v;

    if (s == NULL || out == NULL) {
        return 0;
    }

    v = wcstoul(s, &end, 10);
    if (end == s || *end != L'\0') {
        return 0;
    }
    *out = (DWORD)v;
    return 1;
}

static WCHAR *wcsdup_heap(const WCHAR *s)
{
    size_t n;
    WCHAR *out;

    if (s == NULL) {
        return NULL;
    }

    n = wcslen(s) + 1;
    out = (WCHAR *)malloc(n * sizeof(WCHAR));
    if (out == NULL) {
        return NULL;
    }
    memcpy(out, s, n * sizeof(WCHAR));
    return out;
}

static void dump_hex(const BYTE *buf, DWORD len)
{
    DWORD i;
    for (i = 0; i < len; i++) {
        wprintf(L"%02X", buf[i]);
        if (i + 1 != len) {
            wprintf(L" ");
        }
    }
}

static int reset_vioinput_counters(const SELECTED_DEVICE *dev, int quiet)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        if (quiet) {
            fwprintf(stderr, L"Invalid device handle\n");
        } else {
            wprintf(L"Invalid device handle\n");
        }
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        if (quiet) {
            fwprintf(stderr, L"Device was not opened with GENERIC_WRITE; cannot reset counters\n");
        } else {
            wprintf(L"Device was not opened with GENERIC_WRITE; cannot reset counters\n");
        }
        return 1;
    }

    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_RESET_COUNTERS, NULL, 0, NULL, 0, &bytes, NULL);
    if (!ok) {
        if (quiet) {
            print_last_error_file_w(stderr, L"DeviceIoControl(IOCTL_VIOINPUT_RESET_COUNTERS)");
        } else {
            print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_RESET_COUNTERS)");
        }
        return 1;
    }

    if (!quiet) {
        wprintf(L"\nvirtio-input driver diagnostic counters reset.\n");
    }
    return 0;
}

static void fprint_win32_error_w(FILE *f, const wchar_t *prefix, DWORD err)
{
    wchar_t *msg = NULL;
    DWORD flags = FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM |
                  FORMAT_MESSAGE_IGNORE_INSERTS;
    DWORD len = FormatMessageW(flags, NULL, err, 0, (LPWSTR)&msg, 0, NULL);
    if (len == 0 || msg == NULL) {
        fwprintf(f, L"%ls: error %lu\n", prefix, err);
        return;
    }

    while (len > 0 && (msg[len - 1] == L'\r' || msg[len - 1] == L'\n')) {
        msg[len - 1] = L'\0';
        len--;
    }
    fwprintf(f, L"%ls: %ls (error %lu)\n", prefix, msg, err);
    LocalFree(msg);
}

static void fprint_last_error_w(FILE *f, const wchar_t *prefix)
{
    fprint_win32_error_w(f, prefix, GetLastError());
}

static void json_print_string_w(const WCHAR *s)
{
    const WCHAR *p;

    if (s == NULL) {
        wprintf(L"null");
        return;
    }

    wprintf(L"\"");
    for (p = s; *p; p++) {
        WCHAR ch = *p;
        switch (ch) {
        case L'"':
            wprintf(L"\\\"");
            break;
        case L'\\':
            wprintf(L"\\\\");
            break;
        case L'\b':
            wprintf(L"\\b");
            break;
        case L'\f':
            wprintf(L"\\f");
            break;
        case L'\n':
            wprintf(L"\\n");
            break;
        case L'\r':
            wprintf(L"\\r");
            break;
        case L'\t':
            wprintf(L"\\t");
            break;
        default:
            if (ch < 0x20) {
                wprintf(L"\\u%04X", (unsigned)ch);
            } else if (ch <= 0x7E) {
                wprintf(L"%lc", ch);
            } else {
                wprintf(L"\\u%04X", (unsigned)ch);
            }
            break;
        }
    }
    wprintf(L"\"");
}

static void dump_report_descriptor(HANDLE handle)
{
    BYTE buf[4096];
    DWORD bytes = 0;
    BOOL ok;
    DWORD i;

    ZeroMemory(buf, sizeof(buf));
    ok = DeviceIoControl(handle, IOCTL_HID_GET_REPORT_DESCRIPTOR, NULL, 0, buf, (DWORD)sizeof(buf), &bytes,
                         NULL);
    if (!ok || bytes == 0) {
        bytes = 0;
        ok = DeviceIoControl(handle, IOCTL_HID_GET_REPORT_DESCRIPTOR_ALT, NULL, 0, buf, (DWORD)sizeof(buf),
                             &bytes, NULL);
    }

    if (!ok || bytes == 0) {
        print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_REPORT_DESCRIPTOR)");
        return;
    }

    wprintf(L"\nReport descriptor (%lu bytes):\n", bytes);
    for (i = 0; i < bytes; i += 16) {
        DWORD chunk = bytes - i;
        if (chunk > 16) {
            chunk = 16;
        }
        wprintf(L"  %04lX: ", i);
        dump_hex(buf + i, chunk);
        wprintf(L"\n");
    }
}

static void dump_collection_descriptor(HANDLE handle)
{
    BYTE buf[4096];
    DWORD bytes = 0;
    BOOL ok;
    DWORD i;
    DWORD ioctl;

    ZeroMemory(buf, sizeof(buf));
    ioctl = IOCTL_HID_GET_COLLECTION_DESCRIPTOR;
    ok = DeviceIoControl(handle, ioctl, NULL, 0, buf, (DWORD)sizeof(buf), &bytes, NULL);
    if (!ok || bytes == 0) {
        bytes = 0;
        ioctl = IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT;
        ok = DeviceIoControl(
            handle,
            ioctl,
            NULL,
            0,
            buf,
            (DWORD)sizeof(buf),
            &bytes,
            NULL);
    }
    if (!ok || bytes == 0) {
        print_last_error_w(
            (ioctl == IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT) ? L"DeviceIoControl(IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT)"
                                                               : L"DeviceIoControl(IOCTL_HID_GET_COLLECTION_DESCRIPTOR)");
        return;
    }

    wprintf(L"\nCollection descriptor (%lu bytes) (ioctl=0x%08lX):\n", bytes, ioctl);
    for (i = 0; i < bytes; i += 16) {
        DWORD chunk = bytes - i;
        if (chunk > 16) {
            chunk = 16;
        }
        wprintf(L"  %04lX: ", i);
        dump_hex(buf + i, chunk);
        wprintf(L"\n");
    }
}
static const wchar_t *vioinput_device_kind_to_string(ULONG kind)
{
    switch (kind) {
    case VIOINPUT_DEVICE_KIND_KEYBOARD:
        return L"keyboard";
    case VIOINPUT_DEVICE_KIND_MOUSE:
        return L"mouse";
    case VIOINPUT_DEVICE_KIND_TABLET:
        return L"tablet";
    default:
        return L"unknown";
    }
}

static const wchar_t *vioinput_interrupt_mode_to_string(ULONG mode)
{
    switch (mode) {
    case VioInputInterruptModeIntx:
        return L"intx";
    case VioInputInterruptModeMsix:
        return L"msix";
    default:
        return L"unknown";
    }
}

static const wchar_t *vioinput_interrupt_mapping_to_string(ULONG mapping)
{
    switch (mapping) {
    case VioInputInterruptMappingAllOnVector0:
        return L"all-on-vector0";
    case VioInputInterruptMappingPerQueue:
        return L"per-queue";
    default:
        return L"unknown";
    }
}

static int query_vioinput_state_blob(HANDLE handle, BYTE **buf_out, DWORD *bytes_out)
{
    BYTE *buf = NULL;
    DWORD cap;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;
    ULONG expected_size = 0;

    if (buf_out != NULL) {
        *buf_out = NULL;
    }
    if (bytes_out != NULL) {
        *bytes_out = 0;
    }

    if (handle == INVALID_HANDLE_VALUE || handle == NULL) {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }

    cap = (DWORD)sizeof(VIOINPUT_STATE);
    if (cap < sizeof(VIOINPUT_STATE_V1_MIN)) {
        cap = (DWORD)sizeof(VIOINPUT_STATE_V1_MIN);
    }

    buf = (BYTE *)calloc(cap, 1);
    if (buf == NULL) {
        SetLastError(ERROR_OUTOFMEMORY);
        return 0;
    }

    ok = DeviceIoControl(handle, IOCTL_VIOINPUT_QUERY_STATE, NULL, 0, buf, cap, &bytes, NULL);
    if (ok) {
        if (buf_out != NULL) {
            *buf_out = buf;
        }
        if (bytes_out != NULL) {
            *bytes_out = bytes;
        }
        return 1;
    }

    err = GetLastError();

    // If the buffer was too small, the driver should still return at least Size
    // (and ideally Size+Version). Retry with the reported Size.
    if ((err == ERROR_INSUFFICIENT_BUFFER || err == ERROR_MORE_DATA) && cap >= sizeof(expected_size)) {
        memcpy(&expected_size, buf, sizeof(expected_size));
        if (expected_size != 0 && expected_size > cap && expected_size <= 64u * 1024u) {
            BYTE *b2 = (BYTE *)realloc(buf, expected_size);
            if (b2 == NULL) {
                free(buf);
                SetLastError(ERROR_OUTOFMEMORY);
                return 0;
            }
            buf = b2;
            ZeroMemory(buf, expected_size);
            bytes = 0;
            ok = DeviceIoControl(handle, IOCTL_VIOINPUT_QUERY_STATE, NULL, 0, buf, expected_size, &bytes, NULL);
            if (ok) {
                if (buf_out != NULL) {
                    *buf_out = buf;
                }
                if (bytes_out != NULL) {
                    *bytes_out = bytes;
                }
                return 1;
            }
            err = GetLastError();
        }
    }

    free(buf);
    SetLastError(err);
    return 0;
}

static int query_vioinput_interrupt_info_blob(HANDLE handle, BYTE **buf_out, DWORD *bytes_out)
{
    BYTE *buf = NULL;
    DWORD cap;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;
    ULONG expected_size = 0;

    if (buf_out != NULL) {
        *buf_out = NULL;
    }
    if (bytes_out != NULL) {
        *bytes_out = 0;
    }

    if (handle == INVALID_HANDLE_VALUE || handle == NULL) {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }

    cap = (DWORD)sizeof(VIOINPUT_INTERRUPT_INFO);
    if (cap < sizeof(VIOINPUT_INTERRUPT_INFO_V1_MIN)) {
        cap = (DWORD)sizeof(VIOINPUT_INTERRUPT_INFO_V1_MIN);
    }

    buf = (BYTE *)calloc(cap, 1);
    if (buf == NULL) {
        SetLastError(ERROR_OUTOFMEMORY);
        return 0;
    }

    ok = DeviceIoControl(handle, IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO, NULL, 0, buf, cap, &bytes, NULL);
    if (ok) {
        if (buf_out != NULL) {
            *buf_out = buf;
        }
        if (bytes_out != NULL) {
            *bytes_out = bytes;
        }
        return 1;
    }

    err = GetLastError();

    // If the buffer was too small, the driver should still return at least Size
    // (and ideally Size+Version). Retry with the reported Size.
    if ((err == ERROR_INSUFFICIENT_BUFFER || err == ERROR_MORE_DATA) && cap >= sizeof(expected_size)) {
        memcpy(&expected_size, buf, sizeof(expected_size));
        if (expected_size != 0 && expected_size > cap && expected_size <= 64u * 1024u) {
            BYTE *b2 = (BYTE *)realloc(buf, expected_size);
            if (b2 == NULL) {
                free(buf);
                SetLastError(ERROR_OUTOFMEMORY);
                return 0;
            }
            buf = b2;
            ZeroMemory(buf, expected_size);
            bytes = 0;
            ok = DeviceIoControl(handle, IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO, NULL, 0, buf, expected_size, &bytes,
                                 NULL);
            if (ok) {
                if (buf_out != NULL) {
                    *buf_out = buf;
                }
                if (bytes_out != NULL) {
                    *bytes_out = bytes;
                }
                return 1;
            }
            err = GetLastError();
        }
    }

    free(buf);
    SetLastError(err);
    return 0;
}

static void print_vioinput_state(const VIOINPUT_STATE *st, DWORD bytes)
{
    DWORD avail;

    if (st == NULL) {
        return;
    }

    avail = bytes;
    if (st->Size != 0 && st->Size < avail) {
        avail = st->Size;
    }

    wprintf(L"\nvirtio-input driver state:\n");
    if (avail >= offsetof(VIOINPUT_STATE, Size) + sizeof(ULONG)) {
        wprintf(L"  Size:              %lu (returned %lu bytes)\n", st->Size, bytes);
    } else {
        wprintf(L"  Size:              <missing> (returned %lu bytes)\n", bytes);
    }
    if (avail >= offsetof(VIOINPUT_STATE, Version) + sizeof(ULONG)) {
        wprintf(L"  Version:           %lu\n", st->Version);
    } else {
        wprintf(L"  Version:           <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, DeviceKind) + sizeof(ULONG)) {
        wprintf(L"  DeviceKind:        %ls (%lu)\n", vioinput_device_kind_to_string(st->DeviceKind), st->DeviceKind);
    } else {
        wprintf(L"  DeviceKind:        <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, PciRevisionId) + sizeof(ULONG)) {
        wprintf(L"  PciRevisionId:     0x%02lX\n", st->PciRevisionId);
    } else {
        wprintf(L"  PciRevisionId:     <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, PciSubsystemDeviceId) + sizeof(ULONG)) {
        wprintf(L"  PciSubsystemDevId: 0x%04lX\n", st->PciSubsystemDeviceId);
    } else {
        wprintf(L"  PciSubsystemDevId: <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, HardwareReady) + sizeof(ULONG)) {
        wprintf(L"  HardwareReady:     %lu\n", st->HardwareReady);
    } else {
        wprintf(L"  HardwareReady:     <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, InD0) + sizeof(ULONG)) {
        wprintf(L"  InD0:              %lu\n", st->InD0);
    } else {
        wprintf(L"  InD0:              <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, HidActivated) + sizeof(ULONG)) {
        wprintf(L"  HidActivated:      %lu\n", st->HidActivated);
    } else {
        wprintf(L"  HidActivated:      <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, VirtioStarted) + sizeof(ULONG)) {
        wprintf(L"  VirtioStarted:     %lu\n", st->VirtioStarted);
    } else {
        wprintf(L"  VirtioStarted:     <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, NegotiatedFeatures) + sizeof(st->NegotiatedFeatures)) {
        wprintf(L"  NegotiatedFeatures: 0x%016llX\n", (unsigned long long)st->NegotiatedFeatures);
    } else {
        wprintf(L"  NegotiatedFeatures: <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, StatusQDropOnFull) + sizeof(ULONG)) {
        wprintf(L"  StatusQDropOnFull: %lu\n", st->StatusQDropOnFull);
    } else {
        wprintf(L"  StatusQDropOnFull: <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, KeyboardLedSupportedMask) + sizeof(ULONG)) {
        wprintf(L"  KeyboardLedSupportedMask: 0x%02lX\n", st->KeyboardLedSupportedMask & 0x1Ful);
    } else {
        wprintf(L"  KeyboardLedSupportedMask: <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_STATE, StatusQActive) + sizeof(ULONG)) {
        wprintf(L"  StatusQActive:     %lu\n", st->StatusQActive);
    } else {
        wprintf(L"  StatusQActive:     <missing>\n");
    }
}

static void print_vioinput_interrupt_info(const VIOINPUT_INTERRUPT_INFO *info, DWORD bytes)
{
    DWORD avail;

    if (info == NULL) {
        return;
    }

    avail = bytes;
    if (avail >= sizeof(ULONG)) {
        if (info->Size != 0 && info->Size < avail) {
            avail = info->Size;
        }
    }

    wprintf(L"\nvirtio-input interrupt info:\n");
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Size) + sizeof(ULONG)) {
        wprintf(L"  Size:            %lu (returned %lu bytes)\n", info->Size, bytes);
    } else {
        wprintf(L"  Size:            <missing> (returned %lu bytes)\n", bytes);
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Version) + sizeof(ULONG)) {
        wprintf(L"  Version:         %lu\n", info->Version);
        if (info->Version != VIOINPUT_INTERRUPT_INFO_VERSION) {
            wprintf(L"  [WARN] Version=%lu != expected %u; dumping what is present\n", info->Version,
                    (unsigned)VIOINPUT_INTERRUPT_INFO_VERSION);
        }
    } else {
        wprintf(L"  Version:         <missing>\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Mode) + sizeof(info->Mode)) {
        wprintf(L"  Mode:            %ls (%lu)\n", vioinput_interrupt_mode_to_string((ULONG)info->Mode),
                (ULONG)info->Mode);
    } else {
        wprintf(L"  Mode:            <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, MessageCount) + sizeof(ULONG)) {
        wprintf(L"  MessageCount:    %lu\n", info->MessageCount);
    } else {
        wprintf(L"  MessageCount:    <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Mapping) + sizeof(info->Mapping)) {
        wprintf(L"  Mapping:         %ls (%lu)\n", vioinput_interrupt_mapping_to_string((ULONG)info->Mapping),
                (ULONG)info->Mapping);
    } else {
        wprintf(L"  Mapping:         <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, UsedVectorCount) + sizeof(USHORT)) {
        wprintf(L"  UsedVectorCount: %u\n", (unsigned)info->UsedVectorCount);
    } else {
        wprintf(L"  UsedVectorCount: <missing>\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, ConfigVector) + sizeof(USHORT)) {
        if (info->ConfigVector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  ConfigVector:    none\n");
        } else {
            wprintf(L"  ConfigVector:    %u\n", (unsigned)info->ConfigVector);
        }
    } else {
        wprintf(L"  ConfigVector:    <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue0Vector) + sizeof(USHORT)) {
        if (info->Queue0Vector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  Queue0Vector:    none\n");
        } else {
            wprintf(L"  Queue0Vector:    %u\n", (unsigned)info->Queue0Vector);
        }
    } else {
        wprintf(L"  Queue0Vector:    <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue1Vector) + sizeof(USHORT)) {
        if (info->Queue1Vector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  Queue1Vector:    none\n");
        } else {
            wprintf(L"  Queue1Vector:    %u\n", (unsigned)info->Queue1Vector);
        }
    } else {
        wprintf(L"  Queue1Vector:    <missing>\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, IntxSpuriousCount) + sizeof(LONG)) {
        wprintf(L"  IntxSpurious:    %ld\n", info->IntxSpuriousCount);
    } else {
        wprintf(L"  IntxSpurious:    <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, TotalInterruptCount) + sizeof(LONG)) {
        wprintf(L"  TotalInterrupts: %ld\n", info->TotalInterruptCount);
    } else {
        wprintf(L"  TotalInterrupts: <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, TotalDpcCount) + sizeof(LONG)) {
        wprintf(L"  TotalDpcs:       %ld\n", info->TotalDpcCount);
    } else {
        wprintf(L"  TotalDpcs:       <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, ConfigInterruptCount) + sizeof(LONG)) {
        wprintf(L"  ConfigIrqs:      %ld\n", info->ConfigInterruptCount);
    } else {
        wprintf(L"  ConfigIrqs:      <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue0InterruptCount) + sizeof(LONG)) {
        wprintf(L"  Queue0Irqs:      %ld\n", info->Queue0InterruptCount);
    } else {
        wprintf(L"  Queue0Irqs:      <missing>\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue1InterruptCount) + sizeof(LONG)) {
        wprintf(L"  Queue1Irqs:      %ld\n", info->Queue1InterruptCount);
    } else {
        wprintf(L"  Queue1Irqs:      <missing>\n");
    }

    if (avail >= sizeof(ULONG) && info->Size != 0 && info->Size < sizeof(VIOINPUT_INTERRUPT_INFO)) {
        wprintf(L"  [WARN] driver returned interrupt info Size=%lu < expected %u; dumping what is present\n",
                info->Size, (unsigned)sizeof(VIOINPUT_INTERRUPT_INFO));
    }
}

static void print_vioinput_interrupt_info_json(const VIOINPUT_INTERRUPT_INFO *info, DWORD bytes)
{
    DWORD avail;
    int have_size;
    int have_version;

    if (info == NULL) {
        fwprintf(stderr, L"null interrupt info\n");
        return;
    }

    avail = bytes;
    have_size = (avail >= sizeof(ULONG));
    if (have_size) {
        if (info->Size != 0 && info->Size < avail) {
            avail = info->Size;
        }
    }
    have_version = (avail >= sizeof(ULONG) * 2);

    wprintf(L"{\n");
    wprintf(L"  \"BytesReturned\": %lu,\n", bytes);
    if (have_size && info->Size != 0) {
        wprintf(L"  \"Size\": %lu,\n", info->Size);
    } else {
        wprintf(L"  \"Size\": null,\n");
    }
    if (have_version) {
        wprintf(L"  \"Version\": %lu,\n", info->Version);
    } else {
        wprintf(L"  \"Version\": null,\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Mode) + sizeof(info->Mode)) {
        wprintf(L"  \"Mode\": \"%ls\",\n", vioinput_interrupt_mode_to_string((ULONG)info->Mode));
    } else {
        wprintf(L"  \"Mode\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, MessageCount) + sizeof(ULONG)) {
        wprintf(L"  \"MessageCount\": %lu,\n", info->MessageCount);
    } else {
        wprintf(L"  \"MessageCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Mapping) + sizeof(info->Mapping)) {
        wprintf(L"  \"Mapping\": \"%ls\",\n", vioinput_interrupt_mapping_to_string((ULONG)info->Mapping));
    } else {
        wprintf(L"  \"Mapping\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, UsedVectorCount) + sizeof(USHORT)) {
        wprintf(L"  \"UsedVectorCount\": %u,\n", (unsigned)info->UsedVectorCount);
    } else {
        wprintf(L"  \"UsedVectorCount\": null,\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, ConfigVector) + sizeof(USHORT)) {
        if (info->ConfigVector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  \"ConfigVector\": null,\n");
        } else {
            wprintf(L"  \"ConfigVector\": %u,\n", (unsigned)info->ConfigVector);
        }
    } else {
        wprintf(L"  \"ConfigVector\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue0Vector) + sizeof(USHORT)) {
        if (info->Queue0Vector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  \"Queue0Vector\": null,\n");
        } else {
            wprintf(L"  \"Queue0Vector\": %u,\n", (unsigned)info->Queue0Vector);
        }
    } else {
        wprintf(L"  \"Queue0Vector\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue1Vector) + sizeof(USHORT)) {
        if (info->Queue1Vector == VIOINPUT_INTERRUPT_VECTOR_NONE) {
            wprintf(L"  \"Queue1Vector\": null,\n");
        } else {
            wprintf(L"  \"Queue1Vector\": %u,\n", (unsigned)info->Queue1Vector);
        }
    } else {
        wprintf(L"  \"Queue1Vector\": null,\n");
    }

    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, IntxSpuriousCount) + sizeof(LONG)) {
        wprintf(L"  \"IntxSpuriousCount\": %ld,\n", info->IntxSpuriousCount);
    } else {
        wprintf(L"  \"IntxSpuriousCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, TotalInterruptCount) + sizeof(LONG)) {
        wprintf(L"  \"TotalInterruptCount\": %ld,\n", info->TotalInterruptCount);
    } else {
        wprintf(L"  \"TotalInterruptCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, TotalDpcCount) + sizeof(LONG)) {
        wprintf(L"  \"TotalDpcCount\": %ld,\n", info->TotalDpcCount);
    } else {
        wprintf(L"  \"TotalDpcCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, ConfigInterruptCount) + sizeof(LONG)) {
        wprintf(L"  \"ConfigInterruptCount\": %ld,\n", info->ConfigInterruptCount);
    } else {
        wprintf(L"  \"ConfigInterruptCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue0InterruptCount) + sizeof(LONG)) {
        wprintf(L"  \"Queue0InterruptCount\": %ld,\n", info->Queue0InterruptCount);
    } else {
        wprintf(L"  \"Queue0InterruptCount\": null,\n");
    }
    if (avail >= offsetof(VIOINPUT_INTERRUPT_INFO, Queue1InterruptCount) + sizeof(LONG)) {
        wprintf(L"  \"Queue1InterruptCount\": %ld\n", info->Queue1InterruptCount);
    } else {
        wprintf(L"  \"Queue1InterruptCount\": null\n");
    }

    wprintf(L"}\n");
}

static void dump_keyboard_report(const BYTE *buf, DWORD len)
{
    DWORD off = 0;
    BYTE report_id = 0;
    BYTE modifiers;
    const BYTE *keys;
    DWORD key_count;
    DWORD i;

    if (len == 0) {
        wprintf(L"keyboard: <empty>\n");
        return;
    }

    // Common layouts:
    // - Boot keyboard: 8 bytes (no ReportID) => [mod][res][k1..k6]
    // - With ReportID: 9 bytes             => [id][mod][res][k1..k6]
    if (len == 9 && buf[0] != 0) {
        report_id = buf[0];
        off = 1;
    }

    if (len < off + 2) {
        wprintf(L"keyboard: <short> ");
        dump_hex(buf, len);
        wprintf(L"\n");
        return;
    }

    modifiers = buf[off];
    keys = buf + off + 2;
    key_count = len - (off + 2);

    if (report_id != 0) {
        wprintf(L"keyboard: id=%u ", report_id);
    } else {
        wprintf(L"keyboard: ");
    }

    wprintf(L"mods=0x%02X keys=[", modifiers);
    for (i = 0; i < key_count; i++) {
        wprintf(L"%02X", keys[i]);
        if (i + 1 != key_count) {
            wprintf(L" ");
        }
    }
    wprintf(L"]\n");
}

static void dump_mouse_report(const BYTE *buf, DWORD len, int assume_report_id)
{
    DWORD off = 0;
    BYTE report_id = 0;
    BYTE buttons;
    char dx;
    char dy;
    char wheel;
    char pan;

    if (len == 0) {
        wprintf(L"mouse: <empty>\n");
        return;
    }

    // Common layouts:
    // - Boot mouse: 3 bytes (no ReportID) => [btn][x][y]
    // - Wheel mouse: 4 bytes              => [btn][x][y][wheel]
    // - Wheel+Pan mouse: 5 bytes          => [btn][x][y][wheel][pan] (HID Consumer/AC Pan)
    // - With ReportID: one extra byte at front.
    if (assume_report_id && len >= 4 && buf[0] != 0) {
        report_id = buf[0];
        off = 1;
    }

    if (len < off + 3) {
        wprintf(L"mouse: <short> ");
        dump_hex(buf, len);
        wprintf(L"\n");
        return;
    }

    buttons = buf[off + 0];
    dx = (char)buf[off + 1];
    dy = (char)buf[off + 2];
    wheel = 0;
    pan = 0;
    if (len >= off + 4) {
        wheel = (char)buf[off + 3];
    }
    if (len >= off + 5) {
        pan = (char)buf[off + 4];
    }

    if (report_id != 0) {
        wprintf(L"mouse: id=%u ", report_id);
    } else {
        wprintf(L"mouse: ");
    }

    wprintf(L"buttons=0x%02X dx=%d dy=%d", buttons, (int)dx, (int)dy);
    if (len >= off + 4) {
        wprintf(L" wheel=%d", (int)wheel);
    }
    if (len >= off + 5) {
        wprintf(L" pan=%d", (int)pan);
    }
    wprintf(L"\n");
}

static void dump_consumer_report(const BYTE *buf, DWORD len, int assume_report_id)
{
    DWORD off = 0;
    BYTE report_id = 0;
    BYTE bits;

    if (len == 0) {
        wprintf(L"consumer: <empty>\n");
        return;
    }

    // Common layout for this driver:
    // - Consumer Control (media keys): 1 byte bitmask
    // - With ReportID: one extra byte at front.
    if (assume_report_id && len >= 2 && buf[0] != 0) {
        report_id = buf[0];
        off = 1;
    }

    if (len < off + 1) {
        wprintf(L"consumer: <short> ");
        dump_hex(buf, len);
        wprintf(L"\n");
        return;
    }

    bits = buf[off];

    if (report_id != 0) {
        wprintf(L"consumer: id=%u ", report_id);
    } else {
        wprintf(L"consumer: ");
    }

    wprintf(L"bits=0x%02X", bits);

    if (bits != 0) {
        int first = 1;
        wprintf(L" [");
        if (bits & (1u << 0)) {
            wprintf(L"%smute", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 1)) {
            wprintf(L"%svol-", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 2)) {
            wprintf(L"%svol+", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 3)) {
            wprintf(L"%splay/pause", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 4)) {
            wprintf(L"%snext", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 5)) {
            wprintf(L"%sprev", first ? L"" : L" ");
            first = 0;
        }
        if (bits & (1u << 6)) {
            wprintf(L"%sstop", first ? L"" : L" ");
            first = 0;
        }
        wprintf(L"]");
    }

    wprintf(L"\n");
}
static void dump_tablet_report(const BYTE *buf, DWORD len, int assume_report_id)
{
    DWORD off = 0;
    BYTE report_id = 0;
    BYTE buttons;
    USHORT x;
    USHORT y;

    if (len == 0) {
        wprintf(L"tablet: <empty>\n");
        return;
    }

    // Driver layout:
    // - Tablet: 5 bytes (no ReportID) => [btn][x_lo][x_hi][y_lo][y_hi]
    // - With ReportID: one extra byte at front.
    if (assume_report_id && len >= 6 && buf[0] != 0) {
        report_id = buf[0];
        off = 1;
    }

    if (len < off + 5) {
        wprintf(L"tablet: <short> ");
        dump_hex(buf, len);
        wprintf(L"\n");
        return;
    }

    buttons = buf[off + 0];
    x = (USHORT)(buf[off + 1] | ((USHORT)buf[off + 2] << 8));
    y = (USHORT)(buf[off + 3] | ((USHORT)buf[off + 4] << 8));

    if (report_id != 0) {
        wprintf(L"tablet: id=%u ", report_id);
    } else {
        wprintf(L"tablet: ");
    }

    wprintf(L"buttons=0x%02X x=%u y=%u\n", buttons, (unsigned)x, (unsigned)y);
}

static int query_vioinput_counters_blob(const SELECTED_DEVICE *dev, BYTE **buf_out, DWORD *bytes_out)
{
    BYTE *buf = NULL;
    DWORD cap;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;
    ULONG expected_size = 0;

    if (buf_out != NULL) {
        *buf_out = NULL;
    }
    if (bytes_out != NULL) {
        *bytes_out = 0;
    }

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        SetLastError(ERROR_INVALID_HANDLE);
        return 0;
    }

    // Start with the size we expect for this build of the tool, then adapt if the
    // driver reports a larger Size (e.g. newer driver version with extra fields).
    cap = (DWORD)sizeof(VIOINPUT_COUNTERS);
    if (cap < sizeof(VIOINPUT_COUNTERS_V1_MIN)) {
        cap = (DWORD)sizeof(VIOINPUT_COUNTERS_V1_MIN);
    }

    buf = (BYTE *)calloc(cap, 1);
    if (buf == NULL) {
        SetLastError(ERROR_OUTOFMEMORY);
        return 0;
    }

    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_QUERY_COUNTERS, NULL, 0, buf, cap, &bytes, NULL);
    if (ok) {
        if (buf_out != NULL) {
            *buf_out = buf;
        }
        if (bytes_out != NULL) {
            *bytes_out = bytes;
        }
        return 1;
    }

    err = GetLastError();

    // If the buffer was too small, the driver should still return at least Size
    // (and ideally Size+Version). Retry with the reported Size.
    if ((err == ERROR_INSUFFICIENT_BUFFER || err == ERROR_MORE_DATA) && cap >= sizeof(expected_size)) {
        memcpy(&expected_size, buf, sizeof(expected_size));
        if (expected_size != 0 && expected_size > cap && expected_size <= 64u * 1024u) {
            BYTE *b2 = (BYTE *)realloc(buf, expected_size);
            if (b2 == NULL) {
                free(buf);
                SetLastError(ERROR_OUTOFMEMORY);
                return 0;
            }
            buf = b2;
            ZeroMemory(buf, expected_size);
            bytes = 0;
            ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_QUERY_COUNTERS, NULL, 0, buf, expected_size, &bytes, NULL);
            if (ok) {
                if (buf_out != NULL) {
                    *buf_out = buf;
                }
                if (bytes_out != NULL) {
                    *bytes_out = bytes;
                }
                return 1;
            }
            err = GetLastError();
        }
    }

    free(buf);
    SetLastError(err);
    return 0;
}

static int dump_vioinput_counters(const SELECTED_DEVICE *dev)
{
    BYTE *buf = NULL;
    DWORD bytes = 0;

    ULONG size = 0;
    ULONG version = 0;
    DWORD avail = 0;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if (!query_vioinput_counters_blob(dev, &buf, &bytes)) {
        print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_COUNTERS)");
        return 1;
    }
    if (bytes == 0) {
        wprintf(L"IOCTL_VIOINPUT_QUERY_COUNTERS returned 0 bytes\n");
        free(buf);
        return 1;
    }

    avail = bytes;

    if (avail >= sizeof(ULONG)) {
        memcpy(&size, buf, sizeof(size));
        if (size != 0 && size < avail) {
            avail = size;
        }
    }
    if (avail >= sizeof(ULONG) * 2) {
        memcpy(&version, buf + sizeof(ULONG), sizeof(version));
    }

    wprintf(L"\nVIOINPUT counters (bytes=%lu):\n", bytes);
    if (size != 0) {
        wprintf(L"  Size:    %lu\n", size);
    } else {
        wprintf(L"  Size:    <missing>\n");
    }
    if (avail >= sizeof(ULONG) * 2) {
        wprintf(L"  Version: %lu\n", version);
    } else {
        wprintf(L"  Version: <missing>\n");
    }

    if (size != 0 && size < sizeof(VIOINPUT_COUNTERS)) {
        wprintf(L"  [WARN] driver returned counters Size=%lu < expected %u; dumping what is present\n", size,
                (unsigned)sizeof(VIOINPUT_COUNTERS));
    }
    if (avail >= sizeof(ULONG) * 2 && version != VIOINPUT_COUNTERS_VERSION) {
        wprintf(L"  [WARN] counters Version=%lu != expected %u; dumping what is present\n", version,
                (unsigned)VIOINPUT_COUNTERS_VERSION);
    }

    // Helper: print a LONG field if it is present, else mark as n/a.
#define VIOINPUT_WIDEN2(_x) L##_x
#define VIOINPUT_WIDEN(_x) VIOINPUT_WIDEN2(_x)
#define DUMP_LONG_FIELD(_name)                                                                  \
    do {                                                                                        \
        LONG v;                                                                                 \
        size_t off = offsetof(VIOINPUT_COUNTERS, _name);                                        \
        if (avail >= off + sizeof(v)) {                                                         \
            memcpy(&v, buf + off, sizeof(v));                                                   \
            wprintf(L"  %-32ls: %ld\n", VIOINPUT_WIDEN(#_name), v);                              \
        } else {                                                                                \
            wprintf(L"  %-32ls: <n/a>\n", VIOINPUT_WIDEN(#_name));                               \
        }                                                                                       \
    } while (0)

    wprintf(L"\n  -- IRP / IOCTL flow --\n");
    DUMP_LONG_FIELD(IoctlTotal);
    DUMP_LONG_FIELD(IoctlUnknown);
    DUMP_LONG_FIELD(IoctlHidGetDeviceDescriptor);
    DUMP_LONG_FIELD(IoctlHidGetReportDescriptor);
    DUMP_LONG_FIELD(IoctlHidGetDeviceAttributes);
    DUMP_LONG_FIELD(IoctlHidGetCollectionInformation);
    DUMP_LONG_FIELD(IoctlHidGetCollectionDescriptor);
    DUMP_LONG_FIELD(IoctlHidFlushQueue);
    DUMP_LONG_FIELD(IoctlHidGetString);
    DUMP_LONG_FIELD(IoctlHidGetIndexedString);
    DUMP_LONG_FIELD(IoctlHidGetFeature);
    DUMP_LONG_FIELD(IoctlHidSetFeature);
    DUMP_LONG_FIELD(IoctlHidGetInputReport);
    DUMP_LONG_FIELD(IoctlHidSetOutputReport);
    DUMP_LONG_FIELD(IoctlHidReadReport);
    DUMP_LONG_FIELD(IoctlHidWriteReport);

    wprintf(L"\n  -- READ_REPORT lifecycle --\n");
    DUMP_LONG_FIELD(ReadReportPended);
    DUMP_LONG_FIELD(ReadReportCompleted);
    DUMP_LONG_FIELD(ReadReportCancelled);
    DUMP_LONG_FIELD(ReadReportQueueDepth);
    DUMP_LONG_FIELD(ReadReportQueueMaxDepth);

    wprintf(L"\n  -- Translator report ring buffering (virtio_input_device.report_ring) --\n");
    DUMP_LONG_FIELD(ReportRingDepth);
    DUMP_LONG_FIELD(ReportRingMaxDepth);
    DUMP_LONG_FIELD(ReportRingDrops);
    DUMP_LONG_FIELD(ReportRingOverruns);

    wprintf(L"\n  -- Pending READ_REPORT buffering (PendingReportRing[]) --\n");
    DUMP_LONG_FIELD(PendingRingDepth);
    DUMP_LONG_FIELD(PendingRingMaxDepth);
    DUMP_LONG_FIELD(PendingRingDrops);

    wprintf(L"\n  -- Virtqueue / interrupt side --\n");
    DUMP_LONG_FIELD(VirtioInterrupts);
    DUMP_LONG_FIELD(VirtioDpcs);
    DUMP_LONG_FIELD(VirtioEvents);
    DUMP_LONG_FIELD(VirtioEventDrops);
    DUMP_LONG_FIELD(VirtioEventOverruns);
    DUMP_LONG_FIELD(VirtioQueueDepth);
    DUMP_LONG_FIELD(VirtioQueueMaxDepth);
    DUMP_LONG_FIELD(VirtioStatusDrops);

    wprintf(L"\n  -- statusq / keyboard LEDs --\n");
    DUMP_LONG_FIELD(LedWritesRequested);
    DUMP_LONG_FIELD(LedWritesSubmitted);
    DUMP_LONG_FIELD(LedWritesDropped);
    DUMP_LONG_FIELD(StatusQSubmits);
    DUMP_LONG_FIELD(StatusQCompletions);
    DUMP_LONG_FIELD(StatusQFull);

#undef DUMP_LONG_FIELD
#undef VIOINPUT_WIDEN
#undef VIOINPUT_WIDEN2

    free(buf);
    return 0;
}

static int dump_vioinput_counters_json(const SELECTED_DEVICE *dev)
{
    BYTE *buf = NULL;
    DWORD bytes = 0;

    ULONG size = 0;
    ULONG version = 0;
    DWORD avail = 0;

    int have_size = 0;
    int have_version = 0;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        fwprintf(stderr, L"Invalid device handle\n");
        return 1;
    }

    if (!query_vioinput_counters_blob(dev, &buf, &bytes)) {
        print_last_error_file_w(stderr, L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_COUNTERS)");
        return 1;
    }
    if (bytes == 0) {
        fwprintf(stderr, L"IOCTL_VIOINPUT_QUERY_COUNTERS returned 0 bytes\n");
        free(buf);
        return 1;
    }

    avail = bytes;

    have_size = (avail >= sizeof(ULONG));
    if (have_size) {
        memcpy(&size, buf, sizeof(size));
        if (size != 0 && size < avail) {
            avail = size;
        }
    }

    have_version = (avail >= sizeof(ULONG) * 2);
    if (have_version) {
        memcpy(&version, buf + sizeof(ULONG), sizeof(version));
    }

    if (size != 0 && size < sizeof(VIOINPUT_COUNTERS)) {
        fwprintf(stderr, L"WARNING: driver returned counters Size=%lu < expected %u; dumping what is present\n", size,
                 (unsigned)sizeof(VIOINPUT_COUNTERS));
    }
    if (have_version && version != VIOINPUT_COUNTERS_VERSION) {
        fwprintf(stderr, L"WARNING: counters Version=%lu != expected %u; dumping what is present\n", version,
                 (unsigned)VIOINPUT_COUNTERS_VERSION);
    }

#define VIOINPUT_WIDEN2(_x) L##_x
#define VIOINPUT_WIDEN(_x) VIOINPUT_WIDEN2(_x)
#define JSON_LONG_FIELD(_name, _is_last)                                                         \
    do {                                                                                         \
        LONG v;                                                                                  \
        size_t off = offsetof(VIOINPUT_COUNTERS, _name);                                         \
        wprintf(L"  \"%ls\": ", VIOINPUT_WIDEN(#_name));                                         \
        if (avail >= off + sizeof(v)) {                                                          \
            memcpy(&v, buf + off, sizeof(v));                                                    \
            wprintf(L"%ld", v);                                                                  \
        } else {                                                                                 \
            wprintf(L"null");                                                                    \
        }                                                                                        \
        if (!(_is_last)) {                                                                       \
            wprintf(L",");                                                                       \
        }                                                                                        \
        wprintf(L"\n");                                                                          \
    } while (0)

    wprintf(L"{\n");
    wprintf(L"  \"BytesReturned\": %lu,\n", bytes);
    if (have_size && size != 0) {
        wprintf(L"  \"Size\": %lu,\n", size);
    } else {
        wprintf(L"  \"Size\": null,\n");
    }
    if (have_version) {
        wprintf(L"  \"Version\": %lu,\n", version);
    } else {
        wprintf(L"  \"Version\": null,\n");
    }

    JSON_LONG_FIELD(IoctlTotal, 0);
    JSON_LONG_FIELD(IoctlUnknown, 0);
    JSON_LONG_FIELD(IoctlHidGetDeviceDescriptor, 0);
    JSON_LONG_FIELD(IoctlHidGetReportDescriptor, 0);
    JSON_LONG_FIELD(IoctlHidGetDeviceAttributes, 0);
    JSON_LONG_FIELD(IoctlHidGetCollectionInformation, 0);
    JSON_LONG_FIELD(IoctlHidGetCollectionDescriptor, 0);
    JSON_LONG_FIELD(IoctlHidFlushQueue, 0);
    JSON_LONG_FIELD(IoctlHidGetString, 0);
    JSON_LONG_FIELD(IoctlHidGetIndexedString, 0);
    JSON_LONG_FIELD(IoctlHidGetFeature, 0);
    JSON_LONG_FIELD(IoctlHidSetFeature, 0);
    JSON_LONG_FIELD(IoctlHidGetInputReport, 0);
    JSON_LONG_FIELD(IoctlHidSetOutputReport, 0);
    JSON_LONG_FIELD(IoctlHidReadReport, 0);
    JSON_LONG_FIELD(IoctlHidWriteReport, 0);
    JSON_LONG_FIELD(ReadReportPended, 0);
    JSON_LONG_FIELD(ReadReportCompleted, 0);
    JSON_LONG_FIELD(ReadReportCancelled, 0);
    JSON_LONG_FIELD(ReadReportQueueDepth, 0);
    JSON_LONG_FIELD(ReadReportQueueMaxDepth, 0);
    JSON_LONG_FIELD(ReportRingDepth, 0);
    JSON_LONG_FIELD(ReportRingMaxDepth, 0);
    JSON_LONG_FIELD(ReportRingDrops, 0);
    JSON_LONG_FIELD(ReportRingOverruns, 0);
    JSON_LONG_FIELD(VirtioInterrupts, 0);
    JSON_LONG_FIELD(VirtioDpcs, 0);
    JSON_LONG_FIELD(VirtioEvents, 0);
    JSON_LONG_FIELD(VirtioEventDrops, 0);
    JSON_LONG_FIELD(VirtioEventOverruns, 0);
    JSON_LONG_FIELD(VirtioQueueDepth, 0);
    JSON_LONG_FIELD(VirtioQueueMaxDepth, 0);
    JSON_LONG_FIELD(VirtioStatusDrops, 0);
    JSON_LONG_FIELD(PendingRingDepth, 0);
    JSON_LONG_FIELD(PendingRingMaxDepth, 0);
    JSON_LONG_FIELD(PendingRingDrops, 0);
    JSON_LONG_FIELD(LedWritesRequested, 0);
    JSON_LONG_FIELD(LedWritesSubmitted, 0);
    JSON_LONG_FIELD(LedWritesDropped, 0);
    JSON_LONG_FIELD(StatusQSubmits, 0);
    JSON_LONG_FIELD(StatusQCompletions, 0);
    JSON_LONG_FIELD(StatusQFull, 1);

    wprintf(L"}\n");

#undef JSON_LONG_FIELD
#undef VIOINPUT_WIDEN
#undef VIOINPUT_WIDEN2

    free(buf);
    return 0;
}

static void print_usage(void)
{
    wprintf(L"hidtest: minimal HID report/IOCTL probe tool (Win7)\n");
    wprintf(L"\n");
    wprintf(L"Usage:\n");
    wprintf(L"  hidtest.exe [--list [--json]]\n");
    wprintf(L"  hidtest.exe --selftest [--keyboard|--mouse|--tablet] [--json]\n");
    wprintf(L"  hidtest.exe [--keyboard|--mouse|--tablet|--consumer] [--index N] [--vid 0x1234] [--pid 0x5678]\n");
    wprintf(L"             [--led 0x1F | --led-hidd 0x1F | --led-ioctl-set-output 0x1F | --led-cycle | --led-spam N] [--dump-desc]\n");
    wprintf(L"             [--duration SECS] [--count N]\n");
    wprintf(L"             [--dump-collection-desc]\n");
    wprintf(L"             [--state]\n");
    wprintf(L"             [--interrupt-info]\n");
    wprintf(L"             [--interrupt-info-json]\n");
    wprintf(L"             [--counters]\n");
    wprintf(L"             [--counters-json]\n");
    wprintf(L"             [--reset-counters]\n");
    wprintf(L"             [--get-log-mask | --set-log-mask 0xMASK]\n");
    wprintf(L"             [--ioctl-bad-xfer-packet | --ioctl-bad-write-report |\n");
    wprintf(L"              --ioctl-bad-read-xfer-packet | --ioctl-bad-read-report |\n");
    wprintf(L"              --ioctl-bad-get-input-xfer-packet | --ioctl-bad-get-input-report]\n");
    wprintf(L"             [--ioctl-bad-set-output-xfer-packet | --ioctl-bad-set-output-report | --hidd-bad-set-output-report]\n");
    wprintf(L"             [--ioctl-bad-get-report-descriptor | --ioctl-bad-get-collection-descriptor | --ioctl-bad-get-device-descriptor |\n");
    wprintf(L"              --ioctl-bad-get-string | --ioctl-bad-get-indexed-string |\n");
    wprintf(L"              --ioctl-bad-get-string-out | --ioctl-bad-get-indexed-string-out]\n");
    wprintf(L"             [--ioctl-get-input-report]\n");
    wprintf(L"             [--hidd-get-input-report]\n");
    wprintf(L"\n");
    wprintf(L"Options:\n");
    wprintf(L"  --list          List all present HID interfaces and exit\n");
    wprintf(L"  --selftest      Validate virtio-input HID descriptor contract and exit (0=pass, 1=fail)\n");
    wprintf(L"  --json          With --list or --selftest, emit machine-readable JSON on stdout\n");
    wprintf(L"  --quiet         Suppress enumeration / device summary output (keeps stdout clean for scraping)\n");
    wprintf(L"  --keyboard      Prefer/select the keyboard top-level collection (Usage=Keyboard)\n");
    wprintf(L"  --mouse         Prefer/select the mouse top-level collection (Usage=Mouse)\n");
    wprintf(L"  --consumer      Prefer/select the Consumer Control collection (UsagePage=Consumer, Usage=Consumer Control)\n");
    wprintf(L"  --tablet        Prefer/select the virtio-input tablet interface (VID 0x1AF4, PID 0x0003)\n");
    wprintf(L"  --index N       Open HID interface at enumeration index N\n");
    wprintf(L"  --vid 0xVID     Filter by vendor ID (hex)\n");
    wprintf(L"  --pid 0xPID     Filter by product ID (hex)\n");
    wprintf(L"  --duration SECS Exit report read loop after SECS seconds\n");
    wprintf(L"  --count N       Exit report read loop after reading N reports\n");
    wprintf(L"  --state         Query virtio-input driver state via IOCTL_VIOINPUT_QUERY_STATE and exit\n");
    wprintf(L"  --interrupt-info\n");
    wprintf(L"                 Query virtio-input interrupt diagnostics via IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO and exit\n");
    wprintf(L"  --interrupt-info-json\n");
    wprintf(L"                 Query virtio-input interrupt diagnostics and print as JSON\n");
    wprintf(L"  --led 0xMASK    Send keyboard LED output report (ReportID=1)\n");
    wprintf(L"                 Bits: 0x01 NumLock, 0x02 CapsLock, 0x04 ScrollLock, 0x08 Compose, 0x10 Kana\n");
    wprintf(L"  --led-hidd 0xMASK\n");
    wprintf(L"                 Send keyboard LEDs using HidD_SetOutputReport (exercises IOCTL_HID_SET_OUTPUT_REPORT)\n");
    wprintf(L"  --led-ioctl-set-output 0xMASK\n");
    wprintf(L"                 Send keyboard LEDs using DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)\n");
    wprintf(L"  --led-cycle     Cycle keyboard LEDs to visually confirm write path\n");
    wprintf(L"                 (cycles the 5 HID boot keyboard LED bits: Num/Caps/Scroll/Compose/Kana)\n");
    wprintf(L"  --led-spam N    Rapidly send N keyboard LED output reports (alternating 0 and 0x1F by default) to stress the write path\n");
    wprintf(L"                 The \"on\" value can be overridden by combining with --led/--led-hidd/--led-ioctl-set-output.\n");
    wprintf(L"  --dump-desc     Print the raw HID report descriptor bytes\n");
    wprintf(L"  --dump-collection-desc\n");
    wprintf(L"                 Print the raw bytes returned by IOCTL_HID_GET_COLLECTION_DESCRIPTOR\n");
    wprintf(L"  --counters      Query and print virtio-input driver diagnostic counters (IOCTL_VIOINPUT_QUERY_COUNTERS)\n");
    wprintf(L"  --counters-json Query and print virtio-input driver diagnostic counters as JSON\n");
    wprintf(L"  --reset-counters\n");
    wprintf(L"                 Reset virtio-input driver diagnostic counters (IOCTL_VIOINPUT_RESET_COUNTERS)\n");
    wprintf(L"                 (Depth gauges reflect current driver state and may remain non-zero after reset)\n");
    wprintf(L"                 (May be combined with --counters/--counters-json to verify reset)\n");
    wprintf(L"  --get-log-mask  Query the current Aero virtio-input diagnostics mask (DBG driver builds only)\n");
    wprintf(L"  --set-log-mask  Set the current Aero virtio-input diagnostics mask (DBG driver builds only)\n");
    wprintf(L"  --ioctl-bad-xfer-packet\n");
    wprintf(L"                 Send IOCTL_HID_WRITE_REPORT with an invalid HID_XFER_PACKET pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-write-report\n");
    wprintf(L"                 Send IOCTL_HID_WRITE_REPORT with an invalid reportBuffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-read-xfer-packet\n");
    wprintf(L"                 Send IOCTL_HID_READ_REPORT with an invalid HID_XFER_PACKET pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-read-report\n");
    wprintf(L"                 Send IOCTL_HID_READ_REPORT with an invalid reportBuffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-input-xfer-packet\n");
    wprintf(L"                 Send IOCTL_HID_GET_INPUT_REPORT with an invalid HID_XFER_PACKET pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-input-report\n");
    wprintf(L"                 Send IOCTL_HID_GET_INPUT_REPORT with an invalid reportBuffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-set-output-xfer-packet\n");
    wprintf(L"                 Send IOCTL_HID_SET_OUTPUT_REPORT with an invalid HID_XFER_PACKET pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-set-output-report\n");
    wprintf(L"                 Send IOCTL_HID_SET_OUTPUT_REPORT with an invalid reportBuffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-report-descriptor\n");
    wprintf(L"                 Send IOCTL_HID_GET_REPORT_DESCRIPTOR with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-collection-descriptor\n");
    wprintf(L"                 Send IOCTL_HID_GET_COLLECTION_DESCRIPTOR with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-device-descriptor\n");
    wprintf(L"                 Send IOCTL_HID_GET_DEVICE_DESCRIPTOR with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-string\n");
    wprintf(L"                 Send IOCTL_HID_GET_STRING with an invalid input buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-indexed-string\n");
    wprintf(L"                 Send IOCTL_HID_GET_INDEXED_STRING with an invalid input buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-string-out\n");
    wprintf(L"                 Send IOCTL_HID_GET_STRING with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-indexed-string-out\n");
    wprintf(L"                 Send IOCTL_HID_GET_INDEXED_STRING with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-query-counters-short\n");
    wprintf(L"                 Call IOCTL_VIOINPUT_QUERY_COUNTERS with a short output buffer and verify that\n");
    wprintf(L"                 the driver returns STATUS_BUFFER_TOO_SMALL while still returning Size/Version\n");
    wprintf(L"  --ioctl-query-state-short\n");
    wprintf(L"                 Call IOCTL_VIOINPUT_QUERY_STATE with a short output buffer and verify that\n");
    wprintf(L"                 the driver returns STATUS_BUFFER_TOO_SMALL while still returning Size/Version\n");
    wprintf(L"  --ioctl-query-interrupt-info-short\n");
    wprintf(L"                 Call IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO with a short output buffer and verify that\n");
    wprintf(L"                 the driver returns STATUS_BUFFER_TOO_SMALL while still returning Size/Version\n");
    wprintf(L"  --ioctl-get-input-report\n");
    wprintf(L"                 Call DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT) and validate behavior\n");
    wprintf(L"  --hidd-get-input-report\n");
    wprintf(L"                 Call HidD_GetInputReport (exercises IOCTL_HID_GET_INPUT_REPORT) and validate behavior\n");
    wprintf(L"  --hidd-bad-set-output-report\n");
    wprintf(L"                 Call HidD_SetOutputReport with an invalid buffer pointer\n");
    wprintf(L"                 (negative test for IOCTL_HID_SET_OUTPUT_REPORT path; should fail, no crash)\n");
    wprintf(L"\n");
    wprintf(L"Notes:\n");
    wprintf(L"  - virtio-input detection: VID 0x1AF4, PID 0x0001 (keyboard) / 0x0002 (mouse) / 0x0003 (tablet)\n");
    wprintf(L"    (legacy/alternate PIDs: 0x1052 / 0x1011).\n");
    wprintf(L"  - Without filters, the tool prefers a virtio-input keyboard interface.\n");
    wprintf(L"  - Press Ctrl+C to exit the report read loop (a summary is printed on exit).\n");
}

static void selftest_logf(const wchar_t *device, const wchar_t *check, const wchar_t *status, const wchar_t *fmt, ...)
{
    va_list ap;
    wprintf(L"HIDTEST|SELFTEST|%ls|%ls|%ls", device ? device : L"<null>", check ? check : L"<null>",
            status ? status : L"<null>");
    if (fmt != NULL && fmt[0] != L'\0') {
        wprintf(L"|");
        va_start(ap, fmt);
        vwprintf(fmt, ap);
        va_end(ap);
    }
    wprintf(L"\n");
}

static int virtio_pid_allowed_for_keyboard(USHORT pid)
{
    return (pid == VIRTIO_INPUT_PID_KEYBOARD) || (pid == VIRTIO_INPUT_PID_MODERN) || (pid == VIRTIO_INPUT_PID_TRANSITIONAL);
}

static int virtio_pid_allowed_for_mouse(USHORT pid)
{
    return (pid == VIRTIO_INPUT_PID_MOUSE) || (pid == VIRTIO_INPUT_PID_MODERN) || (pid == VIRTIO_INPUT_PID_TRANSITIONAL);
}

static int virtio_pid_allowed_for_tablet(USHORT pid)
{
    return (pid == VIRTIO_INPUT_PID_TABLET) || (pid == VIRTIO_INPUT_PID_MODERN) || (pid == VIRTIO_INPUT_PID_TRANSITIONAL);
}

typedef enum _SELFTEST_DEVICE_KIND {
    SELFTEST_DEVICE_KIND_KEYBOARD = 1,
    SELFTEST_DEVICE_KIND_MOUSE = 2,
    SELFTEST_DEVICE_KIND_TABLET = 3,
} SELFTEST_DEVICE_KIND;

static int selftest_validate_device(const wchar_t *device_name, const SELECTED_DEVICE *dev, SELFTEST_DEVICE_KIND kind)
{
    int ok = 1;
    DWORD expected_input_len = 0;
    DWORD expected_output_len = 0;
    int check_output_len = 0;
    DWORD expected_desc_len = 0;
    USHORT expected_pid = 0;
    int (*pid_allowed)(USHORT) = NULL;

    switch (kind) {
    case SELFTEST_DEVICE_KIND_KEYBOARD:
        expected_input_len = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
        expected_output_len = VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN;
        check_output_len = 1;
        expected_desc_len = VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN;
        expected_pid = VIRTIO_INPUT_PID_KEYBOARD;
        pid_allowed = virtio_pid_allowed_for_keyboard;
        break;
    case SELFTEST_DEVICE_KIND_MOUSE:
        expected_input_len = VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN;
        expected_desc_len = VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN;
        expected_pid = VIRTIO_INPUT_PID_MOUSE;
        pid_allowed = virtio_pid_allowed_for_mouse;
        break;
    case SELFTEST_DEVICE_KIND_TABLET:
        expected_input_len = VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN;
        expected_desc_len = VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN;
        expected_pid = VIRTIO_INPUT_PID_TABLET;
        pid_allowed = virtio_pid_allowed_for_tablet;
        break;
    default:
        selftest_logf(device_name, L"CONFIG", L"FAIL", L"reason=unknown_kind");
        return 0;
    }

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        selftest_logf(device_name, L"OPEN", L"FAIL", L"reason=no_device_handle");
        return 0;
    }

    if (!dev->attr_valid) {
        selftest_logf(device_name, L"HidD_GetAttributes", L"FAIL", L"reason=unavailable");
        ok = 0;
    } else {
        if (dev->attr.VendorID == VIRTIO_INPUT_VID) {
            selftest_logf(device_name, L"VID", L"PASS", L"expected=0x%04X got=0x%04X", VIRTIO_INPUT_VID,
                          dev->attr.VendorID);
        } else {
            selftest_logf(device_name, L"VID", L"FAIL", L"expected=0x%04X got=0x%04X", VIRTIO_INPUT_VID,
                          dev->attr.VendorID);
            ok = 0;
        }

        if (pid_allowed(dev->attr.ProductID)) {
            selftest_logf(device_name, L"PID", L"PASS", L"allowed=0x%04X/0x%04X/0x%04X got=0x%04X",
                          expected_pid, VIRTIO_INPUT_PID_MODERN,
                          VIRTIO_INPUT_PID_TRANSITIONAL, dev->attr.ProductID);
        } else {
            selftest_logf(device_name, L"PID", L"FAIL", L"allowed=0x%04X/0x%04X/0x%04X got=0x%04X",
                          expected_pid, VIRTIO_INPUT_PID_MODERN,
                          VIRTIO_INPUT_PID_TRANSITIONAL, dev->attr.ProductID);
            ok = 0;
        }
    }

    if (!dev->caps_valid) {
        selftest_logf(device_name, L"HidP_GetCaps", L"FAIL", L"reason=unavailable");
        ok = 0;
    } else {
        if (dev->caps.InputReportByteLength == expected_input_len) {
            selftest_logf(device_name, L"InputReportByteLength", L"PASS", L"expected=%lu got=%u",
                          (unsigned long)expected_input_len, dev->caps.InputReportByteLength);
        } else {
            selftest_logf(device_name, L"InputReportByteLength", L"FAIL", L"expected=%lu got=%u",
                          (unsigned long)expected_input_len, dev->caps.InputReportByteLength);
            ok = 0;
        }

        if (check_output_len) {
            if (dev->caps.OutputReportByteLength == expected_output_len) {
                selftest_logf(device_name, L"OutputReportByteLength", L"PASS", L"expected=%lu got=%u",
                              (unsigned long)expected_output_len, dev->caps.OutputReportByteLength);
            } else {
                selftest_logf(device_name, L"OutputReportByteLength", L"FAIL", L"expected=%lu got=%u",
                              (unsigned long)expected_output_len, dev->caps.OutputReportByteLength);
                ok = 0;
            }
        }
    }

    if (!dev->report_desc_valid) {
        selftest_logf(device_name, L"IOCTL_HID_GET_REPORT_DESCRIPTOR", L"FAIL", L"reason=ioctl_failed");
        ok = 0;
    } else {
        if (dev->report_desc_len == expected_desc_len) {
            selftest_logf(device_name, L"ReportDescriptorLength", L"PASS", L"expected=%lu got=%lu",
                          (unsigned long)expected_desc_len, (unsigned long)dev->report_desc_len);
        } else {
            selftest_logf(device_name, L"ReportDescriptorLength", L"FAIL", L"expected=%lu got=%lu",
                          (unsigned long)expected_desc_len, (unsigned long)dev->report_desc_len);
            ok = 0;
        }
    }

    if (!dev->hid_report_desc_valid) {
        selftest_logf(device_name, L"IOCTL_HID_GET_DEVICE_DESCRIPTOR", L"FAIL", L"reason=ioctl_failed");
        ok = 0;
    } else if (dev->report_desc_valid && dev->hid_report_desc_len == dev->report_desc_len) {
        selftest_logf(device_name, L"HidDescriptorReportLength", L"PASS", L"hid=%lu ioctl=%lu",
                      (unsigned long)dev->hid_report_desc_len, (unsigned long)dev->report_desc_len);
    } else if (dev->report_desc_valid) {
        selftest_logf(device_name, L"HidDescriptorReportLength", L"FAIL", L"hid=%lu ioctl=%lu",
                      (unsigned long)dev->hid_report_desc_len, (unsigned long)dev->report_desc_len);
        ok = 0;
    } else {
        // Report descriptor length was unavailable (already a failure), but still log the HID-reported value.
        selftest_logf(device_name, L"HidDescriptorReportLength", L"FAIL", L"hid=%lu ioctl=<unavailable>",
                      (unsigned long)dev->hid_report_desc_len);
        ok = 0;
    }

    {
        DWORD coll_len = 0;
        DWORD coll_err = 0;
        DWORD coll_ioctl = 0;
        if (query_collection_descriptor_length(dev->handle, &coll_len, &coll_err, &coll_ioctl)) {
            if (coll_len == expected_desc_len) {
                selftest_logf(
                    device_name,
                    L"CollectionDescriptorLength",
                    L"PASS",
                    L"expected=%lu got=%lu ioctl=0x%08lX",
                    (unsigned long)expected_desc_len,
                    (unsigned long)coll_len,
                    (unsigned long)coll_ioctl);
            } else {
                selftest_logf(
                    device_name,
                    L"CollectionDescriptorLength",
                    L"FAIL",
                    L"expected=%lu got=%lu ioctl=0x%08lX",
                    (unsigned long)expected_desc_len,
                    (unsigned long)coll_len,
                    (unsigned long)coll_ioctl);
                ok = 0;
            }
        } else if (coll_err == ERROR_INVALID_FUNCTION || coll_err == ERROR_NOT_SUPPORTED) {
            selftest_logf(device_name, L"CollectionDescriptorLength", L"SKIP", L"reason=unsupported");
        } else {
            selftest_logf(device_name, L"IOCTL_HID_GET_COLLECTION_DESCRIPTOR", L"FAIL", L"err=%lu", (unsigned long)coll_err);
            ok = 0;
        }
    }

    selftest_logf(device_name, L"RESULT", ok ? L"PASS" : L"FAIL", L"");
    return ok;
}

static int run_selftest(const OPTIONS *opt)
{
    int ok = 1;
    int test_keyboard = 0;
    int test_mouse = 0;
    int test_tablet = 0;

    if (opt != NULL && opt->json) {
        return run_selftest_json(opt);
    }

    if (opt != NULL && (opt->want_keyboard || opt->want_mouse || opt->want_tablet)) {
        test_keyboard = opt->want_keyboard;
        test_mouse = opt->want_mouse;
        test_tablet = opt->want_tablet;
    } else {
        test_keyboard = 1;
        test_mouse = 1;
        test_tablet = 0;
    }

    if (test_keyboard) {
        OPTIONS sel;
        SELECTED_DEVICE dev;
        ZeroMemory(&sel, sizeof(sel));
        ZeroMemory(&dev, sizeof(dev));
        dev.handle = INVALID_HANDLE_VALUE;
        sel.want_keyboard = 1;
        sel.have_vid = 1;
        sel.vid = VIRTIO_INPUT_VID;
        sel.quiet = opt ? opt->quiet : 0;

        if (!enumerate_hid_devices(&sel, &dev)) {
            selftest_logf(L"keyboard", L"ENUM", L"FAIL", L"reason=no_matching_device");
            ok = 0;
        } else if (!selftest_validate_device(L"keyboard", &dev, SELFTEST_DEVICE_KIND_KEYBOARD)) {
            ok = 0;
        }
        free_selected_device(&dev);
    }

    if (test_mouse) {
        OPTIONS sel;
        SELECTED_DEVICE dev;
        ZeroMemory(&sel, sizeof(sel));
        ZeroMemory(&dev, sizeof(dev));
        dev.handle = INVALID_HANDLE_VALUE;
        sel.want_mouse = 1;
        sel.have_vid = 1;
        sel.vid = VIRTIO_INPUT_VID;
        sel.quiet = opt ? opt->quiet : 0;

        if (!enumerate_hid_devices(&sel, &dev)) {
            selftest_logf(L"mouse", L"ENUM", L"FAIL", L"reason=no_matching_device");
            ok = 0;
        } else if (!selftest_validate_device(L"mouse", &dev, SELFTEST_DEVICE_KIND_MOUSE)) {
            ok = 0;
        }
        free_selected_device(&dev);
    }

    if (test_tablet) {
        OPTIONS sel;
        SELECTED_DEVICE dev;
        ZeroMemory(&sel, sizeof(sel));
        ZeroMemory(&dev, sizeof(dev));
        dev.handle = INVALID_HANDLE_VALUE;
        sel.want_tablet = 1;
        sel.have_vid = 1;
        sel.vid = VIRTIO_INPUT_VID;
        sel.quiet = opt ? opt->quiet : 0;

        if (!enumerate_hid_devices(&sel, &dev)) {
            selftest_logf(L"tablet", L"ENUM", L"FAIL", L"reason=no_matching_device");
            ok = 0;
        } else if (!selftest_validate_device(L"tablet", &dev, SELFTEST_DEVICE_KIND_TABLET)) {
            ok = 0;
        }
        free_selected_device(&dev);
    }

    selftest_logf(L"SUMMARY", L"RESULT", ok ? L"PASS" : L"FAIL", L"");
    return ok ? 0 : 1;
}

static void free_selected_device(SELECTED_DEVICE *dev)
{
    if (dev == NULL) {
        return;
    }
    if (dev->handle != INVALID_HANDLE_VALUE && dev->handle != NULL) {
        CloseHandle(dev->handle);
    }
    free(dev->path);
    ZeroMemory(dev, sizeof(*dev));
}

static int device_matches_opts(const OPTIONS *opt, DWORD iface_index, const HIDD_ATTRIBUTES *attr)
{
    if (opt->have_index && opt->index != iface_index) {
        return 0;
    }
    if (opt->have_vid && attr->VendorID != opt->vid) {
        return 0;
    }
    if (opt->have_pid && attr->ProductID != opt->pid) {
        return 0;
    }
    return 1;
}

static void print_device_strings(HANDLE handle)
{
    WCHAR s[256];

    if (HidD_GetManufacturerString(handle, s, sizeof(s))) {
        s[(sizeof(s) / sizeof(s[0])) - 1] = L'\0';
        wprintf(L"      Manufacturer: %ls\n", s);
    }
    if (HidD_GetProductString(handle, s, sizeof(s))) {
        s[(sizeof(s) / sizeof(s[0])) - 1] = L'\0';
        wprintf(L"      Product:      %ls\n", s);
    }
    if (HidD_GetSerialNumberString(handle, s, sizeof(s))) {
        s[(sizeof(s) / sizeof(s[0])) - 1] = L'\0';
        wprintf(L"      Serial:       %ls\n", s);
    }
}

static int query_hid_caps(HANDLE handle, HIDP_CAPS *caps_out)
{
    PHIDP_PREPARSED_DATA ppd = NULL;
    NTSTATUS st;

    if (!HidD_GetPreparsedData(handle, &ppd)) {
        return 0;
    }

    st = HidP_GetCaps(ppd, caps_out);
    HidD_FreePreparsedData(ppd);

    return st == HIDP_STATUS_SUCCESS;
}

static int query_report_descriptor_length(HANDLE handle, DWORD *len_out)
{
    BYTE buf[4096];
    DWORD bytes = 0;
    BOOL ok;

    if (len_out == NULL) {
        return 0;
    }

    ZeroMemory(buf, sizeof(buf));

    ok = DeviceIoControl(handle, IOCTL_HID_GET_REPORT_DESCRIPTOR, NULL, 0, buf, (DWORD)sizeof(buf),
                         &bytes, NULL);
    if (!ok || bytes == 0) {
        bytes = 0;
        ok = DeviceIoControl(handle, IOCTL_HID_GET_REPORT_DESCRIPTOR_ALT, NULL, 0, buf,
                             (DWORD)sizeof(buf), &bytes, NULL);
        if (!ok || bytes == 0) {
            return 0;
        }
    }

    *len_out = bytes;
    return 1;
}

static int query_collection_descriptor_length(HANDLE handle, DWORD *len_out, DWORD *err_out, DWORD *ioctl_out)
{
    BYTE buf[4096];
    DWORD bytes = 0;
    BOOL ok;

    if (len_out == NULL) {
        return 0;
    }
    *len_out = 0;
    if (err_out != NULL) {
        *err_out = 0;
    }
    if (ioctl_out != NULL) {
        *ioctl_out = 0;
    }

    ZeroMemory(buf, sizeof(buf));

    SetLastError(ERROR_SUCCESS);
    ok = DeviceIoControl(handle, IOCTL_HID_GET_COLLECTION_DESCRIPTOR, NULL, 0, buf, (DWORD)sizeof(buf), &bytes, NULL);
    if (ok && bytes != 0) {
        *len_out = bytes;
        if (ioctl_out != NULL) {
            *ioctl_out = IOCTL_HID_GET_COLLECTION_DESCRIPTOR;
        }
        return 1;
    }

    bytes = 0;
    SetLastError(ERROR_SUCCESS);
    ok = DeviceIoControl(handle, IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT, NULL, 0, buf, (DWORD)sizeof(buf), &bytes, NULL);
    if (ok && bytes != 0) {
        *len_out = bytes;
        if (ioctl_out != NULL) {
            *ioctl_out = IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT;
        }
        return 1;
    }

    if (err_out != NULL) {
        *err_out = ok ? ERROR_NO_DATA : GetLastError();
    }
    if (ioctl_out != NULL) {
        *ioctl_out = IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT;
    }
    return 0;
}

static int query_hid_descriptor_report_length(HANDLE handle, DWORD *len_out)
{
    BYTE buf[256];
    DWORD bytes = 0;
    BOOL ok;
    const HID_DESCRIPTOR_MIN *desc;
    DWORD min_bytes;
    DWORD i;

    if (len_out == NULL) {
        return 0;
    }

    ZeroMemory(buf, sizeof(buf));
    ok = DeviceIoControl(handle, IOCTL_HID_GET_DEVICE_DESCRIPTOR, NULL, 0, buf, (DWORD)sizeof(buf),
                         &bytes, NULL);
    if (!ok) {
        return 0;
    }

    if (bytes < sizeof(HID_DESCRIPTOR_MIN)) {
        return 0;
    }

    desc = (const HID_DESCRIPTOR_MIN *)buf;
    min_bytes = (DWORD)(6 + desc->bNumDescriptors * 3);
    if (bytes < min_bytes) {
        return 0;
    }

    // Look for the report descriptor entry.
    for (i = 0; i < desc->bNumDescriptors; i++) {
        const BYTE *entry = buf + 6 + i * 3;
        BYTE report_type = entry[0];
        USHORT report_len = (USHORT)(entry[1] | ((USHORT)entry[2] << 8));
        if (report_type == HID_REPORT_DESCRIPTOR_TYPE) {
            *len_out = report_len;
            return 1;
        }
    }

    return 0;
}

static HANDLE open_hid_path(const WCHAR *path, DWORD *desired_access_out)
{
    HANDLE h;
    DWORD access;

    access = GENERIC_READ | GENERIC_WRITE;
    h = CreateFileW(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    if (h != INVALID_HANDLE_VALUE) {
        if (desired_access_out != NULL) {
            *desired_access_out = access;
        }
        return h;
    }

    access = GENERIC_READ;
    h = CreateFileW(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    if (desired_access_out != NULL) {
        *desired_access_out = (h == INVALID_HANDLE_VALUE) ? 0 : access;
    }
    return h;
}

static int list_hid_devices_json(void)
{
    GUID hid_guid;
    HDEVINFO devinfo;
    SP_DEVICE_INTERFACE_DATA iface;
    DWORD iface_index;
    int first;
    int ok;

    HidD_GetHidGuid(&hid_guid);

    devinfo = SetupDiGetClassDevsW(&hid_guid, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devinfo == INVALID_HANDLE_VALUE) {
        fprint_last_error_w(stderr, L"SetupDiGetClassDevs");
        wprintf(L"[]\n");
        return 0;
    }

    iface_index = 0;
    first = 1;
    ok = 1;
    wprintf(L"[");
    for (;;) {
        DWORD required = 0;
        PSP_DEVICE_INTERFACE_DETAIL_DATA_W detail = NULL;
        HANDLE handle = INVALID_HANDLE_VALUE;
        HIDD_ATTRIBUTES attr;
        HIDP_CAPS caps;
        int attr_valid = 0;
        int caps_valid = 0;
        DWORD report_desc_len = 0;
        int report_desc_valid = 0;

        ZeroMemory(&iface, sizeof(iface));
        iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(devinfo, NULL, &hid_guid, iface_index, &iface)) {
            DWORD err = GetLastError();
            if (err != ERROR_NO_MORE_ITEMS) {
                fprint_win32_error_w(stderr, L"SetupDiEnumDeviceInterfaces", err);
                ok = 0;
            }
            break;
        }

        SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, NULL, 0, &required, NULL);
        if (required == 0) {
            fprint_last_error_w(stderr, L"SetupDiGetDeviceInterfaceDetail (size query)");
            ok = 0;
            iface_index++;
            continue;
        }

        detail = (PSP_DEVICE_INTERFACE_DETAIL_DATA_W)malloc(required);
        if (detail == NULL) {
            fwprintf(stderr, L"Out of memory\n");
            ok = 0;
            break;
        }

        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, required, NULL, NULL)) {
            fprint_last_error_w(stderr, L"SetupDiGetDeviceInterfaceDetail");
            free(detail);
            ok = 0;
            iface_index++;
            continue;
        }

        handle = open_hid_path(detail->DevicePath, NULL);
        if (handle != INVALID_HANDLE_VALUE) {
            ZeroMemory(&attr, sizeof(attr));
            attr.Size = sizeof(attr);
            if (HidD_GetAttributes(handle, &attr)) {
                attr_valid = 1;
            }

            ZeroMemory(&caps, sizeof(caps));
            caps_valid = query_hid_caps(handle, &caps);

            report_desc_valid = query_report_descriptor_length(handle, &report_desc_len);

            CloseHandle(handle);
        } else {
            // Still emit the device entry but without VID/PID/caps info.
            fprint_last_error_w(stderr, L"CreateFile");
        }

        if (!first) {
            wprintf(L",");
        }
        first = 0;

        wprintf(L"{");
        wprintf(L"\"index\":%lu,", iface_index);
        wprintf(L"\"path\":");
        json_print_string_w(detail->DevicePath);
        wprintf(L",\"vid\":");
        if (attr_valid) {
            wprintf(L"%u", (unsigned)attr.VendorID);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"pid\":");
        if (attr_valid) {
            wprintf(L"%u", (unsigned)attr.ProductID);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"usagePage\":");
        if (caps_valid) {
            wprintf(L"%u", (unsigned)caps.UsagePage);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"usage\":");
        if (caps_valid) {
            wprintf(L"%u", (unsigned)caps.Usage);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"inputLen\":");
        if (caps_valid) {
            wprintf(L"%u", (unsigned)caps.InputReportByteLength);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"outputLen\":");
        if (caps_valid) {
            wprintf(L"%u", (unsigned)caps.OutputReportByteLength);
        } else {
            wprintf(L"null");
        }
        wprintf(L",\"reportDescLen\":");
        if (report_desc_valid) {
            wprintf(L"%lu", report_desc_len);
        } else {
            wprintf(L"null");
        }
        wprintf(L"}");

        free(detail);
        iface_index++;
    }

    wprintf(L"]\n");
    SetupDiDestroyDeviceInfoList(devinfo);
    return ok;
}

typedef struct SELFTEST_DEVICE_INFO {
    int found;
    DWORD index;
    WCHAR *path;
    HIDD_ATTRIBUTES attr;
    int attr_valid;
    HIDP_CAPS caps;
    int caps_valid;
    DWORD report_desc_len;
    int report_desc_valid;
    DWORD hid_report_desc_len;
    int hid_report_desc_valid;
    DWORD collection_desc_len;
    int collection_desc_valid;
    DWORD collection_desc_ioctl;
    DWORD collection_desc_err;
} SELFTEST_DEVICE_INFO;

typedef struct SELFTEST_FAILURE {
    const WCHAR *device;
    const WCHAR *field;
    const WCHAR *message;
    int have_expected;
    DWORD expected;
    int have_actual;
    DWORD actual;
} SELFTEST_FAILURE;

#define SELFTEST_MAX_FAILURES 64

static void free_selftest_device_info(SELFTEST_DEVICE_INFO *info)
{
    if (info == NULL) {
        return;
    }
    free(info->path);
    ZeroMemory(info, sizeof(*info));
}

static void selftest_add_failure(SELFTEST_FAILURE failures[SELFTEST_MAX_FAILURES], size_t *count, const WCHAR *device,
                                 const WCHAR *field, const WCHAR *message, int have_expected, DWORD expected,
                                 int have_actual, DWORD actual)
{
    SELFTEST_FAILURE *f;
    if (count == NULL || *count >= SELFTEST_MAX_FAILURES) {
        return;
    }
    f = &failures[*count];
    f->device = device;
    f->field = field;
    f->message = message;
    f->have_expected = have_expected;
    f->expected = expected;
    f->have_actual = have_actual;
    f->actual = actual;
    (*count)++;
}

static void json_print_selftest_device_info(const SELFTEST_DEVICE_INFO *info)
{
    if (info == NULL || !info->found) {
        wprintf(L"null");
        return;
    }

    wprintf(L"{\"index\":%lu,", info->index);
    wprintf(L"\"path\":");
    json_print_string_w(info->path);
    wprintf(L",\"vid\":");
    if (info->attr_valid) {
        wprintf(L"%u", (unsigned)info->attr.VendorID);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"pid\":");
    if (info->attr_valid) {
        wprintf(L"%u", (unsigned)info->attr.ProductID);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"usagePage\":");
    if (info->caps_valid) {
        wprintf(L"%u", (unsigned)info->caps.UsagePage);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"usage\":");
    if (info->caps_valid) {
        wprintf(L"%u", (unsigned)info->caps.Usage);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"inputLen\":");
    if (info->caps_valid) {
        wprintf(L"%u", (unsigned)info->caps.InputReportByteLength);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"outputLen\":");
    if (info->caps_valid) {
        wprintf(L"%u", (unsigned)info->caps.OutputReportByteLength);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"reportDescLen\":");
    if (info->report_desc_valid) {
        wprintf(L"%lu", info->report_desc_len);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"hidReportDescLen\":");
    if (info->hid_report_desc_valid) {
        wprintf(L"%lu", info->hid_report_desc_len);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"collectionDescLen\":");
    if (info->collection_desc_valid) {
        wprintf(L"%lu", info->collection_desc_len);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"collectionDescIoctl\":");
    if (info->collection_desc_valid) {
        wprintf(L"%lu", (unsigned long)info->collection_desc_ioctl);
    } else {
        wprintf(L"null");
    }
    wprintf(L",\"collectionDescErr\":");
    if (!info->collection_desc_valid && info->collection_desc_err != 0) {
        wprintf(L"%lu", (unsigned long)info->collection_desc_err);
    } else {
        wprintf(L"null");
    }
    wprintf(L"}");
}

static int run_selftest_json(const OPTIONS *opt)
{
    GUID hid_guid;
    HDEVINFO devinfo;
    SP_DEVICE_INTERFACE_DATA iface;
    DWORD iface_index;
    int need_keyboard;
    int need_mouse;
    int need_tablet;
    SELFTEST_DEVICE_INFO kbd;
    SELFTEST_DEVICE_INFO mouse;
    SELFTEST_DEVICE_INFO tablet;
    SELFTEST_FAILURE failures[SELFTEST_MAX_FAILURES];
    size_t failure_count;
    int pass;

    ZeroMemory(&kbd, sizeof(kbd));
    ZeroMemory(&mouse, sizeof(mouse));
    ZeroMemory(&tablet, sizeof(tablet));
    ZeroMemory(failures, sizeof(failures));
    failure_count = 0;
    pass = 1;

    if (opt != NULL && (opt->want_keyboard || opt->want_mouse || opt->want_tablet)) {
        need_keyboard = opt->want_keyboard;
        need_mouse = opt->want_mouse;
        need_tablet = opt->want_tablet;
    } else {
        // Default selftest covers the contract v1 keyboard+mouse devices.
        need_keyboard = 1;
        need_mouse = 1;
        need_tablet = 0;
    }

    HidD_GetHidGuid(&hid_guid);
    devinfo = SetupDiGetClassDevsW(&hid_guid, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devinfo == INVALID_HANDLE_VALUE) {
        if (opt->json) {
            wprintf(L"{\"pass\":false,\"keyboard\":null,\"mouse\":null,\"tablet\":null,\"failures\":[");
            wprintf(L"{\"device\":\"global\",\"field\":\"enumeration\",\"message\":\"SetupDiGetClassDevs failed\"}");
            wprintf(L"]}\n");
        } else {
            wprintf(L"Selftest: SetupDiGetClassDevs failed\n");
            print_last_error_w(L"SetupDiGetClassDevs");
        }
        return 1;
    }

    iface_index = 0;
    for (;;) {
        DWORD required = 0;
        PSP_DEVICE_INTERFACE_DETAIL_DATA_W detail = NULL;
        HANDLE handle = INVALID_HANDLE_VALUE;
        HIDD_ATTRIBUTES attr;
        HIDP_CAPS caps;
        int attr_valid = 0;
        int caps_valid = 0;
        DWORD report_desc_len = 0;
        int report_desc_valid = 0;
        DWORD hid_report_desc_len = 0;
        int hid_report_desc_valid = 0;
        DWORD collection_desc_len = 0;
        DWORD collection_desc_ioctl = 0;
        DWORD collection_desc_err = 0;
        int collection_desc_valid = 0;
        int is_virtio = 0;
        int is_keyboard = 0;
        int is_mouse = 0;
        int is_tablet = 0;

        ZeroMemory(&iface, sizeof(iface));
        iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(devinfo, NULL, &hid_guid, iface_index, &iface)) {
            break;
        }

        SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, NULL, 0, &required, NULL);
        if (required == 0) {
            iface_index++;
            continue;
        }

        detail = (PSP_DEVICE_INTERFACE_DETAIL_DATA_W)malloc(required);
        if (detail == NULL) {
            selftest_add_failure(failures, &failure_count, L"global", L"memory", L"out of memory", 0, 0, 0, 0);
            pass = 0;
            break;
        }
        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, required, NULL, NULL)) {
            free(detail);
            iface_index++;
            continue;
        }

        handle = open_hid_path(detail->DevicePath, NULL);
        if (handle == INVALID_HANDLE_VALUE) {
            // We cannot determine whether this is a virtio-input device without HidD_GetAttributes.
            free(detail);
            iface_index++;
            continue;
        }

        ZeroMemory(&attr, sizeof(attr));
        attr.Size = sizeof(attr);
        if (HidD_GetAttributes(handle, &attr)) {
            attr_valid = 1;
            is_virtio = is_virtio_input_device(&attr);
        }

        ZeroMemory(&caps, sizeof(caps));
        caps_valid = query_hid_caps(handle, &caps);

        report_desc_valid = query_report_descriptor_length(handle, &report_desc_len);
        hid_report_desc_valid = query_hid_descriptor_report_length(handle, &hid_report_desc_len);
        collection_desc_valid = query_collection_descriptor_length(handle, &collection_desc_len, &collection_desc_err, &collection_desc_ioctl);

        CloseHandle(handle);

        if (caps_valid) {
            is_keyboard = (caps.UsagePage == 0x01 && caps.Usage == 0x06);
            is_mouse = (caps.UsagePage == 0x01 && caps.Usage == 0x02);
        } else if (attr_valid) {
            // Fallback to PID-based identity if caps are not available.
            if (attr.ProductID == VIRTIO_INPUT_PID_KEYBOARD) {
                is_keyboard = 1;
            } else if (attr.ProductID == VIRTIO_INPUT_PID_MOUSE) {
                is_mouse = 1;
            }
        }

        is_tablet = 0;
        if (is_virtio && attr_valid && (attr.ProductID == VIRTIO_INPUT_PID_TABLET)) {
            is_tablet = 1;
        } else if (is_virtio) {
            // Tablet shares the mouse top-level usage (0x01:0x02). Use descriptor-length heuristics
            // to keep it distinct from the relative mouse collection.
            if (report_desc_valid && report_desc_len == VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
                is_tablet = 1;
            } else if (hid_report_desc_valid && hid_report_desc_len == VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
                is_tablet = 1;
            }
        }
        if (is_tablet) {
            // Avoid accidentally selecting a virtio-input tablet as the "mouse".
            is_mouse = 0;
        }

        if (is_virtio && is_keyboard && need_keyboard && !kbd.found) {
            kbd.found = 1;
            kbd.index = iface_index;
            kbd.path = wcsdup_heap(detail->DevicePath);
            kbd.attr = attr;
            kbd.attr_valid = attr_valid;
            kbd.caps = caps;
            kbd.caps_valid = caps_valid;
            kbd.report_desc_len = report_desc_len;
            kbd.report_desc_valid = report_desc_valid;
            kbd.hid_report_desc_len = hid_report_desc_len;
            kbd.hid_report_desc_valid = hid_report_desc_valid;
            kbd.collection_desc_len = collection_desc_len;
            kbd.collection_desc_valid = collection_desc_valid;
            kbd.collection_desc_ioctl = collection_desc_ioctl;
            kbd.collection_desc_err = collection_desc_err;
        } else if (is_virtio && is_mouse && need_mouse && !mouse.found) {
            mouse.found = 1;
            mouse.index = iface_index;
            mouse.path = wcsdup_heap(detail->DevicePath);
            mouse.attr = attr;
            mouse.attr_valid = attr_valid;
            mouse.caps = caps;
            mouse.caps_valid = caps_valid;
            mouse.report_desc_len = report_desc_len;
            mouse.report_desc_valid = report_desc_valid;
            mouse.hid_report_desc_len = hid_report_desc_len;
            mouse.hid_report_desc_valid = hid_report_desc_valid;
            mouse.collection_desc_len = collection_desc_len;
            mouse.collection_desc_valid = collection_desc_valid;
            mouse.collection_desc_ioctl = collection_desc_ioctl;
            mouse.collection_desc_err = collection_desc_err;
        } else if (is_virtio && is_tablet && need_tablet && !tablet.found) {
            tablet.found = 1;
            tablet.index = iface_index;
            tablet.path = wcsdup_heap(detail->DevicePath);
            tablet.attr = attr;
            tablet.attr_valid = attr_valid;
            tablet.caps = caps;
            tablet.caps_valid = caps_valid;
            tablet.report_desc_len = report_desc_len;
            tablet.report_desc_valid = report_desc_valid;
            tablet.hid_report_desc_len = hid_report_desc_len;
            tablet.hid_report_desc_valid = hid_report_desc_valid;
            tablet.collection_desc_len = collection_desc_len;
            tablet.collection_desc_valid = collection_desc_valid;
            tablet.collection_desc_ioctl = collection_desc_ioctl;
            tablet.collection_desc_err = collection_desc_err;
        }

        free(detail);

        if ((!need_keyboard || kbd.found) && (!need_mouse || mouse.found) && (!need_tablet || tablet.found)) {
            break;
        }

        iface_index++;
    }

    SetupDiDestroyDeviceInfoList(devinfo);

    if (need_keyboard && !kbd.found) {
        selftest_add_failure(failures, &failure_count, L"keyboard", L"present", L"not found", 0, 0, 0, 0);
        pass = 0;
    }
    if (need_mouse && !mouse.found) {
        selftest_add_failure(failures, &failure_count, L"mouse", L"present", L"not found", 0, 0, 0, 0);
        pass = 0;
    }
    if (need_tablet && !tablet.found) {
        selftest_add_failure(failures, &failure_count, L"tablet", L"present", L"not found", 0, 0, 0, 0);
        pass = 0;
    }

    if (need_keyboard && kbd.found) {
        if (!kbd.caps_valid) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"caps",
                                 L"HidD_GetPreparsedData/HidP_GetCaps failed", 0, 0, 0, 0);
            pass = 0;
        } else {
            if (kbd.caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN) {
                selftest_add_failure(failures, &failure_count, L"keyboard", L"inputLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN, 1, kbd.caps.InputReportByteLength);
                pass = 0;
            }
            if (kbd.caps.OutputReportByteLength != VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN) {
                selftest_add_failure(failures, &failure_count, L"keyboard", L"outputLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN, 1, kbd.caps.OutputReportByteLength);
                pass = 0;
            }
        }

        if (!kbd.report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"reportDescLen",
                                 L"IOCTL_HID_GET_REPORT_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (kbd.report_desc_len != VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"reportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN, 1, kbd.report_desc_len);
            pass = 0;
        }

        if (!kbd.hid_report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"hidReportDescLen",
                                 L"IOCTL_HID_GET_DEVICE_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (kbd.hid_report_desc_len != VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"hidReportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN, 1, kbd.hid_report_desc_len);
            pass = 0;
        }

        if (kbd.report_desc_valid && kbd.hid_report_desc_valid && kbd.report_desc_len != kbd.hid_report_desc_len) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"reportDescLenConsistency",
                                 L"IOCTL vs HID descriptor report length mismatch", 1, kbd.report_desc_len, 1,
                                 kbd.hid_report_desc_len);
            pass = 0;
        }

        if (kbd.collection_desc_valid) {
            if (kbd.collection_desc_len != VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN) {
                selftest_add_failure(failures, &failure_count, L"keyboard", L"collectionDescLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN, 1, kbd.collection_desc_len);
                pass = 0;
            }
        } else if (kbd.collection_desc_err == ERROR_INVALID_FUNCTION || kbd.collection_desc_err == ERROR_NOT_SUPPORTED) {
            // IOCTL not supported on this OS/stack (common on Win7). Treat as informational.
        } else if (kbd.collection_desc_err != 0) {
            selftest_add_failure(failures, &failure_count, L"keyboard", L"collectionDescLen",
                                 L"IOCTL_HID_GET_COLLECTION_DESCRIPTOR failed", 0, 0, 1, kbd.collection_desc_err);
            pass = 0;
        }
    }

    if (need_mouse && mouse.found) {
        if (!mouse.caps_valid) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"caps",
                                 L"HidD_GetPreparsedData/HidP_GetCaps failed", 0, 0, 0, 0);
            pass = 0;
        } else {
            if (mouse.caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN) {
                selftest_add_failure(failures, &failure_count, L"mouse", L"inputLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN, 1, mouse.caps.InputReportByteLength);
                pass = 0;
            }
        }

        if (!mouse.report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"reportDescLen",
                                 L"IOCTL_HID_GET_REPORT_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (mouse.report_desc_len != VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"reportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN, 1, mouse.report_desc_len);
            pass = 0;
        }

        if (!mouse.hid_report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"hidReportDescLen",
                                 L"IOCTL_HID_GET_DEVICE_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (mouse.hid_report_desc_len != VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"hidReportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN, 1, mouse.hid_report_desc_len);
            pass = 0;
        }

        if (mouse.report_desc_valid && mouse.hid_report_desc_valid && mouse.report_desc_len != mouse.hid_report_desc_len) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"reportDescLenConsistency",
                                 L"IOCTL vs HID descriptor report length mismatch", 1, mouse.report_desc_len, 1,
                                 mouse.hid_report_desc_len);
            pass = 0;
        }

        if (mouse.collection_desc_valid) {
            if (mouse.collection_desc_len != VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN) {
                selftest_add_failure(failures, &failure_count, L"mouse", L"collectionDescLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN, 1, mouse.collection_desc_len);
                pass = 0;
            }
        } else if (mouse.collection_desc_err == ERROR_INVALID_FUNCTION || mouse.collection_desc_err == ERROR_NOT_SUPPORTED) {
            // IOCTL not supported on this OS/stack (common on Win7). Treat as informational.
        } else if (mouse.collection_desc_err != 0) {
            selftest_add_failure(failures, &failure_count, L"mouse", L"collectionDescLen",
                                 L"IOCTL_HID_GET_COLLECTION_DESCRIPTOR failed", 0, 0, 1, mouse.collection_desc_err);
            pass = 0;
        }
    }

    if (need_tablet && tablet.found) {
        if (!tablet.caps_valid) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"caps",
                                 L"HidD_GetPreparsedData/HidP_GetCaps failed", 0, 0, 0, 0);
            pass = 0;
        } else {
            if (tablet.caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN) {
                selftest_add_failure(failures, &failure_count, L"tablet", L"inputLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN, 1, tablet.caps.InputReportByteLength);
                pass = 0;
            }
        }

        if (!tablet.report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"reportDescLen",
                                 L"IOCTL_HID_GET_REPORT_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (tablet.report_desc_len != VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"reportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN, 1, tablet.report_desc_len);
            pass = 0;
        }

        if (!tablet.hid_report_desc_valid) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"hidReportDescLen",
                                 L"IOCTL_HID_GET_DEVICE_DESCRIPTOR failed", 0, 0, 0, 0);
            pass = 0;
        } else if (tablet.hid_report_desc_len != VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"hidReportDescLen", NULL, 1,
                                 VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN, 1, tablet.hid_report_desc_len);
            pass = 0;
        }

        if (tablet.report_desc_valid && tablet.hid_report_desc_valid && tablet.report_desc_len != tablet.hid_report_desc_len) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"reportDescLenConsistency",
                                 L"IOCTL vs HID descriptor report length mismatch", 1, tablet.report_desc_len, 1,
                                 tablet.hid_report_desc_len);
            pass = 0;
        }

        if (tablet.collection_desc_valid) {
            if (tablet.collection_desc_len != VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
                selftest_add_failure(failures, &failure_count, L"tablet", L"collectionDescLen", NULL, 1,
                                     VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN, 1, tablet.collection_desc_len);
                pass = 0;
            }
        } else if (tablet.collection_desc_err == ERROR_INVALID_FUNCTION || tablet.collection_desc_err == ERROR_NOT_SUPPORTED) {
            // IOCTL not supported on this OS/stack (common on Win7). Treat as informational.
        } else if (tablet.collection_desc_err != 0) {
            selftest_add_failure(failures, &failure_count, L"tablet", L"collectionDescLen",
                                 L"IOCTL_HID_GET_COLLECTION_DESCRIPTOR failed", 0, 0, 1, tablet.collection_desc_err);
            pass = 0;
        }
    }

    if (opt->json) {
        size_t i;
        wprintf(L"{\"pass\":%ls,\"keyboard\":", pass ? L"true" : L"false");
        json_print_selftest_device_info(&kbd);
        wprintf(L",\"mouse\":");
        json_print_selftest_device_info(&mouse);
        wprintf(L",\"tablet\":");
        json_print_selftest_device_info(&tablet);
        wprintf(L",\"failures\":[");
        for (i = 0; i < failure_count; i++) {
            const SELFTEST_FAILURE *f = &failures[i];
            if (i != 0) {
                wprintf(L",");
            }
            wprintf(L"{\"device\":");
            json_print_string_w(f->device);
            wprintf(L",\"field\":");
            json_print_string_w(f->field);
            if (f->message != NULL) {
                wprintf(L",\"message\":");
                json_print_string_w(f->message);
            }
            if (f->have_expected) {
                wprintf(L",\"expected\":%lu", f->expected);
            }
            if (f->have_actual) {
                wprintf(L",\"actual\":%lu", f->actual);
            }
            wprintf(L"}");
        }
        wprintf(L"]}\n");
    } else {
        size_t i;
        wprintf(L"hidtest selftest: %ls\n", pass ? L"PASS" : L"FAIL");
        if (need_keyboard) {
            if (kbd.found) {
                wprintf(L"  keyboard: index=%lu path=%ls\n", kbd.index, kbd.path ? kbd.path : L"<null>");
                if (kbd.caps_valid) {
                    wprintf(L"    inputLen=%u outputLen=%u usagePage=%04X usage=%04X\n",
                            (unsigned)kbd.caps.InputReportByteLength, (unsigned)kbd.caps.OutputReportByteLength,
                            (unsigned)kbd.caps.UsagePage, (unsigned)kbd.caps.Usage);
                }
                if (kbd.report_desc_valid) {
                    wprintf(L"    reportDescLen=%lu\n", kbd.report_desc_len);
                }
                if (kbd.hid_report_desc_valid) {
                    wprintf(L"    hidReportDescLen=%lu\n", kbd.hid_report_desc_len);
                }
            } else {
                wprintf(L"  keyboard: not found\n");
            }
        }
        if (need_mouse) {
            if (mouse.found) {
                wprintf(L"  mouse: index=%lu path=%ls\n", mouse.index, mouse.path ? mouse.path : L"<null>");
                if (mouse.caps_valid) {
                    wprintf(L"    inputLen=%u outputLen=%u usagePage=%04X usage=%04X\n",
                            (unsigned)mouse.caps.InputReportByteLength, (unsigned)mouse.caps.OutputReportByteLength,
                            (unsigned)mouse.caps.UsagePage, (unsigned)mouse.caps.Usage);
                }
                if (mouse.report_desc_valid) {
                    wprintf(L"    reportDescLen=%lu\n", mouse.report_desc_len);
                }
                if (mouse.hid_report_desc_valid) {
                    wprintf(L"    hidReportDescLen=%lu\n", mouse.hid_report_desc_len);
                }
            } else {
                wprintf(L"  mouse: not found\n");
            }
        }
        if (need_tablet) {
            if (tablet.found) {
                wprintf(L"  tablet: index=%lu path=%ls\n", tablet.index, tablet.path ? tablet.path : L"<null>");
                if (tablet.caps_valid) {
                    wprintf(L"    inputLen=%u outputLen=%u usagePage=%04X usage=%04X\n",
                            (unsigned)tablet.caps.InputReportByteLength, (unsigned)tablet.caps.OutputReportByteLength,
                            (unsigned)tablet.caps.UsagePage, (unsigned)tablet.caps.Usage);
                }
                if (tablet.report_desc_valid) {
                    wprintf(L"    reportDescLen=%lu\n", tablet.report_desc_len);
                }
                if (tablet.hid_report_desc_valid) {
                    wprintf(L"    hidReportDescLen=%lu\n", tablet.hid_report_desc_len);
                }
            } else {
                wprintf(L"  tablet: not found\n");
            }
        }

        for (i = 0; i < failure_count; i++) {
            const SELFTEST_FAILURE *f = &failures[i];
            wprintf(L"  FAIL %ls.%ls", f->device, f->field);
            if (f->message != NULL) {
                wprintf(L": %ls", f->message);
            }
            if (f->have_expected || f->have_actual) {
                wprintf(L" (");
                if (f->have_expected) {
                    wprintf(L"expected=%lu", f->expected);
                }
                if (f->have_actual) {
                    if (f->have_expected) {
                        wprintf(L", ");
                    }
                    wprintf(L"actual=%lu", f->actual);
                }
                wprintf(L")");
            }
            wprintf(L"\n");
        }
    }

    free_selftest_device_info(&kbd);
    free_selftest_device_info(&mouse);
    free_selftest_device_info(&tablet);

    return pass ? 0 : 1;
}

static int enumerate_hid_devices(const OPTIONS *opt, SELECTED_DEVICE *out)
{
    GUID hid_guid;
    HDEVINFO devinfo;
    SP_DEVICE_INTERFACE_DATA iface;
    DWORD iface_index;
    int have_hard_filters;
    int have_usage_filter;
    int usage_only;
    SELECTED_DEVICE fallback_any;
    SELECTED_DEVICE fallback_virtio;

    ZeroMemory(out, sizeof(*out));
    out->handle = INVALID_HANDLE_VALUE;
    ZeroMemory(&fallback_any, sizeof(fallback_any));
    fallback_any.handle = INVALID_HANDLE_VALUE;
    ZeroMemory(&fallback_virtio, sizeof(fallback_virtio));
    fallback_virtio.handle = INVALID_HANDLE_VALUE;

    HidD_GetHidGuid(&hid_guid);

    devinfo = SetupDiGetClassDevsW(&hid_guid, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devinfo == INVALID_HANDLE_VALUE) {
        if (opt != NULL && opt->quiet) {
            print_last_error_file_w(stderr, L"SetupDiGetClassDevs");
        } else {
            print_last_error_w(L"SetupDiGetClassDevs");
        }
        return 0;
    }

    iface_index = 0;
    have_hard_filters = opt->have_index || opt->have_vid || opt->have_pid;
    have_usage_filter = opt->want_keyboard || opt->want_mouse || opt->want_consumer || opt->want_tablet;
    usage_only = have_usage_filter && !have_hard_filters;
    for (;;) {
        DWORD required = 0;
        PSP_DEVICE_INTERFACE_DETAIL_DATA_W detail = NULL;
        HANDLE handle = INVALID_HANDLE_VALUE;
        DWORD desired_access = 0;
        HIDD_ATTRIBUTES attr;
        HIDP_CAPS caps;
        int caps_valid = 0;
        int attr_valid = 0;
        DWORD report_desc_len = 0;
        int report_desc_valid = 0;
        DWORD hid_report_desc_len = 0;
        int hid_report_desc_valid = 0;
        DWORD virtio_expected_desc_len = 0;
        int virtio_expected_desc_valid = 0;
        int match = 0;
        int is_virtio = 0;
        int is_keyboard = 0;
        int is_mouse = 0;
        int is_consumer = 0;
        int is_tablet = 0;

        ZeroMemory(&iface, sizeof(iface));
        iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(devinfo, NULL, &hid_guid, iface_index, &iface)) {
            DWORD err = GetLastError();
            if (err != ERROR_NO_MORE_ITEMS) {
                if (opt != NULL && opt->quiet) {
                    print_win32_error_file_w(stderr, L"SetupDiEnumDeviceInterfaces", err);
                } else {
                    print_win32_error_w(L"SetupDiEnumDeviceInterfaces", err);
                }
            }
            break;
        }

        SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, NULL, 0, &required, NULL);
        if (required == 0) {
            if (opt != NULL && opt->quiet) {
                fwprintf(stderr, L"[%lu] SetupDiGetDeviceInterfaceDetail: required size=0\n", iface_index);
            } else {
                wprintf(L"[%lu] SetupDiGetDeviceInterfaceDetail: required size=0\n", iface_index);
            }
            iface_index++;
            continue;
        }

        detail = (PSP_DEVICE_INTERFACE_DETAIL_DATA_W)malloc(required);
        if (detail == NULL) {
            if (opt != NULL && opt->quiet) {
                fwprintf(stderr, L"Out of memory\n");
            } else {
                wprintf(L"Out of memory\n");
            }
            SetupDiDestroyDeviceInfoList(devinfo);
            return 0;
        }

        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, required, NULL, NULL)) {
            if (opt != NULL && opt->quiet) {
                fwprintf(stderr, L"[%lu] SetupDiGetDeviceInterfaceDetail failed\n", iface_index);
                print_last_error_file_w(stderr, L"SetupDiGetDeviceInterfaceDetail");
            } else {
                wprintf(L"[%lu] SetupDiGetDeviceInterfaceDetail failed\n", iface_index);
                print_last_error_w(L"SetupDiGetDeviceInterfaceDetail");
            }
            free(detail);
            iface_index++;
            continue;
        }

        handle = open_hid_path(detail->DevicePath, &desired_access);
        if (handle == INVALID_HANDLE_VALUE) {
            if (!opt->quiet) {
                wprintf(L"[%lu] %ls\n", iface_index, detail->DevicePath);
                print_last_error_w(L"      CreateFile");
            }
            free(detail);
            iface_index++;
            continue;
        }

        ZeroMemory(&attr, sizeof(attr));
        attr.Size = sizeof(attr);
        if (HidD_GetAttributes(handle, &attr)) {
            attr_valid = 1;
            is_virtio = is_virtio_input_device(&attr);
        }

        ZeroMemory(&caps, sizeof(caps));
        caps_valid = query_hid_caps(handle, &caps);
        report_desc_valid = query_report_descriptor_length(handle, &report_desc_len);
        hid_report_desc_valid = query_hid_descriptor_report_length(handle, &hid_report_desc_len);

        if (!opt->quiet) {
            wprintf(L"[%lu] %ls\n", iface_index, detail->DevicePath);
            if (attr_valid) {
                wprintf(L"      VID:PID %04X:%04X (ver %04X)\n", attr.VendorID, attr.ProductID,
                        attr.VersionNumber);
            } else {
                wprintf(L"      HidD_GetAttributes failed\n");
            }

            if (caps_valid) {
                wprintf(L"      UsagePage:Usage %04X:%04X\n", caps.UsagePage, caps.Usage);
                wprintf(L"      Report bytes (in/out/feat): %u / %u / %u\n", caps.InputReportByteLength,
                        caps.OutputReportByteLength, caps.FeatureReportByteLength);
            } else {
                wprintf(L"      HidD_GetPreparsedData/HidP_GetCaps failed\n");
            }
        }

        is_keyboard = caps_valid && caps.UsagePage == 0x01 && caps.Usage == 0x06;
        is_mouse = caps_valid && caps.UsagePage == 0x01 && caps.Usage == 0x02;
        is_consumer = caps_valid && caps.UsagePage == 0x0C && caps.Usage == 0x01;
        is_tablet = 0;
        if (is_virtio && attr_valid && attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
            is_tablet = 1;
        } else if (is_virtio) {
            // Heuristic for virtio-input tablet (absolute pointer): currently shares
            // the mouse top-level usage, so distinguish by report descriptor length.
            if (report_desc_valid && report_desc_len == VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
                is_tablet = 1;
            } else if (hid_report_desc_valid && hid_report_desc_len == VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN) {
                is_tablet = 1;
            }
        }
        if (is_tablet) {
            // Tablet uses the same top-level usage as Mouse (0x01:0x02). Keep it distinct so --mouse/selftest
            // don't accidentally select the tablet.
            is_mouse = 0;
        }

        if (is_keyboard) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (is_mouse) {
            virtio_expected_desc_len =
                is_tablet ? VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN : VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (is_tablet) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (attr_valid && attr.ProductID == VIRTIO_INPUT_PID_KEYBOARD) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (attr_valid && attr.ProductID == VIRTIO_INPUT_PID_MOUSE) {
            virtio_expected_desc_len =
                is_tablet ? VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN : VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (attr_valid && attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_TABLET_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        }

        if (!opt->quiet) {
            if (is_virtio) {
                if (is_keyboard) {
                    wprintf(L"      Detected: virtio-input keyboard\n");
                } else if (is_consumer) {
                    wprintf(L"      Detected: virtio-input consumer control\n");
                } else if (is_mouse && is_tablet) {
                    wprintf(L"      Detected: virtio-input tablet\n");
                } else if (is_mouse) {
                    wprintf(L"      Detected: virtio-input mouse\n");
                } else if (is_tablet) {
                    wprintf(L"      Detected: virtio-input tablet\n");
                } else {
                    wprintf(L"      Detected: virtio-input\n");
                }
            }

            if (report_desc_valid) {
                wprintf(L"      Report descriptor length: %lu bytes\n", report_desc_len);
            } else {
                wprintf(L"      IOCTL_HID_GET_REPORT_DESCRIPTOR failed\n");
            }
            if (hid_report_desc_valid) {
                wprintf(L"      HID descriptor report length: %lu bytes\n", hid_report_desc_len);
            } else {
                wprintf(L"      IOCTL_HID_GET_DEVICE_DESCRIPTOR failed\n");
            }
            if (report_desc_valid && hid_report_desc_valid && report_desc_len != hid_report_desc_len) {
                wprintf(L"      [WARN] report descriptor length mismatch (IOCTL=%lu, HID=%lu)\n",
                        report_desc_len, hid_report_desc_len);
            }
        }

        if (!opt->quiet) {
            if (is_virtio) {
                if (virtio_expected_desc_valid) {
                    if (report_desc_valid && report_desc_len != virtio_expected_desc_len) {
                        wprintf(L"      [WARN] unexpected virtio-input report descriptor length (expected %u)\n",
                                (unsigned)virtio_expected_desc_len);
                    }
                    if (hid_report_desc_valid && hid_report_desc_len != virtio_expected_desc_len) {
                        wprintf(L"      [WARN] unexpected virtio-input HID descriptor report length (expected %u)\n",
                                (unsigned)virtio_expected_desc_len);
                    }
                }

                if (caps_valid && is_keyboard) {
                    if (caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN) {
                        wprintf(L"      [WARN] unexpected virtio-input keyboard input report length (expected %u)\n",
                                (unsigned)VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN);
                    }
                    if (caps.OutputReportByteLength != VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN) {
                        wprintf(L"      [WARN] unexpected virtio-input keyboard output report length (expected %u)\n",
                                (unsigned)VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN);
                    }
                } else if (caps_valid && is_mouse && is_tablet) {
                    if (caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN) {
                        wprintf(L"      [WARN] unexpected virtio-input tablet input report length (expected %u)\n",
                                (unsigned)VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN);
                    }
                } else if (caps_valid && is_mouse) {
                    if (caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN) {
                        wprintf(L"      [WARN] unexpected virtio-input mouse input report length (expected %u)\n",
                                (unsigned)VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN);
                    }
                } else if (caps_valid && is_tablet) {
                    if (caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN) {
                        wprintf(L"      [WARN] unexpected virtio-input tablet input report length (expected %u)\n",
                                (unsigned)VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN);
                    }
                }
            }

            if (desired_access & GENERIC_WRITE) {
                wprintf(L"      Access: read/write\n");
            } else {
                wprintf(L"      Access: read-only\n");
            }

            print_device_strings(handle);
        }

        // Match selection filters. If the user is selecting by index only, we can match even if
        // HidD_GetAttributes failed.
        match = 1;
        if (opt->have_index && opt->index != iface_index) {
            match = 0;
        }
        if (match && (opt->have_vid || opt->have_pid)) {
            if (!attr_valid) {
                match = 0;
            } else if (!device_matches_opts(opt, iface_index, &attr)) {
                match = 0;
            }
        }
        if (match && opt->want_keyboard) {
            match = is_keyboard;
        }
        if (match && opt->want_mouse) {
            match = is_mouse;
        }
        if (match && opt->want_consumer) {
            match = is_consumer;
        }
        if (match && opt->want_tablet) {
            match = is_tablet;
        }

        if (opt->list_only) {
            CloseHandle(handle);
            free(detail);
            iface_index++;
            continue;
        }

        // Selection rules:
        // - With hard filters (--index/--vid/--pid): pick the first match.
        // - With only usage filters (--keyboard/--mouse/--tablet): prefer a matching virtio interface,
        //   otherwise fall back to the first matching interface of that usage.
        // - With no filters: prefer virtio keyboard, then first virtio, then first HID interface.
        if (have_hard_filters) {
            if (match) {
                out->handle = handle;
                out->desired_access = desired_access;
                out->path = wcsdup_heap(detail->DevicePath);
                out->attr = attr;
                out->attr_valid = attr_valid;
                out->caps = caps;
                out->caps_valid = caps_valid;
                out->report_desc_len = report_desc_len;
                out->report_desc_valid = report_desc_valid;
                out->hid_report_desc_len = hid_report_desc_len;
                out->hid_report_desc_valid = hid_report_desc_valid;
                free(detail);
                break;
            }
            CloseHandle(handle);
        } else if (usage_only) {
            if (!match) {
                CloseHandle(handle);
                free(detail);
                iface_index++;
                continue;
            }

            if (is_virtio) {
                out->handle = handle;
                out->desired_access = desired_access;
                out->path = wcsdup_heap(detail->DevicePath);
                out->attr = attr;
                out->attr_valid = attr_valid;
                out->caps = caps;
                out->caps_valid = caps_valid;
                out->report_desc_len = report_desc_len;
                out->report_desc_valid = report_desc_valid;
                out->hid_report_desc_len = hid_report_desc_len;
                out->hid_report_desc_valid = hid_report_desc_valid;

                free_selected_device(&fallback_any);

                free(detail);
                break;
            }

            if (fallback_any.handle == INVALID_HANDLE_VALUE) {
                fallback_any.handle = handle;
                fallback_any.desired_access = desired_access;
                fallback_any.path = wcsdup_heap(detail->DevicePath);
                fallback_any.attr = attr;
                fallback_any.attr_valid = attr_valid;
                fallback_any.caps = caps;
                fallback_any.caps_valid = caps_valid;
                fallback_any.report_desc_len = report_desc_len;
                fallback_any.report_desc_valid = report_desc_valid;
                fallback_any.hid_report_desc_len = hid_report_desc_len;
                fallback_any.hid_report_desc_valid = hid_report_desc_valid;
            } else {
                CloseHandle(handle);
            }
        } else if (is_virtio && is_keyboard) {
            out->handle = handle;
            out->desired_access = desired_access;
            out->path = wcsdup_heap(detail->DevicePath);
            out->attr = attr;
            out->attr_valid = attr_valid;
            out->caps = caps;
            out->caps_valid = caps_valid;
            out->report_desc_len = report_desc_len;
            out->report_desc_valid = report_desc_valid;
            out->hid_report_desc_len = hid_report_desc_len;
            out->hid_report_desc_valid = hid_report_desc_valid;

            free_selected_device(&fallback_any);
            free_selected_device(&fallback_virtio);

            free(detail);
            break;
        } else if (is_virtio && fallback_virtio.handle == INVALID_HANDLE_VALUE) {
            fallback_virtio.handle = handle;
            fallback_virtio.desired_access = desired_access;
            fallback_virtio.path = wcsdup_heap(detail->DevicePath);
            fallback_virtio.attr = attr;
            fallback_virtio.attr_valid = attr_valid;
            fallback_virtio.caps = caps;
            fallback_virtio.caps_valid = caps_valid;
            fallback_virtio.report_desc_len = report_desc_len;
            fallback_virtio.report_desc_valid = report_desc_valid;
            fallback_virtio.hid_report_desc_len = hid_report_desc_len;
            fallback_virtio.hid_report_desc_valid = hid_report_desc_valid;
        } else if (fallback_any.handle == INVALID_HANDLE_VALUE) {
            fallback_any.handle = handle;
            fallback_any.desired_access = desired_access;
            fallback_any.path = wcsdup_heap(detail->DevicePath);
            fallback_any.attr = attr;
            fallback_any.attr_valid = attr_valid;
            fallback_any.caps = caps;
            fallback_any.caps_valid = caps_valid;
            fallback_any.report_desc_len = report_desc_len;
            fallback_any.report_desc_valid = report_desc_valid;
            fallback_any.hid_report_desc_len = hid_report_desc_len;
            fallback_any.hid_report_desc_valid = hid_report_desc_valid;
        } else {
            CloseHandle(handle);
        }

        free(detail);
        iface_index++;
    }

    SetupDiDestroyDeviceInfoList(devinfo);

    if (opt->list_only) {
        return 1;
    }

    if (out->handle == INVALID_HANDLE_VALUE) {
        if (!usage_only && fallback_virtio.handle != INVALID_HANDLE_VALUE) {
            *out = fallback_virtio;
            ZeroMemory(&fallback_virtio, sizeof(fallback_virtio));
            fallback_virtio.handle = INVALID_HANDLE_VALUE;
        } else if (fallback_any.handle != INVALID_HANDLE_VALUE) {
            *out = fallback_any;
            ZeroMemory(&fallback_any, sizeof(fallback_any));
            fallback_any.handle = INVALID_HANDLE_VALUE;
        }
    }

    if (fallback_any.handle != INVALID_HANDLE_VALUE) {
        free_selected_device(&fallback_any);
    }
    if (fallback_virtio.handle != INVALID_HANDLE_VALUE) {
        free_selected_device(&fallback_virtio);
    }

    return out->handle != INVALID_HANDLE_VALUE;
}

static int send_keyboard_led_report(const SELECTED_DEVICE *dev, BYTE led_mask)
{
    BYTE *out_report;
    DWORD out_len;
    DWORD written = 0;
    BOOL ok;

    if (dev->handle == INVALID_HANDLE_VALUE) {
        return 0;
    }
    if (!(dev->desired_access & GENERIC_WRITE)) {
        wprintf(L"LED write requested, but device was opened read-only.\n");
        return 0;
    }
    if (!dev->caps_valid) {
        wprintf(L"LED write requested, but HID caps are not available.\n");
        return 0;
    }
    if (!(dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06)) {
        wprintf(L"LED write requested, but selected interface is not a keyboard collection.\n");
        return 0;
    }

    out_len = dev->caps.OutputReportByteLength;
    if (out_len == 0) {
        // Some miniports don't report an output report length (or report 0). For virtio-input we
        // still want to try the common [ReportID][LEDs] layout.
        out_len = 2;
    }

    out_report = (BYTE *)calloc(out_len, 1);
    if (out_report == NULL) {
        wprintf(L"Out of memory\n");
        return 0;
    }

    if (out_len == 1) {
        // No report ID byte.
        out_report[0] = led_mask;
    } else {
        out_report[0] = 1; // ReportID=1 (keyboard LED output report for virtio-input).
        out_report[1] = led_mask;
    }

    wprintf(L"Writing keyboard LED output report: ");
    dump_hex(out_report, out_len);
    wprintf(L"\n");

    ok = WriteFile(dev->handle, out_report, out_len, &written, NULL);
    if (!ok) {
        print_last_error_w(L"WriteFile(IOCTL_HID_WRITE_REPORT)");
        free(out_report);
        return 0;
    }
    wprintf(L"Wrote %lu bytes\n", written);
    free(out_report);
    return 1;
}

static int send_keyboard_led_report_hidd(const SELECTED_DEVICE *dev, BYTE led_mask)
{
    BYTE *out_report;
    DWORD out_len;
    BOOL ok;

    if (dev->handle == INVALID_HANDLE_VALUE) {
        return 0;
    }
    if (!(dev->desired_access & GENERIC_WRITE)) {
        wprintf(L"LED write requested, but device was opened read-only.\n");
        return 0;
    }
    if (!dev->caps_valid) {
        wprintf(L"LED write requested, but HID caps are not available.\n");
        return 0;
    }
    if (!(dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06)) {
        wprintf(L"LED write requested, but selected interface is not a keyboard collection.\n");
        return 0;
    }

    out_len = dev->caps.OutputReportByteLength;
    if (out_len == 0) {
        // Some miniports don't report an output report length (or report 0). For virtio-input we
        // still want to try the common [ReportID][LEDs] layout.
        out_len = 2;
    }

    out_report = (BYTE *)calloc(out_len, 1);
    if (out_report == NULL) {
        wprintf(L"Out of memory\n");
        return 0;
    }

    if (out_len == 1) {
        // No report ID byte.
        out_report[0] = led_mask;
    } else {
        out_report[0] = 1; // ReportID=1 (keyboard LED output report for virtio-input).
        out_report[1] = led_mask;
    }

    wprintf(L"HidD_SetOutputReport keyboard LEDs: ");
    dump_hex(out_report, out_len);
    wprintf(L"\n");

    ok = HidD_SetOutputReport(dev->handle, out_report, out_len);
    if (!ok) {
        print_last_error_w(L"HidD_SetOutputReport");
        free(out_report);
        return 0;
    }

    wprintf(L"HidD_SetOutputReport succeeded\n");
    free(out_report);
    return 1;
}

static int send_keyboard_led_report_ioctl_set_output(const SELECTED_DEVICE *dev, BYTE led_mask)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    BYTE report[2];
    ULONG_PTR inbuf[16];
    HID_XFER_PACKET_MIN *pkt;
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        return 0;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"LED write requested, but device was opened read-only.\n");
        return 0;
    }

    if (!dev->caps_valid || !(dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06)) {
        wprintf(L"LED write requested, but selected interface is not a keyboard collection.\n");
        return 0;
    }

    report[0] = 1; // ReportID=1 (keyboard)
    report[1] = led_mask;

    ZeroMemory(inbuf, sizeof(inbuf));
    pkt = (HID_XFER_PACKET_MIN *)inbuf;
    pkt->reportId = 1;
    pkt->reportBuffer = report;
    pkt->reportBufferLen = (ULONG)sizeof(report);

    wprintf(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT) keyboard LEDs: ");
    dump_hex(report, (DWORD)sizeof(report));
    wprintf(L"\n");

    ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, NULL);
    if (!ok) {
        print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)");
        return 0;
    }

    wprintf(L"IOCTL_HID_SET_OUTPUT_REPORT succeeded\n");
    return 1;
}

static void cycle_keyboard_leds(const SELECTED_DEVICE *dev)
{
    // Short sequence to guarantee visible state changes even if the current LED
    // state is unknown.
    static const BYTE seq[] = {
        0x00,
        0x01, // NumLock
        0x00,
        0x02, // CapsLock
        0x00,
        0x04, // ScrollLock
        0x00,
        0x08, // Compose (optional HID boot keyboard LED bit)
        0x00,
        0x10, // Kana (optional HID boot keyboard LED bit)
        0x00,
        0x1F, // All 5 defined HID boot keyboard LED bits
        0x00,
    };
    int i;

    if (dev->handle == INVALID_HANDLE_VALUE) {
        return;
    }
    if (!(dev->desired_access & GENERIC_WRITE)) {
        wprintf(L"LED cycle requested, but device was opened read-only.\n");
        return;
    }
    if (!dev->caps_valid || !(dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06)) {
        wprintf(L"LED cycle requested, but selected interface is not a keyboard collection.\n");
        return;
    }

    for (i = 0; i < (int)(sizeof(seq) / sizeof(seq[0])); i++) {
        (VOID)send_keyboard_led_report(dev, seq[i]);
        Sleep(250);
    }
}

static int spam_keyboard_leds(const SELECTED_DEVICE *dev, BYTE on_mask, DWORD count, int via_hidd, int via_ioctl_set_output)
{
    DWORD i;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        return 0;
    }
    if (!(dev->desired_access & GENERIC_WRITE)) {
        wprintf(L"LED spam requested, but device was opened read-only.\n");
        return 0;
    }
    if (!dev->caps_valid || !(dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06)) {
        wprintf(L"LED spam requested, but selected interface is not a keyboard collection.\n");
        return 0;
    }

    if (count == 0) {
        wprintf(L"LED spam count is 0; nothing to do.\n");
        return 1;
    }

    if (on_mask == 0) {
        // A nonzero mask makes it easier to see traffic in logs/counters even if the guest keyboard LEDs are not visible.
        on_mask = 0x1F;
    }

    if (via_ioctl_set_output) {
        // Use the explicit IOCTL_HID_SET_OUTPUT_REPORT path (matches send_keyboard_led_report_ioctl_set_output).
        typedef struct HID_XFER_PACKET_MIN {
            PUCHAR reportBuffer;
            ULONG reportBufferLen;
            UCHAR reportId;
        } HID_XFER_PACKET_MIN;

        BYTE report[2];
        ULONG_PTR inbuf[16];
        HID_XFER_PACKET_MIN *pkt;
        DWORD bytes;

        ZeroMemory(inbuf, sizeof(inbuf));
        pkt = (HID_XFER_PACKET_MIN *)inbuf;
        pkt->reportId = 1;
        pkt->reportBuffer = report;
        pkt->reportBufferLen = (ULONG)sizeof(report);

        report[0] = 1; // ReportID=1 (keyboard)

        wprintf(L"Spamming keyboard LEDs via IOCTL_HID_SET_OUTPUT_REPORT: count=%lu onMask=0x%02X\n", count, on_mask);

        for (i = 0; i < count; i++) {
            const BYTE mask = (BYTE)((i & 1u) ? on_mask : 0);
            BOOL ok;

            report[1] = mask;
            bytes = 0;
            ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, NULL);
            if (!ok) {
                print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)");
                return 0;
            }
        }

        wprintf(L"LED spam complete\n");
        return 1;
    }

    if (via_hidd) {
        DWORD out_len;
        BYTE *out_report;

        out_len = dev->caps.OutputReportByteLength;
        if (out_len == 0) {
            out_len = 2;
        }

        out_report = (BYTE *)calloc(out_len, 1);
        if (out_report == NULL) {
            wprintf(L"Out of memory\n");
            return 0;
        }

        if (out_len > 1) {
            out_report[0] = 1;
        }

        wprintf(L"Spamming keyboard LEDs via HidD_SetOutputReport: count=%lu onMask=0x%02X\n", count, on_mask);

        for (i = 0; i < count; i++) {
            const BYTE mask = (BYTE)((i & 1u) ? on_mask : 0);
            BOOL ok;

            if (out_len == 1) {
                out_report[0] = mask;
            } else {
                out_report[1] = mask;
            }

            ok = HidD_SetOutputReport(dev->handle, out_report, out_len);
            if (!ok) {
                print_last_error_w(L"HidD_SetOutputReport");
                free(out_report);
                return 0;
            }
        }

        free(out_report);
        wprintf(L"LED spam complete\n");
        return 1;
    }

    {
        DWORD out_len;
        BYTE *out_report;

        out_len = dev->caps.OutputReportByteLength;
        if (out_len == 0) {
            out_len = 2;
        }

        out_report = (BYTE *)calloc(out_len, 1);
        if (out_report == NULL) {
            wprintf(L"Out of memory\n");
            return 0;
        }

        if (out_len > 1) {
            out_report[0] = 1;
        }

        wprintf(L"Spamming keyboard LEDs via WriteFile(IOCTL_HID_WRITE_REPORT): count=%lu onMask=0x%02X\n", count, on_mask);

        for (i = 0; i < count; i++) {
            const BYTE mask = (BYTE)((i & 1u) ? on_mask : 0);
            DWORD written;
            BOOL ok;

            if (out_len == 1) {
                out_report[0] = mask;
            } else {
                out_report[1] = mask;
            }

            written = 0;
            ok = WriteFile(dev->handle, out_report, out_len, &written, NULL);
            if (!ok) {
                print_last_error_w(L"WriteFile(IOCTL_HID_WRITE_REPORT)");
                free(out_report);
                return 0;
            }
        }

        free(out_report);
        wprintf(L"LED spam complete\n");
        return 1;
    }
}

static DWORD qpc_ticks_to_timeout_ms(LONGLONG ticks, LONGLONG freq)
{
    ULONGLONG ms;

    if (ticks <= 0) {
        return 0;
    }

    // Convert to milliseconds, rounding up so we don't exit early when using a
    // duration-based timeout.
    ms = (ULONGLONG)ticks * 1000ULL;
    ms = (ms + (ULONGLONG)freq - 1ULL) / (ULONGLONG)freq;

    // WaitFor* uses 0xFFFFFFFF (INFINITE) as a sentinel.
    if (ms >= 0xFFFFFFFFULL) {
        return 0xFFFFFFFEUL;
    }

    return (DWORD)ms;
}

static void read_reports_loop(const SELECTED_DEVICE *dev, const OPTIONS *opt)
{
    BYTE *buf;
    DWORD buf_len;
    DWORD n;
    DWORD seq = 0;
    int is_virtio = dev->attr_valid && is_virtio_input_device(&dev->attr);
    HANDLE read_handle = INVALID_HANDLE_VALUE;
    HANDLE read_event = NULL;
    int have_duration = 0;
    int have_count = 0;
    DWORD duration_secs = 0;
    DWORD count_limit = 0;
    ULONGLONG reports_read = 0;
    ULONGLONG errors = 0;
    LARGE_INTEGER qpc_freq;
    LARGE_INTEGER qpc_start;
    LARGE_INTEGER qpc_now;
    LONGLONG deadline_ticks = 0;
    DWORD wait_rc;
    DWORD wait_timeout_ms;
    OVERLAPPED ov;
    HANDLE wait_handles[2];
    BOOL ok;

    if (opt != NULL) {
        if (opt->have_duration) {
            have_duration = 1;
            duration_secs = opt->duration_secs;
        }
        if (opt->have_count) {
            have_count = 1;
            count_limit = opt->count;
        }
    }

    QueryPerformanceFrequency(&qpc_freq);
    QueryPerformanceCounter(&qpc_start);

    if (!dev->caps_valid) {
        wprintf(L"Cannot read reports: HID caps not available.\n");
        errors++;
        goto done;
    }

    if (dev->path == NULL) {
        wprintf(L"Cannot read reports: selected device path is unavailable.\n");
        errors++;
        goto done;
    }

    // Open a separate overlapped handle for the report read loop so the rest of
    // the tool can keep using the original handle (opened without
    // FILE_FLAG_OVERLAPPED) for DeviceIoControl/WriteFile/etc.
    read_handle = CreateFileW(dev->path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING,
                              FILE_FLAG_OVERLAPPED, NULL);
    if (read_handle == INVALID_HANDLE_VALUE) {
        print_last_error_w(L"CreateFile(overlapped read handle)");
        errors++;
        goto done;
    }

    read_event = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (read_event == NULL) {
        print_last_error_w(L"CreateEvent(read_event)");
        errors++;
        goto done;
    }

    g_stop_event = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (g_stop_event == NULL) {
        print_last_error_w(L"CreateEvent(stop_event)");
        errors++;
        goto done;
    }
    InterlockedExchange(&g_stop_requested, 0);
    (VOID)SetConsoleCtrlHandler(console_ctrl_handler, TRUE);

    if (have_duration) {
        deadline_ticks = qpc_start.QuadPart + (LONGLONG)duration_secs * qpc_freq.QuadPart;
    }

    buf_len = dev->caps.InputReportByteLength;
    if (buf_len == 0) {
        buf_len = 64;
    }

    buf = (BYTE *)malloc(buf_len);
    if (buf == NULL) {
        wprintf(L"Out of memory\n");
        errors++;
        goto done;
    }

    wprintf(L"\nReading input reports (%lu bytes)...\n", buf_len);
    if (have_duration) {
        wprintf(L"Auto-exit: --duration %lu\n", duration_secs);
    }
    if (have_count) {
        wprintf(L"Auto-exit: --count %lu\n", count_limit);
    }

    wait_handles[0] = g_stop_event;
    wait_handles[1] = read_event;
    for (;;) {
        if (InterlockedCompareExchange(&g_stop_requested, 0, 0) != 0) {
            break;
        }
        if (have_count && reports_read >= (ULONGLONG)count_limit) {
            break;
        }
        if (have_duration) {
            QueryPerformanceCounter(&qpc_now);
            if (qpc_now.QuadPart >= deadline_ticks) {
                break;
            }
        }

        ZeroMemory(buf, buf_len);
        n = 0;
        ZeroMemory(&ov, sizeof(ov));
        ov.hEvent = read_event;
        ResetEvent(read_event);

        ok = ReadFile(read_handle, buf, buf_len, &n, &ov);
        if (!ok) {
            DWORD err = GetLastError();
            if (err != ERROR_IO_PENDING) {
                print_win32_error_w(L"ReadFile(IOCTL_HID_READ_REPORT)", err);
                errors++;
                break;
            }

            // Wait for either:
            // - Ctrl+C (stop event), or
            // - the read to complete (read event), or
            // - the duration timer to expire (timeout).
            if (have_duration) {
                QueryPerformanceCounter(&qpc_now);
                wait_timeout_ms = qpc_ticks_to_timeout_ms(deadline_ticks - qpc_now.QuadPart, qpc_freq.QuadPart);
            } else {
                wait_timeout_ms = INFINITE;
            }

            wait_rc = WaitForMultipleObjects(2, wait_handles, FALSE, wait_timeout_ms);
            if (wait_rc == WAIT_OBJECT_0) {
                // Ctrl+C requested.
                CancelIo(read_handle);
                (VOID)GetOverlappedResult(read_handle, &ov, &n, TRUE);
                break;
            }
            if (wait_rc == WAIT_TIMEOUT) {
                // Duration timer expired.
                CancelIo(read_handle);
                (VOID)GetOverlappedResult(read_handle, &ov, &n, TRUE);
                break;
            }
            if (wait_rc != WAIT_OBJECT_0 + 1) {
                print_last_error_w(L"WaitForMultipleObjects");
                errors++;
                CancelIo(read_handle);
                (VOID)GetOverlappedResult(read_handle, &ov, &n, TRUE);
                break;
            }

            ok = GetOverlappedResult(read_handle, &ov, &n, FALSE);
            if (!ok) {
                DWORD err2 = GetLastError();
                if (err2 == ERROR_OPERATION_ABORTED) {
                    // Can happen due to CancelIo on Ctrl+C / duration expiry.
                    break;
                }
                print_win32_error_w(L"GetOverlappedResult(ReadFile)", err2);
                errors++;
                break;
            }
        }

        wprintf(L"[%lu] %lu bytes: ", seq, n);
        dump_hex(buf, n);
        wprintf(L"\n");

        // Best-effort decode:
        // - For virtio-input, use ReportID (byte 0) since report IDs are stable.
        // - Otherwise fall back to top-level usage heuristics.
        if (is_virtio && n > 0) {
            // virtio-input reports are expected to include a Report ID byte, but some consumer-only HID devices
            // (and some non-Aero/QEMU variants) omit Report IDs entirely. If the byte stream doesn't match a
            // known virtio-input report ID+length pair, fall back to usage-based decoding.
            if (buf[0] == 1 && n == VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN) {
                dump_keyboard_report(buf, n);
            } else if (buf[0] == 2 && n == VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN) {
                dump_mouse_report(buf, n, 1);
            } else if (buf[0] == 3 &&
                       (n == VIRTIO_INPUT_EXPECTED_CONSUMER_INPUT_LEN || n == VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN)) {
                dump_consumer_report(buf, n, 1);
            } else if (buf[0] == 4 && n == VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN) {
                dump_tablet_report(buf, n, 1);
            } else if (dev->caps.UsagePage == 0x0C && dev->caps.Usage == 0x01) {
                // Consumer Control (media keys). If the report begins with the expected virtio-input Report ID,
                // decode it as such; otherwise treat the first byte as the data payload.
                dump_consumer_report(buf, n, (n >= 2 && buf[0] == 3) ? 1 : 0);
            } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06) {
                dump_keyboard_report(buf, n);
            } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x02) {
                dump_mouse_report(buf, n, 0);
            }
        } else {
            if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06) {
                dump_keyboard_report(buf, n);
            } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x02) {
                dump_mouse_report(buf, n, 0);
            } else if (dev->caps.UsagePage == 0x0C && dev->caps.Usage == 0x01) {
                dump_consumer_report(buf, n, 0);
            }
        }

        seq++;
        reports_read++;
    }

    free(buf);

done:
    QueryPerformanceCounter(&qpc_now);
    wprintf(L"\nSummary:\n");
    wprintf(L"  Reports read: %I64u\n", reports_read);
    wprintf(L"  Errors:       %I64u\n", errors);
    wprintf(L"  Elapsed:      %.3f s\n",
            (double)(qpc_now.QuadPart - qpc_start.QuadPart) / (double)qpc_freq.QuadPart);

    (VOID)SetConsoleCtrlHandler(console_ctrl_handler, FALSE);
    if (read_event != NULL) {
        CloseHandle(read_event);
    }
    if (read_handle != INVALID_HANDLE_VALUE) {
        CloseHandle(read_handle);
    }
    if (g_stop_event != NULL) {
        CloseHandle(g_stop_event);
        g_stop_event = NULL;
    }
}

static int ioctl_query_counters_short(const SELECTED_DEVICE *dev)
{
    VIOINPUT_COUNTERS_V1_MIN out;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    ZeroMemory(&out, sizeof(out));

    wprintf(L"\nIssuing IOCTL_VIOINPUT_QUERY_COUNTERS with short output buffer (%u bytes)...\n",
            (unsigned)sizeof(out));
    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_QUERY_COUNTERS, NULL, 0, &out, (DWORD)sizeof(out), &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    err = GetLastError();
    if (err != ERROR_INSUFFICIENT_BUFFER) {
        print_win32_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_COUNTERS short buffer)", err);
        return 1;
    }

    if (out.Size < sizeof(out) || out.Version == 0) {
        wprintf(L"Expected Size/Version to be returned even on ERROR_INSUFFICIENT_BUFFER; got Size=%lu Version=%lu\n",
                out.Size, out.Version);
        return 1;
    }

    wprintf(L"Got counters header despite short buffer: Size=%lu Version=%lu (bytesReturned=%lu)\n", out.Size,
            out.Version, bytes);
    return 0;
}

static int ioctl_query_state_short(const SELECTED_DEVICE *dev)
{
    VIOINPUT_STATE_V1_MIN out;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    ZeroMemory(&out, sizeof(out));

    wprintf(L"\nIssuing IOCTL_VIOINPUT_QUERY_STATE with short output buffer (%u bytes)...\n", (unsigned)sizeof(out));
    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_QUERY_STATE, NULL, 0, &out, (DWORD)sizeof(out), &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    err = GetLastError();
    if (err != ERROR_INSUFFICIENT_BUFFER) {
        print_win32_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_STATE short buffer)", err);
        return 1;
    }

    if (out.Size < sizeof(out) || out.Version == 0) {
        wprintf(L"Expected Size/Version to be returned even on ERROR_INSUFFICIENT_BUFFER; got Size=%lu Version=%lu\n",
                out.Size, out.Version);
        return 1;
    }

    wprintf(L"Got state header despite short buffer: Size=%lu Version=%lu (bytesReturned=%lu)\n", out.Size, out.Version,
            bytes);
    return 0;
}

static int ioctl_query_interrupt_info_short(const SELECTED_DEVICE *dev)
{
    VIOINPUT_INTERRUPT_INFO_V1_MIN out;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    ZeroMemory(&out, sizeof(out));

    wprintf(L"\nIssuing IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO with short output buffer (%u bytes)...\n",
            (unsigned)sizeof(out));
    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO, NULL, 0, &out, (DWORD)sizeof(out), &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    err = GetLastError();
    if (err != ERROR_INSUFFICIENT_BUFFER) {
        print_win32_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO short buffer)", err);
        return 1;
    }

    if (out.Size < sizeof(out) || out.Version == 0) {
        wprintf(L"Expected Size/Version to be returned even on ERROR_INSUFFICIENT_BUFFER; got Size=%lu Version=%lu\n",
                out.Size, out.Version);
        return 1;
    }

    wprintf(L"Got interrupt info header despite short buffer: Size=%lu Version=%lu (bytesReturned=%lu)\n", out.Size,
            out.Version, bytes);
    return 0;
}

static int ioctl_get_input_report(const SELECTED_DEVICE *dev)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    BYTE report_id = 0;
    DWORD expected_len = 0;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if (dev->caps_valid) {
        if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06) {
            report_id = 1;
            expected_len = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
        } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x02) {
            report_id = 2;
            expected_len = VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN;
        }
    }

    if (report_id == 2 && dev->attr_valid && dev->attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
        report_id = 4;
        expected_len = VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN;
    }

    if (report_id == 0 && dev->attr_valid) {
        if (dev->attr.ProductID == VIRTIO_INPUT_PID_KEYBOARD) {
            report_id = 1;
            expected_len = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
        } else if (dev->attr.ProductID == VIRTIO_INPUT_PID_MOUSE) {
            report_id = 2;
            expected_len = VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN;
        } else if (dev->attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
            report_id = 4;
            expected_len = VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN;
        }
    }

    if (report_id == 0 || expected_len == 0) {
        wprintf(L"Cannot infer expected report ID/length for this device.\n");
        wprintf(L"Hint: select a keyboard/mouse/tablet interface explicitly.\n");
        return 1;
    }

    BYTE report[64];
    HID_XFER_PACKET_MIN pkt;
    DWORD bytes = 0;
    BOOL ok;

    ZeroMemory(report, sizeof(report));
    report[0] = report_id;

    ZeroMemory(&pkt, sizeof(pkt));
    pkt.reportId = report_id;
    pkt.reportBufferLen = expected_len;
    pkt.reportBuffer = report;

    wprintf(L"\nIssuing IOCTL_HID_GET_INPUT_REPORT (reportId=%u)...\n", (unsigned)report_id);
    ok = DeviceIoControl(dev->handle,
                         IOCTL_HID_GET_INPUT_REPORT,
                         &pkt,
                         (DWORD)sizeof(pkt),
                         &pkt,
                         (DWORD)sizeof(pkt),
                         &bytes,
                         NULL);
    if (!ok) {
        print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT)");
        return 1;
    }

    wprintf(L"Success: %lu bytes: ", bytes);
    dump_hex(report, bytes);
    wprintf(L"\n");

    if (bytes != expected_len) {
        wprintf(L"[FAIL] Unexpected report length (expected %lu)\n", expected_len);
        return 1;
    }
    if (bytes > 0 && report[0] != report_id) {
        wprintf(L"[FAIL] Unexpected ReportID in payload (expected %u, got %u)\n",
                (unsigned)report_id,
                (unsigned)report[0]);
        return 1;
    }

    if (report_id == 1) {
        dump_keyboard_report(report, bytes);
    } else if (report_id == 2) {
        dump_mouse_report(report, bytes, 1);
    } else if (report_id == 4) {
        dump_tablet_report(report, bytes, 1);
    }

    /*
     * Issue the IOCTL again and expect a "no data" style error once there are no
     * new reports available.
     */
    {
        DWORD tries;
        const DWORD max_tries = 50;
        for (tries = 0; tries < max_tries; ++tries) {
            ZeroMemory(report, sizeof(report));
            report[0] = report_id;
            pkt.reportId = report_id;
            pkt.reportBufferLen = expected_len;
            pkt.reportBuffer = report;
            bytes = 0;

            ok = DeviceIoControl(dev->handle,
                                 IOCTL_HID_GET_INPUT_REPORT,
                                 &pkt,
                                 (DWORD)sizeof(pkt),
                                 &pkt,
                                 (DWORD)sizeof(pkt),
                                 &bytes,
                                 NULL);
            if (!ok) {
                DWORD err = GetLastError();
                if (err == ERROR_NO_DATA || err == ERROR_NOT_READY) {
                    wprintf(L"No-data case observed (expected): error %lu\n", err);
                    return 0;
                }
                print_win32_error_w(L"DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT) (unexpected error)", err);
                return 1;
            }

            if (tries == 0) {
                wprintf(L"Another report was available; polling for a no-data response...\n");
            }
            Sleep(10);
        }
    }

    wprintf(L"[FAIL] Did not observe a no-data error after repeated polling.\n");
    wprintf(L"Hint: keep the device still (no mouse movement / key repeats) and retry.\n");
    return 1;
}

static int vioinput_get_log_mask(const SELECTED_DEVICE *dev, DWORD *mask_out)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE || mask_out == NULL) {
        return 0;
    }

    *mask_out = 0;
    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_GET_LOG_MASK, NULL, 0, mask_out, (DWORD)sizeof(*mask_out), &bytes,
                         NULL);
    if (!ok || bytes < sizeof(*mask_out)) {
        print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_GET_LOG_MASK)");
        return 0;
    }

    return 1;
}

static int vioinput_set_log_mask(const SELECTED_DEVICE *dev, DWORD mask)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        return 0;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot set log mask\n");
        return 0;
    }

    ok = DeviceIoControl(dev->handle, IOCTL_VIOINPUT_SET_LOG_MASK, &mask, (DWORD)sizeof(mask), NULL, 0, &bytes, NULL);
    if (!ok) {
        print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_SET_LOG_MASK)");
        return 0;
    }

    return 1;
}

static int hidd_get_input_report(const SELECTED_DEVICE *dev)
{
    BYTE report_id = 0;
    DWORD expected_len = 0;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if (dev->caps_valid) {
        if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06) {
            report_id = 1;
            expected_len = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
        } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x02) {
            report_id = 2;
            expected_len = VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN;
        }
    }

    if (report_id == 2 && dev->attr_valid && dev->attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
        report_id = 4;
        expected_len = VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN;
    }

    if (report_id == 0 && dev->attr_valid) {
        if (dev->attr.ProductID == VIRTIO_INPUT_PID_KEYBOARD) {
            report_id = 1;
            expected_len = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
        } else if (dev->attr.ProductID == VIRTIO_INPUT_PID_MOUSE) {
            report_id = 2;
            expected_len = VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN;
        } else if (dev->attr.ProductID == VIRTIO_INPUT_PID_TABLET) {
            report_id = 4;
            expected_len = VIRTIO_INPUT_EXPECTED_TABLET_INPUT_LEN;
        }
    }

    if (report_id == 0 || expected_len == 0) {
        wprintf(L"Cannot infer expected report ID/length for this device.\n");
        wprintf(L"Hint: select a keyboard/mouse/tablet interface explicitly.\n");
        return 1;
    }

    BYTE report[64];
    BOOL ok;

    ZeroMemory(report, sizeof(report));
    report[0] = report_id;

    wprintf(L"\nCalling HidD_GetInputReport (reportId=%u)...\n", (unsigned)report_id);
    ok = HidD_GetInputReport(dev->handle, report, expected_len);
    if (!ok) {
        print_last_error_w(L"HidD_GetInputReport");
        return 1;
    }

    wprintf(L"Success: %lu bytes: ", expected_len);
    dump_hex(report, expected_len);
    wprintf(L"\n");

    if (report[0] != report_id) {
        wprintf(L"[FAIL] Unexpected ReportID in payload (expected %u, got %u)\n",
                (unsigned)report_id,
                (unsigned)report[0]);
        return 1;
    }

    if (report_id == 1) {
        dump_keyboard_report(report, expected_len);
    } else if (report_id == 2) {
        dump_mouse_report(report, expected_len, 1);
    } else if (report_id == 4) {
        dump_tablet_report(report, expected_len, 1);
    }

    /*
     * Poll until we observe a "no data" style error when there are no new reports
     * available. (If the device is moving/changing state, additional reports may
     * arrive and we may need a few retries.)
     */
    {
        DWORD tries;
        const DWORD max_tries = 50;
        for (tries = 0; tries < max_tries; ++tries) {
            ZeroMemory(report, sizeof(report));
            report[0] = report_id;
            ok = HidD_GetInputReport(dev->handle, report, expected_len);
            if (!ok) {
                DWORD err = GetLastError();
                if (err == ERROR_NO_DATA || err == ERROR_NOT_READY) {
                    wprintf(L"No-data case observed (expected): error %lu\n", err);
                    return 0;
                }
                print_win32_error_w(L"HidD_GetInputReport (unexpected error)", err);
                return 1;
            }

            if (tries == 0) {
                wprintf(L"Another report was available; polling for a no-data response...\n");
            }
            Sleep(10);
        }
    }

    wprintf(L"[FAIL] Did not observe a no-data error after repeated polling.\n");
    wprintf(L"Hint: keep the device still (no mouse movement / key repeats) and retry.\n");
    return 1;
}

static int ioctl_bad_get_input_xfer_packet(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_READ) == 0) {
        wprintf(L"Device was not opened with GENERIC_READ; cannot issue IOCTL_HID_GET_INPUT_REPORT\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_INPUT_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_INPUT_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    err = GetLastError();
    print_win32_error_w(L"DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT bad HID_XFER_PACKET)", err);
    return 0;
}

static int ioctl_bad_get_input_report(const SELECTED_DEVICE *dev)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    BYTE inbuf[64];
    HID_XFER_PACKET_MIN *pkt;
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_READ) == 0) {
        wprintf(L"Device was not opened with GENERIC_READ; cannot issue IOCTL_HID_GET_INPUT_REPORT\n");
        return 1;
    }

    ZeroMemory(inbuf, sizeof(inbuf));
    pkt = (HID_XFER_PACKET_MIN *)inbuf;
    pkt->reportId = 1; // keyboard (doesn't matter; invalid buffer fails before ID checks)
    pkt->reportBufferLen = VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN;
    pkt->reportBuffer = (PUCHAR)(ULONG_PTR)0x1; // invalid user pointer

    wprintf(L"\nIssuing IOCTL_HID_GET_INPUT_REPORT with invalid reportBuffer=%p...\n", pkt->reportBuffer);
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_INPUT_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_INPUT_REPORT bad reportBuffer)");
    return 0;
}

static int ioctl_bad_write_report(const SELECTED_DEVICE *dev)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    BYTE inbuf[64];
    HID_XFER_PACKET_MIN *pkt;
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_WRITE_REPORT\n");
        return 1;
    }

    ZeroMemory(inbuf, sizeof(inbuf));
    pkt = (HID_XFER_PACKET_MIN *)inbuf;
    pkt->reportId = 1; // keyboard
    pkt->reportBufferLen = 2;
    pkt->reportBuffer = (PUCHAR)(ULONG_PTR)0x1; // invalid user pointer

    wprintf(L"\nIssuing IOCTL_HID_WRITE_REPORT with invalid reportBuffer=%p...\n", pkt->reportBuffer);
    ok = DeviceIoControl(dev->handle, IOCTL_HID_WRITE_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_WRITE_REPORT bad reportBuffer)");
    return 0;
}

static int ioctl_bad_xfer_packet(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_WRITE_REPORT\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_WRITE_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_WRITE_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_WRITE_REPORT bad HID_XFER_PACKET)");
    return 0;
}

static int ioctl_bad_read_xfer_packet(const SELECTED_DEVICE *dev)
{
    const DWORD timeout_ms = 2000;
    const DWORD cancel_wait_ms = 1000;
    HANDLE h = INVALID_HANDLE_VALUE;
    HANDLE ev = NULL;
    OVERLAPPED ov;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;
    DWORD wait;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }
    if (dev->path == NULL) {
        wprintf(L"Selected device path unavailable; cannot open an overlapped handle for IOCTL_HID_READ_REPORT\n");
        return 1;
    }
    if ((dev->desired_access & GENERIC_READ) == 0) {
        wprintf(L"Device was not opened with GENERIC_READ; cannot issue IOCTL_HID_READ_REPORT\n");
        return 1;
    }

    // Use a separate overlapped handle so we can enforce a timeout.
    h = CreateFileW(dev->path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        print_last_error_w(L"CreateFile(overlapped IOCTL_HID_READ_REPORT)");
        return 1;
    }

    ev = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (ev == NULL) {
        print_last_error_w(L"CreateEvent(IOCTL_HID_READ_REPORT)");
        CloseHandle(h);
        return 1;
    }

    ZeroMemory(&ov, sizeof(ov));
    ov.hEvent = ev;
    ResetEvent(ev);

    wprintf(L"\nIssuing IOCTL_HID_READ_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(h, IOCTL_HID_READ_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, &ov);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        CloseHandle(ev);
        CloseHandle(h);
        return 1;
    }

    err = GetLastError();
    if (err != ERROR_IO_PENDING) {
        print_win32_error_w(L"DeviceIoControl(IOCTL_HID_READ_REPORT bad HID_XFER_PACKET)", err);
        CloseHandle(ev);
        CloseHandle(h);
        return 0;
    }

    wait = WaitForSingleObject(ev, timeout_ms);
    if (wait == WAIT_OBJECT_0) {
        ok = GetOverlappedResult(h, &ov, &bytes, FALSE);
        if (ok) {
            wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
            CloseHandle(ev);
            CloseHandle(h);
            return 1;
        }
        err = GetLastError();
        print_win32_error_w(L"DeviceIoControl(IOCTL_HID_READ_REPORT bad HID_XFER_PACKET)", err);
        CloseHandle(ev);
        CloseHandle(h);
        return 0;
    }

    if (wait == WAIT_TIMEOUT) {
        wprintf(L"IOCTL_HID_READ_REPORT did not complete within %lu ms; cancelling...\n", timeout_ms);
        (VOID)CancelIo(h);
        wait = WaitForSingleObject(ev, cancel_wait_ms);
        if (wait != WAIT_OBJECT_0) {
            wprintf(L"[FATAL] IOCTL_HID_READ_REPORT did not cancel within %lu ms; terminating.\n", cancel_wait_ms);
            ExitProcess(1);
        }
        // Timed out => negative test failed (it should fail fast on invalid pointers).
        CloseHandle(ev);
        CloseHandle(h);
        return 1;
    }

    err = GetLastError();
    print_win32_error_w(L"WaitForSingleObject(IOCTL_HID_READ_REPORT)", err);
    (VOID)CancelIo(h);
    CloseHandle(ev);
    CloseHandle(h);
    return 1;
}

static int ioctl_bad_read_report(const SELECTED_DEVICE *dev)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    const DWORD timeout_ms = 2000;
    const DWORD cancel_wait_ms = 1000;
    HANDLE h = INVALID_HANDLE_VALUE;
    HANDLE ev = NULL;
    OVERLAPPED ov;
    BYTE inbuf[64];
    HID_XFER_PACKET_MIN *pkt;
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;
    DWORD wait;
    ULONG report_len = 16;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }
    if (dev->path == NULL) {
        wprintf(L"Selected device path unavailable; cannot open an overlapped handle for IOCTL_HID_READ_REPORT\n");
        return 1;
    }
    if ((dev->desired_access & GENERIC_READ) == 0) {
        wprintf(L"Device was not opened with GENERIC_READ; cannot issue IOCTL_HID_READ_REPORT\n");
        return 1;
    }

    if (dev->caps_valid && dev->caps.InputReportByteLength != 0) {
        report_len = dev->caps.InputReportByteLength;
    }

    ZeroMemory(inbuf, sizeof(inbuf));
    pkt = (HID_XFER_PACKET_MIN *)inbuf;
    pkt->reportId = 1; // keyboard
    pkt->reportBufferLen = report_len;
    pkt->reportBuffer = (PUCHAR)(ULONG_PTR)0x1; // invalid user pointer

    // Use a separate overlapped handle so we can enforce a timeout.
    h = CreateFileW(dev->path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        print_last_error_w(L"CreateFile(overlapped IOCTL_HID_READ_REPORT)");
        return 1;
    }

    ev = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (ev == NULL) {
        print_last_error_w(L"CreateEvent(IOCTL_HID_READ_REPORT)");
        CloseHandle(h);
        return 1;
    }

    ZeroMemory(&ov, sizeof(ov));
    ov.hEvent = ev;
    ResetEvent(ev);

    wprintf(L"\nIssuing IOCTL_HID_READ_REPORT with invalid reportBuffer=%p (len=%lu)...\n", pkt->reportBuffer, (DWORD)pkt->reportBufferLen);
    ok = DeviceIoControl(h, IOCTL_HID_READ_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, &ov);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        CloseHandle(ev);
        CloseHandle(h);
        return 1;
    }

    err = GetLastError();
    if (err != ERROR_IO_PENDING) {
        print_win32_error_w(L"DeviceIoControl(IOCTL_HID_READ_REPORT bad reportBuffer)", err);
        CloseHandle(ev);
        CloseHandle(h);
        return 0;
    }

    wait = WaitForSingleObject(ev, timeout_ms);
    if (wait == WAIT_OBJECT_0) {
        ok = GetOverlappedResult(h, &ov, &bytes, FALSE);
        if (ok) {
            wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
            CloseHandle(ev);
            CloseHandle(h);
            return 1;
        }
        err = GetLastError();
        print_win32_error_w(L"DeviceIoControl(IOCTL_HID_READ_REPORT bad reportBuffer)", err);
        CloseHandle(ev);
        CloseHandle(h);
        return 0;
    }

    if (wait == WAIT_TIMEOUT) {
        wprintf(L"IOCTL_HID_READ_REPORT did not complete within %lu ms; cancelling...\n", timeout_ms);
        (VOID)CancelIo(h);
        wait = WaitForSingleObject(ev, cancel_wait_ms);
        if (wait != WAIT_OBJECT_0) {
            wprintf(L"[FATAL] IOCTL_HID_READ_REPORT did not cancel within %lu ms; terminating.\n", cancel_wait_ms);
            ExitProcess(1);
        }
        // Timed out => negative test failed (it should fail fast on invalid pointers).
        CloseHandle(ev);
        CloseHandle(h);
        return 1;
    }

    err = GetLastError();
    print_win32_error_w(L"WaitForSingleObject(IOCTL_HID_READ_REPORT)", err);
    (VOID)CancelIo(h);
    CloseHandle(ev);
    CloseHandle(h);
    return 1;
}

static int ioctl_bad_set_output_xfer_packet(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_SET_OUTPUT_REPORT\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_SET_OUTPUT_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT bad HID_XFER_PACKET)");
    return 0;
}

static int ioctl_bad_set_output_report(const SELECTED_DEVICE *dev)
{
    typedef struct HID_XFER_PACKET_MIN {
        PUCHAR reportBuffer;
        ULONG reportBufferLen;
        UCHAR reportId;
    } HID_XFER_PACKET_MIN;

    BYTE inbuf[64];
    HID_XFER_PACKET_MIN *pkt;
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_SET_OUTPUT_REPORT\n");
        return 1;
    }

    ZeroMemory(inbuf, sizeof(inbuf));
    pkt = (HID_XFER_PACKET_MIN *)inbuf;
    pkt->reportId = 1; // keyboard
    pkt->reportBufferLen = 2;
    pkt->reportBuffer = (PUCHAR)(ULONG_PTR)0x1; // invalid user pointer

    wprintf(L"\nIssuing IOCTL_HID_SET_OUTPUT_REPORT with invalid reportBuffer=%p...\n", pkt->reportBuffer);
    ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, inbuf, (DWORD)sizeof(inbuf), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT bad reportBuffer)");
    return 0;
}

static int ioctl_bad_get_report_descriptor(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_REPORT_DESCRIPTOR with invalid output buffer pointer...\n");
    ok = DeviceIoControl(
        dev->handle,
        IOCTL_HID_GET_REPORT_DESCRIPTOR,
        NULL,
        0,
        (PVOID)(ULONG_PTR)0x1,
        4096,
        &bytes,
        NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_REPORT_DESCRIPTOR bad output buffer)");
    return 0;
}

static int ioctl_bad_get_collection_descriptor(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;
    DWORD err;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_COLLECTION_DESCRIPTOR with invalid output buffer pointer...\n");
    ok = DeviceIoControl(
        dev->handle,
        IOCTL_HID_GET_COLLECTION_DESCRIPTOR,
        NULL,
        0,
        (PVOID)(ULONG_PTR)0x1,
        4096,
        &bytes,
        NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    err = GetLastError();
    print_win32_error_w(L"DeviceIoControl(IOCTL_HID_GET_COLLECTION_DESCRIPTOR bad output buffer)", err);

    // If the primary function code is not supported, try a known alternate.
    if (err == ERROR_INVALID_FUNCTION || err == ERROR_NOT_SUPPORTED) {
        bytes = 0;
        wprintf(L"Primary IOCTL returned %lu; trying alternate IOCTL code...\n", err);
        ok = DeviceIoControl(
            dev->handle,
            IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT,
            NULL,
            0,
            (PVOID)(ULONG_PTR)0x1,
            4096,
            &bytes,
            NULL);
        if (ok) {
            wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
            return 1;
        }

        print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_COLLECTION_DESCRIPTOR_ALT bad output buffer)");
    }
    return 0;
}

static int ioctl_bad_get_device_descriptor(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_DEVICE_DESCRIPTOR with invalid output buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_DEVICE_DESCRIPTOR, NULL, 0, (PVOID)(ULONG_PTR)0x1, 256, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_DEVICE_DESCRIPTOR bad output buffer)");
    return 0;
}

static int ioctl_bad_get_string(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_STRING with invalid input buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_STRING, (PVOID)(ULONG_PTR)0x1, sizeof(ULONG), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_STRING bad input buffer)");
    return 0;
}

static int ioctl_bad_get_indexed_string(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_INDEXED_STRING with invalid input buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_INDEXED_STRING, (PVOID)(ULONG_PTR)0x1, sizeof(ULONG), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_INDEXED_STRING bad input buffer)");
    return 0;
}

static int ioctl_bad_get_string_out(const SELECTED_DEVICE *dev)
{
    ULONG stringId = 1; // HID_STRING_ID_IMANUFACTURER
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_STRING with invalid output buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_STRING, &stringId, (DWORD)sizeof(stringId), (PVOID)(ULONG_PTR)0x1, 256, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_STRING bad output buffer)");
    return 0;
}

static int ioctl_bad_get_indexed_string_out(const SELECTED_DEVICE *dev)
{
    ULONG stringIndex = 1;
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_INDEXED_STRING with invalid output buffer pointer...\n");
    ok = DeviceIoControl(
        dev->handle,
        IOCTL_HID_GET_INDEXED_STRING,
        &stringIndex,
        (DWORD)sizeof(stringIndex),
        (PVOID)(ULONG_PTR)0x1,
        256,
        &bytes,
        NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return 1;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_INDEXED_STRING bad output buffer)");
    return 0;
}

static int hidd_bad_set_output_report(const SELECTED_DEVICE *dev)
{
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return 1;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot call HidD_SetOutputReport\n");
        return 1;
    }

    wprintf(L"\nCalling HidD_SetOutputReport with invalid buffer pointer...\n");
    ok = HidD_SetOutputReport(dev->handle, (PVOID)(ULONG_PTR)0x1, 2);
    if (ok) {
        wprintf(L"Unexpected success\n");
        return 1;
    }

    print_last_error_w(L"HidD_SetOutputReport (bad buffer)");
    return 0;
}

int wmain(int argc, wchar_t **argv)
{
    OPTIONS opt;
    SELECTED_DEVICE dev;
    int i;

    ZeroMemory(&opt, sizeof(opt));
    ZeroMemory(&dev, sizeof(dev));
    dev.handle = INVALID_HANDLE_VALUE;

    for (i = 1; i < argc; i++) {
        if (wcscmp(argv[i], L"--help") == 0 || wcscmp(argv[i], L"-h") == 0 ||
            wcscmp(argv[i], L"/?") == 0) {
            print_usage();
            return 0;
        }

        if (wcscmp(argv[i], L"--list") == 0) {
            opt.list_only = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--selftest") == 0) {
            opt.selftest = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--json") == 0) {
            opt.json = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--quiet") == 0) {
            opt.quiet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--keyboard") == 0) {
            opt.want_keyboard = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--mouse") == 0) {
            opt.want_mouse = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--consumer") == 0) {
            opt.want_consumer = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--tablet") == 0) {
            opt.want_tablet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--dump-desc") == 0) {
            opt.dump_desc = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--dump-collection-desc") == 0) {
            opt.dump_collection_desc = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--state") == 0) {
            opt.query_state = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--interrupt-info") == 0) {
            opt.query_interrupt_info = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--interrupt-info-json") == 0) {
            opt.query_interrupt_info = 1;
            opt.query_interrupt_info_json = 1;
            opt.quiet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--counters") == 0) {
            opt.query_counters = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--counters-json") == 0) {
            opt.query_counters = 1;
            opt.query_counters_json = 1;
            opt.quiet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--reset-counters") == 0) {
            opt.reset_counters = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--get-log-mask") == 0) {
            opt.get_log_mask = 1;
            continue;
        }

        if ((wcscmp(argv[i], L"--set-log-mask") == 0) && i + 1 < argc) {
            DWORD tmp;
            if (!parse_u32_hex(argv[i + 1], &tmp)) {
                wprintf(L"Invalid log mask: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_set_log_mask = 1;
            opt.set_log_mask = tmp;
            i++;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-xfer-packet") == 0) {
            opt.ioctl_bad_xfer_packet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-write-report") == 0) {
            opt.ioctl_bad_write_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-read-xfer-packet") == 0) {
            opt.ioctl_bad_read_xfer_packet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-read-report") == 0) {
            opt.ioctl_bad_read_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-input-xfer-packet") == 0) {
            opt.ioctl_bad_get_input_xfer_packet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-input-report") == 0) {
            opt.ioctl_bad_get_input_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-set-output-xfer-packet") == 0) {
            opt.ioctl_bad_set_output_xfer_packet = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-set-output-report") == 0) {
            opt.ioctl_bad_set_output_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-report-descriptor") == 0) {
            opt.ioctl_bad_get_report_descriptor = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-collection-descriptor") == 0) {
            opt.ioctl_bad_get_collection_descriptor = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-device-descriptor") == 0) {
            opt.ioctl_bad_get_device_descriptor = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-string") == 0) {
            opt.ioctl_bad_get_string = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-indexed-string") == 0) {
            opt.ioctl_bad_get_indexed_string = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-string-out") == 0) {
            opt.ioctl_bad_get_string_out = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-bad-get-indexed-string-out") == 0) {
            opt.ioctl_bad_get_indexed_string_out = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-query-counters-short") == 0) {
            opt.ioctl_query_counters_short = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-query-state-short") == 0) {
            opt.ioctl_query_state_short = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-query-interrupt-info-short") == 0) {
            opt.ioctl_query_interrupt_info_short = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--ioctl-get-input-report") == 0) {
            opt.ioctl_get_input_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--hidd-get-input-report") == 0) {
            opt.hidd_get_input_report = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--hidd-bad-set-output-report") == 0) {
            opt.hidd_bad_set_output_report = 1;
            continue;
        }

        if ((wcscmp(argv[i], L"--vid") == 0) && i + 1 < argc) {
            if (!parse_u16_hex(argv[i + 1], &opt.vid)) {
                wprintf(L"Invalid VID: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_vid = 1;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--pid") == 0) && i + 1 < argc) {
            if (!parse_u16_hex(argv[i + 1], &opt.pid)) {
                wprintf(L"Invalid PID: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_pid = 1;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--index") == 0) && i + 1 < argc) {
            if (!parse_u32_dec(argv[i + 1], &opt.index)) {
                wprintf(L"Invalid index: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_index = 1;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--duration") == 0) && i + 1 < argc) {
            if (!parse_u32_dec(argv[i + 1], &opt.duration_secs)) {
                wprintf(L"Invalid duration: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_duration = 1;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--count") == 0) && i + 1 < argc) {
            if (!parse_u32_dec(argv[i + 1], &opt.count)) {
                wprintf(L"Invalid count: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.have_count = 1;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--led") == 0) && i + 1 < argc) {
            USHORT tmp;
            if (!parse_u16_hex(argv[i + 1], &tmp) || tmp > 0xFF) {
                wprintf(L"Invalid LED mask: %ls\n", argv[i + 1]);
                return 2;
            }
            if (opt.have_led_mask) {
                wprintf(L"Only one of --led / --led-hidd / --led-ioctl-set-output may be specified.\n");
                return 2;
            }
            opt.have_led_mask = 1;
            opt.led_via_hidd = 0;
            opt.led_mask = (BYTE)tmp;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--led-hidd") == 0) && i + 1 < argc) {
            USHORT tmp;
            if (!parse_u16_hex(argv[i + 1], &tmp) || tmp > 0xFF) {
                wprintf(L"Invalid LED mask: %ls\n", argv[i + 1]);
                return 2;
            }
            if (opt.have_led_mask) {
                wprintf(L"Only one of --led / --led-hidd / --led-ioctl-set-output may be specified.\n");
                return 2;
            }
            opt.have_led_mask = 1;
            opt.led_via_hidd = 1;
            opt.led_mask = (BYTE)tmp;
            i++;
            continue;
        }

        if ((wcscmp(argv[i], L"--led-ioctl-set-output") == 0) && i + 1 < argc) {
            USHORT tmp;
            if (!parse_u16_hex(argv[i + 1], &tmp) || tmp > 0xFF) {
                wprintf(L"Invalid LED mask: %ls\n", argv[i + 1]);
                return 2;
            }
            if (opt.have_led_mask) {
                wprintf(L"Only one of --led / --led-hidd / --led-ioctl-set-output may be specified.\n");
                return 2;
            }
            opt.have_led_mask = 1;
            opt.have_led_ioctl_set_output = 1;
            opt.led_ioctl_set_output_mask = (BYTE)tmp;
            i++;
            continue;
        }

        if (wcscmp(argv[i], L"--led-cycle") == 0) {
            opt.led_cycle = 1;
            continue;
        }

        if ((wcscmp(argv[i], L"--led-spam") == 0) && i + 1 < argc) {
            if (!parse_u32_dec(argv[i + 1], &opt.led_spam_count)) {
                wprintf(L"Invalid LED spam count: %ls\n", argv[i + 1]);
                return 2;
            }
            opt.led_spam = 1;
            i++;
            continue;
        }

        wprintf(L"Unknown argument: %ls\n", argv[i]);
        print_usage();
        return 2;
    }

    if ((opt.want_keyboard + opt.want_mouse + opt.want_consumer + opt.want_tablet) > 1) {
        wprintf(L"--keyboard, --mouse, --consumer, and --tablet are mutually exclusive.\n");
        return 2;
    }
    if (opt.list_only &&
        (opt.query_state || opt.query_interrupt_info || opt.query_counters || opt.reset_counters || opt.ioctl_query_counters_short ||
         opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short)) {
        wprintf(
            L"--list is mutually exclusive with --state, --interrupt-info, --counters/--counters-json/--reset-counters, and --ioctl-query-*-short.\n");
        return 2;
    }
    if (opt.json && !(opt.list_only || opt.selftest)) {
        wprintf(L"--json is only supported with --list or --selftest.\n");
        return 2;
    }
    if (opt.selftest &&
        (opt.query_state || opt.query_interrupt_info || opt.list_only || opt.dump_desc || opt.dump_collection_desc || opt.have_vid || opt.have_pid ||
         opt.have_index || opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report ||
          opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report || opt.ioctl_bad_get_input_xfer_packet ||
          opt.ioctl_bad_get_input_report || opt.ioctl_bad_set_output_xfer_packet ||
          opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor ||
          opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string ||
          opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out || opt.ioctl_query_counters_short ||
          opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short ||
          opt.ioctl_get_input_report || opt.hidd_get_input_report || opt.hidd_bad_set_output_report || opt.have_led_ioctl_set_output ||
          opt.query_counters || opt.query_counters_json || opt.reset_counters)) {
        wprintf(
            L"--selftest cannot be combined with --state/--interrupt-info, --list, descriptor dump options, --vid/--pid/--index, counters, LED, or negative-test options.\n");
        return 2;
    }
    if (opt.query_state &&
        (opt.selftest || opt.query_interrupt_info || opt.query_counters || opt.query_counters_json || opt.reset_counters ||
         opt.ioctl_query_counters_short || opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short ||
         opt.ioctl_get_input_report || opt.hidd_get_input_report || opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc ||
         opt.dump_collection_desc || opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet ||
         opt.ioctl_bad_read_report || opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out ||
         opt.hidd_bad_set_output_report || opt.have_led_ioctl_set_output)) {
        wprintf(
            L"--state is mutually exclusive with --selftest, --interrupt-info, --counters/--counters-json/--reset-counters, and other report/IOCTL tests.\n");
        return 2;
    }
    if (opt.query_interrupt_info &&
        (opt.selftest || opt.list_only || opt.query_state || opt.query_counters || opt.query_counters_json || opt.reset_counters ||
         opt.ioctl_query_counters_short || opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short ||
         opt.ioctl_get_input_report || opt.hidd_get_input_report || opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc ||
         opt.dump_collection_desc || opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet ||
         opt.ioctl_bad_read_report || opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out ||
         opt.hidd_bad_set_output_report || opt.have_led_ioctl_set_output)) {
        wprintf(
            L"--interrupt-info is mutually exclusive with --list, --selftest, --state, --counters/--counters-json/--reset-counters, and other report/IOCTL tests.\n");
        return 2;
    }
    if ((opt.get_log_mask || opt.have_set_log_mask) &&
        (opt.selftest || opt.list_only || opt.query_state || opt.query_interrupt_info || opt.query_counters || opt.query_counters_json ||
         opt.reset_counters ||
         opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc || opt.dump_collection_desc ||
         opt.have_duration || opt.have_count ||
         opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out ||
         opt.ioctl_query_counters_short || opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short ||
         opt.ioctl_get_input_report || opt.hidd_get_input_report || opt.hidd_bad_set_output_report ||
         opt.have_led_ioctl_set_output)) {
        wprintf(L"--get-log-mask/--set-log-mask are mutually exclusive with other action/negative-test modes.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.led_cycle) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --led-cycle are mutually exclusive.\n");
        return 2;
    }
    if (opt.led_cycle && opt.led_spam) {
        wprintf(L"--led-cycle and --led-spam are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_write_report) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-write-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_read_xfer_packet) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-read-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_read_report) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-read-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_input_xfer_packet) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-input-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_input_report) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-input-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_report_descriptor) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-report-descriptor are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_collection_descriptor) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-collection-descriptor are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_device_descriptor) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-device-descriptor are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_string) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-string are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_indexed_string) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-indexed-string are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_string_out) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-string-out are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_indexed_string_out) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-get-indexed-string-out are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_xfer_packet) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.hidd_bad_set_output_report) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_xfer_packet && opt.ioctl_bad_write_report) {
        wprintf(L"--ioctl-bad-xfer-packet and --ioctl-bad-write-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_read_xfer_packet && opt.ioctl_bad_read_report) {
        wprintf(L"--ioctl-bad-read-xfer-packet and --ioctl-bad-read-report are mutually exclusive.\n");
        return 2;
    }
    if ((opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report) &&
        (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_set_output_xfer_packet ||
         opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out ||
         opt.ioctl_bad_get_indexed_string_out || opt.hidd_bad_set_output_report)) {
        wprintf(L"IOCTL_HID_READ_REPORT negative tests are mutually exclusive with other negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_input_xfer_packet && opt.ioctl_bad_get_input_report) {
        wprintf(L"--ioctl-bad-get-input-xfer-packet and --ioctl-bad-get-input-report are mutually exclusive.\n");
        return 2;
    }
    if ((opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report) &&
        (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out ||
         opt.hidd_bad_set_output_report)) {
        wprintf(L"IOCTL_HID_GET_INPUT_REPORT negative tests are mutually exclusive with other negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_xfer_packet && opt.hidd_bad_set_output_report) {
        wprintf(L"--ioctl-bad-xfer-packet and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_write_report && opt.hidd_bad_set_output_report) {
        wprintf(L"--ioctl-bad-write-report and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_set_output_xfer_packet) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-set-output-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_set_output_report) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --ioctl-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_set_output_xfer_packet && opt.ioctl_bad_set_output_report) {
        wprintf(L"--ioctl-bad-set-output-xfer-packet and --ioctl-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_set_output_xfer_packet && opt.hidd_bad_set_output_report) {
        wprintf(L"--ioctl-bad-set-output-xfer-packet and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_set_output_report && opt.hidd_bad_set_output_report) {
        wprintf(L"--ioctl-bad-set-output-report and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_set_output_xfer_packet && (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report)) {
        wprintf(L"--ioctl-bad-set-output-xfer-packet is mutually exclusive with IOCTL_HID_WRITE_REPORT negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_set_output_report && (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report)) {
        wprintf(L"--ioctl-bad-set-output-report is mutually exclusive with IOCTL_HID_WRITE_REPORT negative tests.\n");
        return 2;
    }
    if ((opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out ||
         opt.ioctl_bad_get_indexed_string_out) &&
        (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report ||
         opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report || opt.ioctl_bad_set_output_xfer_packet ||
         opt.ioctl_bad_set_output_report || opt.hidd_bad_set_output_report)) {
        wprintf(L"Descriptor/string negative tests are mutually exclusive with IOCTL read/write negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_report_descriptor &&
        (opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string ||
         opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-report-descriptor is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_collection_descriptor &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-collection-descriptor is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_device_descriptor &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-device-descriptor is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_string &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-string is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_indexed_string &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-indexed-string is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_string_out &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_indexed_string_out)) {
        wprintf(L"--ioctl-bad-get-string-out is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_indexed_string_out &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor ||
         opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out)) {
        wprintf(L"--ioctl-bad-get-indexed-string-out is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }

    if ((opt.query_counters || opt.reset_counters) &&
        (opt.query_state || opt.query_interrupt_info || opt.ioctl_get_input_report || opt.hidd_get_input_report ||
         opt.ioctl_query_counters_short || opt.ioctl_query_state_short || opt.ioctl_query_interrupt_info_short ||
         opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc || opt.dump_collection_desc ||
         opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report ||
         opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_collection_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string || opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out ||
         opt.hidd_bad_set_output_report || opt.have_led_ioctl_set_output)) {
        wprintf(
            L"--counters/--reset-counters are mutually exclusive with --state/--interrupt-info, GetInputReport tests, IOCTL counters selftests, LED actions, descriptor dumps, and negative tests.\n");
        return 2;
    }

    if (opt.list_only && opt.json) {
        return list_hid_devices_json() ? 0 : 1;
    }

    if (opt.selftest) {
        return run_selftest(&opt);
    }

    if (opt.ioctl_get_input_report &&
        (opt.query_counters || opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc || opt.dump_collection_desc ||
         opt.hidd_get_input_report ||
         opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet || opt.ioctl_bad_read_report ||
         opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report || opt.ioctl_bad_set_output_xfer_packet ||
         opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_collection_descriptor ||
         opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string ||
         opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out || opt.hidd_bad_set_output_report)) {
        wprintf(L"--ioctl-get-input-report is mutually exclusive with other action/negative-test modes.\n");
        return 2;
    }

    if (opt.hidd_get_input_report &&
        (opt.query_counters || opt.have_led_mask || opt.led_cycle || opt.led_spam || opt.dump_desc || opt.dump_collection_desc ||
         opt.ioctl_get_input_report || opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_read_xfer_packet ||
         opt.ioctl_bad_read_report || opt.ioctl_bad_get_input_xfer_packet || opt.ioctl_bad_get_input_report ||
         opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report || opt.ioctl_bad_get_report_descriptor ||
         opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string ||
         opt.ioctl_bad_get_string_out || opt.ioctl_bad_get_indexed_string_out || opt.hidd_bad_set_output_report)) {
        wprintf(L"--hidd-get-input-report is mutually exclusive with other action/negative-test modes.\n");
        return 2;
    }

    if (!enumerate_hid_devices(&opt, &dev)) {
        if (opt.query_counters_json || opt.query_interrupt_info_json) {
            fwprintf(stderr, L"No matching HID devices found.\n");
        } else {
            wprintf(L"No matching HID devices found.\n");
        }
        return 1;
    }

    if (opt.list_only) {
        return 0;
    }

    if (!opt.quiet) {
        wprintf(L"\nSelected device:\n");
        wprintf(L"  Path: %ls\n", dev.path ? dev.path : L"<null>");
        if (dev.attr_valid) {
            wprintf(L"  VID:PID %04X:%04X (ver %04X)\n", dev.attr.VendorID, dev.attr.ProductID,
                    dev.attr.VersionNumber);
        } else {
            wprintf(L"  VID:PID <unavailable>\n");
        }
        if (dev.caps_valid) {
            wprintf(L"  UsagePage:Usage %04X:%04X\n", dev.caps.UsagePage, dev.caps.Usage);
            wprintf(L"  Report bytes (in/out/feat): %u / %u / %u\n", dev.caps.InputReportByteLength,
                    dev.caps.OutputReportByteLength, dev.caps.FeatureReportByteLength);
        }
        if (dev.report_desc_valid) {
            wprintf(L"  Report descriptor length: %lu bytes\n", dev.report_desc_len);
        }
        if (dev.hid_report_desc_valid) {
            wprintf(L"  HID descriptor report length: %lu bytes\n", dev.hid_report_desc_len);
        }
        if (dev.report_desc_valid && dev.hid_report_desc_valid && dev.report_desc_len != dev.hid_report_desc_len) {
            wprintf(L"  [WARN] report descriptor length mismatch (IOCTL=%lu, HID=%lu)\n", dev.report_desc_len,
                    dev.hid_report_desc_len);
        }
    }

    if (opt.query_state) {
        BYTE* buf;
        DWORD bytes = 0;
        const VIOINPUT_STATE* st;

        buf = NULL;
        if (!query_vioinput_state_blob(dev.handle, &buf, &bytes) || buf == NULL) {
            print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_STATE)");
            free_selected_device(&dev);
            return 1;
        }

        st = (const VIOINPUT_STATE*)buf;
        print_vioinput_state(st, bytes);
        free(buf);
        free_selected_device(&dev);
        return 0;
    }

    if (opt.query_interrupt_info) {
        BYTE* buf;
        DWORD bytes = 0;
        const VIOINPUT_INTERRUPT_INFO* info;

        buf = NULL;
        if (!query_vioinput_interrupt_info_blob(dev.handle, &buf, &bytes) || buf == NULL) {
            if (opt.query_interrupt_info_json) {
                print_last_error_file_w(stderr, L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO)");
            } else {
                print_last_error_w(L"DeviceIoControl(IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO)");
            }
            free_selected_device(&dev);
            return 1;
        }

        info = (const VIOINPUT_INTERRUPT_INFO*)buf;
        if (opt.query_interrupt_info_json) {
            print_vioinput_interrupt_info_json(info, bytes);
        } else {
            print_vioinput_interrupt_info(info, bytes);
        }
        free(buf);
        free_selected_device(&dev);
        return 0;
    }

    if (opt.have_set_log_mask) {
        DWORD mask = opt.set_log_mask;
        wprintf(L"\nSetting virtio-input DiagnosticsMask to 0x%08lX...\n", mask);
        if (!vioinput_set_log_mask(&dev, mask)) {
            free_selected_device(&dev);
            return 1;
        }
    }
    if (opt.get_log_mask || opt.have_set_log_mask) {
        DWORD mask = 0;
        if (!vioinput_get_log_mask(&dev, &mask)) {
            free_selected_device(&dev);
            return 1;
        }
        wprintf(L"virtio-input DiagnosticsMask: 0x%08lX\n", mask);
        free_selected_device(&dev);
        return 0;
    }

    if (opt.led_spam) {
        BYTE on_mask;
        int via_ioctl_set_output;
        int via_hidd;

        /* Default to all 5 HID boot keyboard LED bits (Num/Caps/Scroll/Compose/Kana). */
        on_mask = 0x1F;
        via_ioctl_set_output = opt.have_led_ioctl_set_output ? 1 : 0;
        via_hidd = opt.led_via_hidd ? 1 : 0;

        if (opt.have_led_mask) {
            if (via_ioctl_set_output) {
                on_mask = opt.led_ioctl_set_output_mask;
            } else {
                on_mask = opt.led_mask;
            }
        }

        if (!spam_keyboard_leds(&dev, on_mask, opt.led_spam_count, via_hidd, via_ioctl_set_output)) {
            free_selected_device(&dev);
            return 1;
        }

        free_selected_device(&dev);
        return 0;
    }

    if (opt.have_led_mask) {
        if (opt.have_led_ioctl_set_output) {
            send_keyboard_led_report_ioctl_set_output(&dev, opt.led_ioctl_set_output_mask);
        } else if (opt.led_via_hidd) {
            send_keyboard_led_report_hidd(&dev, opt.led_mask);
        } else {
            send_keyboard_led_report(&dev, opt.led_mask);
        }
    }
    if (opt.led_cycle) {
        cycle_keyboard_leds(&dev);
    }
    if (opt.dump_desc) {
        dump_report_descriptor(dev.handle);
    }
    if (opt.dump_collection_desc) {
        dump_collection_descriptor(dev.handle);
    }

    if (opt.reset_counters) {
        int rc = reset_vioinput_counters(&dev, opt.quiet);
        if (rc != 0) {
            free_selected_device(&dev);
            return rc;
        }
        if (!opt.query_counters) {
            free_selected_device(&dev);
            return 0;
        }
    }
    if (opt.query_counters) {
        int rc = opt.query_counters_json ? dump_vioinput_counters_json(&dev) : dump_vioinput_counters(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_query_counters_short) {
        int rc = ioctl_query_counters_short(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_query_state_short) {
        int rc = ioctl_query_state_short(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_query_interrupt_info_short) {
        int rc = ioctl_query_interrupt_info_short(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_bad_write_report) {
        int rc = ioctl_bad_write_report(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_bad_read_xfer_packet) {
        int rc = ioctl_bad_read_xfer_packet(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_bad_read_report) {
        int rc = ioctl_bad_read_report(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_bad_get_input_xfer_packet) {
        int rc = ioctl_bad_get_input_xfer_packet(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_bad_get_input_report) {
        int rc = ioctl_bad_get_input_report(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.hidd_bad_set_output_report) {
        int rc = hidd_bad_set_output_report(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_xfer_packet) {
        int rc = ioctl_bad_xfer_packet(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_set_output_xfer_packet) {
        int rc = ioctl_bad_set_output_xfer_packet(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_set_output_report) {
        int rc = ioctl_bad_set_output_report(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_report_descriptor) {
        int rc = ioctl_bad_get_report_descriptor(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_collection_descriptor) {
        int rc = ioctl_bad_get_collection_descriptor(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_device_descriptor) {
        int rc = ioctl_bad_get_device_descriptor(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_string) {
        int rc = ioctl_bad_get_string(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_indexed_string) {
        int rc = ioctl_bad_get_indexed_string(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_string_out) {
        int rc = ioctl_bad_get_string_out(&dev);
        free_selected_device(&dev);
        return rc;
    }
    if (opt.ioctl_bad_get_indexed_string_out) {
        int rc = ioctl_bad_get_indexed_string_out(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.ioctl_get_input_report) {
        int rc = ioctl_get_input_report(&dev);
        free_selected_device(&dev);
        return rc;
    }

    if (opt.hidd_get_input_report) {
        int rc = hidd_get_input_report(&dev);
        free_selected_device(&dev);
        return rc;
    }

    read_reports_loop(&dev, &opt);
    free_selected_device(&dev);
    return 0;
}
