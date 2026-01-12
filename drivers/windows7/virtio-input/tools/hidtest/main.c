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
#include <stdio.h>
#include <stdlib.h>
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

#ifndef IOCTL_HID_SET_OUTPUT_REPORT
// IOCTL_HID_SET_OUTPUT_REPORT is typically HID_CTL_CODE(0x0B) (METHOD_NEITHER).
#define IOCTL_HID_SET_OUTPUT_REPORT HID_CTL_CODE(0x0B)
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
// Legacy/alternate product IDs (e.g. older builds that reused the PCI virtio IDs).
#define VIRTIO_INPUT_PID_MODERN 0x1052
#define VIRTIO_INPUT_PID_TRANSITIONAL 0x1011

// Current Aero virtio-input Win7 driver exposes *separate* keyboard/mouse HID
// devices, each with its own report descriptor.
#define VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN 65
#define VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN 54
#define VIRTIO_INPUT_EXPECTED_KBD_INPUT_LEN 9
#define VIRTIO_INPUT_EXPECTED_KBD_OUTPUT_LEN 2
#define VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN 5

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
    int have_vid;
    int have_pid;
    int have_index;
    int have_led_mask;
    int led_via_hidd;
    int have_led_ioctl_set_output;
    int led_cycle;
    int ioctl_bad_xfer_packet;
    int ioctl_bad_write_report;
    int ioctl_bad_set_output_xfer_packet;
    int ioctl_bad_set_output_report;
    int ioctl_bad_get_report_descriptor;
    int ioctl_bad_get_device_descriptor;
    int ioctl_bad_get_string;
    int ioctl_bad_get_indexed_string;
    int hidd_bad_set_output_report;
    int dump_desc;
    int want_keyboard;
    int want_mouse;
    USHORT vid;
    USHORT pid;
    DWORD index;
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

static void print_last_error_w(const wchar_t *prefix)
{
    print_win32_error_w(prefix, GetLastError());
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

    if (len == 0) {
        wprintf(L"mouse: <empty>\n");
        return;
    }

    // Common layouts:
    // - Boot mouse: 3 bytes (no ReportID) => [btn][x][y]
    // - Wheel mouse: 4 bytes              => [btn][x][y][wheel]
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
    if (len >= off + 4) {
        wheel = (char)buf[off + 3];
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
    wprintf(L"\n");
}

static void print_usage(void)
{
    wprintf(L"hidtest: minimal HID report/IOCTL probe tool (Win7)\n");
    wprintf(L"\n");
    wprintf(L"Usage:\n");
    wprintf(L"  hidtest.exe [--list]\n");
    wprintf(L"  hidtest.exe [--keyboard|--mouse] [--index N] [--vid 0x1234] [--pid 0x5678]\n");
    wprintf(L"             [--led 0x07 | --led-hidd 0x07 | --led-cycle] [--dump-desc]\n");
    wprintf(L"             [--led-ioctl-set-output 0x07]\n");
    wprintf(L"             [--ioctl-bad-xfer-packet | --ioctl-bad-write-report]\n");
    wprintf(L"             [--ioctl-bad-set-output-xfer-packet | --ioctl-bad-set-output-report | --hidd-bad-set-output-report]\n");
    wprintf(L"             [--ioctl-bad-get-report-descriptor | --ioctl-bad-get-device-descriptor |\n");
    wprintf(L"              --ioctl-bad-get-string | --ioctl-bad-get-indexed-string]\n");
    wprintf(L"\n");
    wprintf(L"Options:\n");
    wprintf(L"  --list          List all present HID interfaces and exit\n");
    wprintf(L"  --keyboard      Prefer/select the keyboard top-level collection (Usage=Keyboard)\n");
    wprintf(L"  --mouse         Prefer/select the mouse top-level collection (Usage=Mouse)\n");
    wprintf(L"  --index N       Open HID interface at enumeration index N\n");
    wprintf(L"  --vid 0xVID     Filter by vendor ID (hex)\n");
    wprintf(L"  --pid 0xPID     Filter by product ID (hex)\n");
    wprintf(L"  --led 0xMASK    Send keyboard LED output report (ReportID=1)\n");
    wprintf(L"                 Bits: 0x01 NumLock, 0x02 CapsLock, 0x04 ScrollLock\n");
    wprintf(L"  --led-hidd 0xMASK\n");
    wprintf(L"                 Send keyboard LEDs using HidD_SetOutputReport (exercises IOCTL_HID_SET_OUTPUT_REPORT)\n");
    wprintf(L"  --led-ioctl-set-output 0xMASK\n");
    wprintf(L"                 Send keyboard LEDs using DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT)\n");
    wprintf(L"  --led-cycle     Cycle keyboard LEDs to visually confirm write path\n");
    wprintf(L"  --dump-desc     Print the raw HID report descriptor bytes\n");
    wprintf(L"  --ioctl-bad-xfer-packet\n");
    wprintf(L"                 Send IOCTL_HID_WRITE_REPORT with an invalid HID_XFER_PACKET pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-write-report\n");
    wprintf(L"                 Send IOCTL_HID_WRITE_REPORT with an invalid reportBuffer pointer\n");
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
    wprintf(L"  --ioctl-bad-get-device-descriptor\n");
    wprintf(L"                 Send IOCTL_HID_GET_DEVICE_DESCRIPTOR with an invalid output buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-string\n");
    wprintf(L"                 Send IOCTL_HID_GET_STRING with an invalid input buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --ioctl-bad-get-indexed-string\n");
    wprintf(L"                 Send IOCTL_HID_GET_INDEXED_STRING with an invalid input buffer pointer\n");
    wprintf(L"                 (negative test for METHOD_NEITHER hardening; should fail, no crash)\n");
    wprintf(L"  --hidd-bad-set-output-report\n");
    wprintf(L"                 Call HidD_SetOutputReport with an invalid buffer pointer\n");
    wprintf(L"                 (negative test for IOCTL_HID_SET_OUTPUT_REPORT path; should fail, no crash)\n");
    wprintf(L"\n");
    wprintf(L"Notes:\n");
    wprintf(L"  - virtio-input detection: VID 0x1AF4, PID 0x0001 (keyboard) / 0x0002 (mouse)\n");
    wprintf(L"    (legacy/alternate PIDs: 0x1052 / 0x1011).\n");
    wprintf(L"  - Without filters, the tool prefers a virtio-input keyboard interface.\n");
    wprintf(L"  - Press Ctrl+C to exit the report read loop.\n");
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
        print_last_error_w(L"SetupDiGetClassDevs");
        return 0;
    }

    iface_index = 0;
    have_hard_filters = opt->have_index || opt->have_vid || opt->have_pid;
    have_usage_filter = opt->want_keyboard || opt->want_mouse;
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

        ZeroMemory(&iface, sizeof(iface));
        iface.cbSize = sizeof(iface);
        if (!SetupDiEnumDeviceInterfaces(devinfo, NULL, &hid_guid, iface_index, &iface)) {
            DWORD err = GetLastError();
            if (err != ERROR_NO_MORE_ITEMS) {
                print_win32_error_w(L"SetupDiEnumDeviceInterfaces", err);
            }
            break;
        }

        SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, NULL, 0, &required, NULL);
        if (required == 0) {
            wprintf(L"[%lu] SetupDiGetDeviceInterfaceDetail: required size=0\n", iface_index);
            iface_index++;
            continue;
        }

        detail = (PSP_DEVICE_INTERFACE_DETAIL_DATA_W)malloc(required);
        if (detail == NULL) {
            wprintf(L"Out of memory\n");
            SetupDiDestroyDeviceInfoList(devinfo);
            return 0;
        }

        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, required, NULL, NULL)) {
            wprintf(L"[%lu] SetupDiGetDeviceInterfaceDetail failed\n", iface_index);
            print_last_error_w(L"SetupDiGetDeviceInterfaceDetail");
            free(detail);
            iface_index++;
            continue;
        }

        handle = open_hid_path(detail->DevicePath, &desired_access);
        if (handle == INVALID_HANDLE_VALUE) {
            wprintf(L"[%lu] %ls\n", iface_index, detail->DevicePath);
            print_last_error_w(L"      CreateFile");
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

        is_keyboard = caps_valid && caps.UsagePage == 0x01 && caps.Usage == 0x06;
        is_mouse = caps_valid && caps.UsagePage == 0x01 && caps.Usage == 0x02;

        if (is_keyboard) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (is_mouse) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (attr_valid && attr.ProductID == VIRTIO_INPUT_PID_KEYBOARD) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_KBD_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        } else if (attr_valid && attr.ProductID == VIRTIO_INPUT_PID_MOUSE) {
            virtio_expected_desc_len = VIRTIO_INPUT_EXPECTED_MOUSE_REPORT_DESC_LEN;
            virtio_expected_desc_valid = 1;
        }

        if (is_virtio) {
            if (is_keyboard) {
                wprintf(L"      Detected: virtio-input keyboard\n");
            } else if (is_mouse) {
                wprintf(L"      Detected: virtio-input mouse\n");
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
            } else if (caps_valid && is_mouse) {
                if (caps.InputReportByteLength != VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN) {
                    wprintf(L"      [WARN] unexpected virtio-input mouse input report length (expected %u)\n",
                            (unsigned)VIRTIO_INPUT_EXPECTED_MOUSE_INPUT_LEN);
                }
            }
        }

        if (desired_access & GENERIC_WRITE) {
            wprintf(L"      Access: read/write\n");
        } else {
            wprintf(L"      Access: read-only\n");
        }

        print_device_strings(handle);

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

        if (opt->list_only) {
            CloseHandle(handle);
            free(detail);
            iface_index++;
            continue;
        }

        // Selection rules:
        // - With hard filters (--index/--vid/--pid): pick the first match.
        // - With only usage filters (--keyboard/--mouse): prefer a matching virtio interface,
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
    HID_XFER_PACKET_MIN pkt;
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

    ZeroMemory(&pkt, sizeof(pkt));
    pkt.reportId = 1;
    pkt.reportBuffer = report;
    pkt.reportBufferLen = (ULONG)sizeof(report);

    wprintf(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT) keyboard LEDs: ");
    dump_hex(report, (DWORD)sizeof(report));
    wprintf(L"\n");

    ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, &pkt, (DWORD)sizeof(pkt), NULL, 0, &bytes, NULL);
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
    static const BYTE seq[] = {0x00, 0x01, 0x00, 0x02, 0x00, 0x04, 0x00, 0x07, 0x00};
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

static void read_reports_loop(const SELECTED_DEVICE *dev)
{
    BYTE *buf;
    DWORD buf_len;
    DWORD n;
    BOOL ok;
    DWORD seq = 0;
    int is_virtio = dev->attr_valid && is_virtio_input_device(&dev->attr);

    if (!dev->caps_valid) {
        wprintf(L"Cannot read reports: HID caps not available.\n");
        return;
    }

    buf_len = dev->caps.InputReportByteLength;
    if (buf_len == 0) {
        buf_len = 64;
    }

    buf = (BYTE *)malloc(buf_len);
    if (buf == NULL) {
        wprintf(L"Out of memory\n");
        return;
    }

    wprintf(L"\nReading input reports (%lu bytes)...\n", buf_len);
    for (;;) {
        ZeroMemory(buf, buf_len);
        n = 0;
        ok = ReadFile(dev->handle, buf, buf_len, &n, NULL);
        if (!ok) {
            print_last_error_w(L"ReadFile(IOCTL_HID_READ_REPORT)");
            break;
        }

        wprintf(L"[%lu] %lu bytes: ", seq, n);
        dump_hex(buf, n);
        wprintf(L"\n");

        // Best-effort decode:
        // - For virtio-input, use ReportID (byte 0) since report IDs are stable.
        // - Otherwise fall back to top-level usage heuristics.
        if (is_virtio && n > 0) {
            if (buf[0] == 1) {
                dump_keyboard_report(buf, n);
            } else if (buf[0] == 2) {
                dump_mouse_report(buf, n, 1);
            }
        } else {
            if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x06) {
                dump_keyboard_report(buf, n);
            } else if (dev->caps.UsagePage == 0x01 && dev->caps.Usage == 0x02) {
                dump_mouse_report(buf, n, 0);
            }
        }

        seq++;
    }

    free(buf);
}

static void ioctl_bad_write_report(const SELECTED_DEVICE *dev)
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
        return;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_WRITE_REPORT\n");
        return;
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
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_WRITE_REPORT bad reportBuffer)");
}

static void ioctl_bad_xfer_packet(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_WRITE_REPORT\n");
        return;
    }

    wprintf(L"\nIssuing IOCTL_HID_WRITE_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_WRITE_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_WRITE_REPORT bad HID_XFER_PACKET)");
}

static void ioctl_bad_set_output_xfer_packet(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_SET_OUTPUT_REPORT\n");
        return;
    }

    wprintf(L"\nIssuing IOCTL_HID_SET_OUTPUT_REPORT with invalid HID_XFER_PACKET pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_SET_OUTPUT_REPORT, (PVOID)(ULONG_PTR)0x1, 64, NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT bad HID_XFER_PACKET)");
}

static void ioctl_bad_set_output_report(const SELECTED_DEVICE *dev)
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
        return;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot issue IOCTL_HID_SET_OUTPUT_REPORT\n");
        return;
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
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_SET_OUTPUT_REPORT bad reportBuffer)");
}

static void ioctl_bad_get_report_descriptor(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
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
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_REPORT_DESCRIPTOR bad output buffer)");
}

static void ioctl_bad_get_device_descriptor(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_DEVICE_DESCRIPTOR with invalid output buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_DEVICE_DESCRIPTOR, NULL, 0, (PVOID)(ULONG_PTR)0x1, 256, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_DEVICE_DESCRIPTOR bad output buffer)");
}

static void ioctl_bad_get_string(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_STRING with invalid input buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_STRING, (PVOID)(ULONG_PTR)0x1, sizeof(ULONG), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_STRING bad input buffer)");
}

static void ioctl_bad_get_indexed_string(const SELECTED_DEVICE *dev)
{
    DWORD bytes = 0;
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    wprintf(L"\nIssuing IOCTL_HID_GET_INDEXED_STRING with invalid input buffer pointer...\n");
    ok = DeviceIoControl(dev->handle, IOCTL_HID_GET_INDEXED_STRING, (PVOID)(ULONG_PTR)0x1, sizeof(ULONG), NULL, 0, &bytes, NULL);
    if (ok) {
        wprintf(L"Unexpected success (bytes=%lu)\n", bytes);
        return;
    }

    print_last_error_w(L"DeviceIoControl(IOCTL_HID_GET_INDEXED_STRING bad input buffer)");
}

static void hidd_bad_set_output_report(const SELECTED_DEVICE *dev)
{
    BOOL ok;

    if (dev == NULL || dev->handle == INVALID_HANDLE_VALUE) {
        wprintf(L"Invalid device handle\n");
        return;
    }

    if ((dev->desired_access & GENERIC_WRITE) == 0) {
        wprintf(L"Device was not opened with GENERIC_WRITE; cannot call HidD_SetOutputReport\n");
        return;
    }

    wprintf(L"\nCalling HidD_SetOutputReport with invalid buffer pointer...\n");
    ok = HidD_SetOutputReport(dev->handle, (PVOID)(ULONG_PTR)0x1, 2);
    if (ok) {
        wprintf(L"Unexpected success\n");
        return;
    }

    print_last_error_w(L"HidD_SetOutputReport (bad buffer)");
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

        if (wcscmp(argv[i], L"--keyboard") == 0) {
            opt.want_keyboard = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--mouse") == 0) {
            opt.want_mouse = 1;
            continue;
        }

        if (wcscmp(argv[i], L"--dump-desc") == 0) {
            opt.dump_desc = 1;
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

        wprintf(L"Unknown argument: %ls\n", argv[i]);
        print_usage();
        return 2;
    }

    if (opt.want_keyboard && opt.want_mouse) {
        wprintf(L"--keyboard and --mouse are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.led_cycle) {
        wprintf(L"--led/--led-hidd/--led-ioctl-set-output and --led-cycle are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_write_report) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-write-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_report_descriptor) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-get-report-descriptor are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_device_descriptor) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-get-device-descriptor are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_string) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-get-string are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_get_indexed_string) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-get-indexed-string are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_xfer_packet) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.hidd_bad_set_output_report) {
        wprintf(L"--led/--led-hidd and --hidd-bad-set-output-report are mutually exclusive.\n");
        return 2;
    }
    if (opt.ioctl_bad_xfer_packet && opt.ioctl_bad_write_report) {
        wprintf(L"--ioctl-bad-xfer-packet and --ioctl-bad-write-report are mutually exclusive.\n");
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
        wprintf(L"--led/--led-hidd and --ioctl-bad-set-output-xfer-packet are mutually exclusive.\n");
        return 2;
    }
    if (opt.have_led_mask && opt.ioctl_bad_set_output_report) {
        wprintf(L"--led/--led-hidd and --ioctl-bad-set-output-report are mutually exclusive.\n");
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
    if ((opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string ||
         opt.ioctl_bad_get_indexed_string) &&
        (opt.ioctl_bad_xfer_packet || opt.ioctl_bad_write_report || opt.ioctl_bad_set_output_xfer_packet || opt.ioctl_bad_set_output_report ||
         opt.hidd_bad_set_output_report)) {
        wprintf(L"Descriptor/string negative tests are mutually exclusive with output-report negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_report_descriptor &&
        (opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string)) {
        wprintf(L"--ioctl-bad-get-report-descriptor is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_device_descriptor &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_string || opt.ioctl_bad_get_indexed_string)) {
        wprintf(L"--ioctl-bad-get-device-descriptor is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_string &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_indexed_string)) {
        wprintf(L"--ioctl-bad-get-string is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }
    if (opt.ioctl_bad_get_indexed_string &&
        (opt.ioctl_bad_get_report_descriptor || opt.ioctl_bad_get_device_descriptor || opt.ioctl_bad_get_string)) {
        wprintf(L"--ioctl-bad-get-indexed-string is mutually exclusive with other descriptor/string negative tests.\n");
        return 2;
    }

    if (!enumerate_hid_devices(&opt, &dev)) {
        wprintf(L"No matching HID devices found.\n");
        return 1;
    }

    if (opt.list_only) {
        return 0;
    }

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

    if (opt.ioctl_bad_write_report) {
        ioctl_bad_write_report(&dev);
        free_selected_device(&dev);
        return 0;
    }

    if (opt.hidd_bad_set_output_report) {
        hidd_bad_set_output_report(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_xfer_packet) {
        ioctl_bad_xfer_packet(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_set_output_xfer_packet) {
        ioctl_bad_set_output_xfer_packet(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_set_output_report) {
        ioctl_bad_set_output_report(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_get_report_descriptor) {
        ioctl_bad_get_report_descriptor(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_get_device_descriptor) {
        ioctl_bad_get_device_descriptor(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_get_string) {
        ioctl_bad_get_string(&dev);
        free_selected_device(&dev);
        return 0;
    }
    if (opt.ioctl_bad_get_indexed_string) {
        ioctl_bad_get_indexed_string(&dev);
        free_selected_device(&dev);
        return 0;
    }

    read_reports_loop(&dev);
    free_selected_device(&dev);
    return 0;
}
