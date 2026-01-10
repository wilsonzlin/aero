// SPDX-License-Identifier: MIT OR Apache-2.0
//
// hidtest: small Windows 7-compatible user-mode HID verification tool.
//
// This tool is intentionally self-contained and only depends on Windows SDK
// headers + libraries (SetupAPI + HID). It can:
//   - Enumerate present HID devices (GUID_DEVINTERFACE_HID)
//   - Print basic HID information (VID/PID, usage, report lengths)
//   - Listen for input reports and decode common keyboard/mouse reports
//   - Send a keyboard LED output report (Num/Caps/Scroll) when supported

#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <hidsdi.h>
#include <hidpi.h>
#include <setupapi.h>

#include <stdint.h>
#include <stdio.h>

#include <algorithm>
#include <set>
#include <string>
#include <vector>

#pragma comment(lib, "hid.lib")
#pragma comment(lib, "setupapi.lib")

#ifndef HID_REPORT_DESCRIPTOR_TYPE
// HID descriptor "Report" type as per HID 1.11 / USB HID.
#define HID_REPORT_DESCRIPTOR_TYPE 0x22
#endif

static volatile LONG g_stop = 0;

static BOOL WINAPI ConsoleCtrlHandler(DWORD ctrlType) {
  switch (ctrlType) {
    case CTRL_C_EVENT:
    case CTRL_BREAK_EVENT:
    case CTRL_CLOSE_EVENT:
    case CTRL_SHUTDOWN_EVENT:
      InterlockedExchange(&g_stop, 1);
      return TRUE;
    default:
      return FALSE;
  }
}

static void PrintWin32Error(const wchar_t *context) {
  DWORD err = GetLastError();
  wchar_t *msg = NULL;
  DWORD flags =
      FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS;
  DWORD len = FormatMessageW(flags, NULL, err, 0, (LPWSTR)&msg, 0, NULL);
  if (len && msg) {
    while (len && (msg[len - 1] == L'\r' || msg[len - 1] == L'\n')) {
      msg[--len] = 0;
    }
    wprintf(L"%ls: error %lu (%ls)\n", context, err, msg);
    LocalFree(msg);
    return;
  }
  wprintf(L"%ls: error %lu\n", context, err);
}

static void PrintHex(const uint8_t *buf, size_t len) {
  for (size_t i = 0; i < len; ++i) {
    wprintf(L"%02X", buf[i]);
    if (i + 1 != len) {
      wprintf(L" ");
    }
  }
}

static const wchar_t *UsagePageName(USAGE page) {
  switch (page) {
    case 0x01:
      return L"GenericDesktop";
    case 0x07:
      return L"Keyboard";
    case 0x08:
      return L"LED";
    case 0x09:
      return L"Button";
    case 0x0C:
      return L"Consumer";
    default:
      return NULL;
  }
}

static const wchar_t *GenericDesktopUsageName(USAGE usage) {
  switch (usage) {
    case 0x02:
      return L"Mouse";
    case 0x04:
      return L"Joystick";
    case 0x05:
      return L"GamePad";
    case 0x06:
      return L"Keyboard";
    case 0x07:
      return L"Keypad";
    case 0x08:
      return L"Multi-axis Controller";
    default:
      return NULL;
  }
}

static const wchar_t *KeyboardUsageName(uint8_t usage) {
  switch (usage) {
    case 0x04:
      return L"A";
    case 0x05:
      return L"B";
    case 0x06:
      return L"C";
    case 0x07:
      return L"D";
    case 0x08:
      return L"E";
    case 0x09:
      return L"F";
    case 0x0A:
      return L"G";
    case 0x0B:
      return L"H";
    case 0x0C:
      return L"I";
    case 0x0D:
      return L"J";
    case 0x0E:
      return L"K";
    case 0x0F:
      return L"L";
    case 0x10:
      return L"M";
    case 0x11:
      return L"N";
    case 0x12:
      return L"O";
    case 0x13:
      return L"P";
    case 0x14:
      return L"Q";
    case 0x15:
      return L"R";
    case 0x16:
      return L"S";
    case 0x17:
      return L"T";
    case 0x18:
      return L"U";
    case 0x19:
      return L"V";
    case 0x1A:
      return L"W";
    case 0x1B:
      return L"X";
    case 0x1C:
      return L"Y";
    case 0x1D:
      return L"Z";
    case 0x1E:
      return L"1";
    case 0x1F:
      return L"2";
    case 0x20:
      return L"3";
    case 0x21:
      return L"4";
    case 0x22:
      return L"5";
    case 0x23:
      return L"6";
    case 0x24:
      return L"7";
    case 0x25:
      return L"8";
    case 0x26:
      return L"9";
    case 0x27:
      return L"0";
    case 0x28:
      return L"Enter";
    case 0x29:
      return L"Esc";
    case 0x2A:
      return L"Backspace";
    case 0x2B:
      return L"Tab";
    case 0x2C:
      return L"Space";
    case 0x39:
      return L"CapsLock";
    case 0x4F:
      return L"Right";
    case 0x50:
      return L"Left";
    case 0x51:
      return L"Down";
    case 0x52:
      return L"Up";
    default:
      return NULL;
  }
}

static std::vector<std::wstring> EnumerateHidDevicePaths() {
  GUID hidGuid;
  HidD_GetHidGuid(&hidGuid);

  HDEVINFO devs =
      SetupDiGetClassDevsW(&hidGuid, NULL, NULL, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devs == INVALID_HANDLE_VALUE) {
    PrintWin32Error(L"SetupDiGetClassDevsW");
    return {};
  }

  std::vector<std::wstring> paths;
  for (DWORD index = 0;; ++index) {
    SP_DEVICE_INTERFACE_DATA ifData = {};
    ifData.cbSize = sizeof(ifData);

    if (!SetupDiEnumDeviceInterfaces(devs, NULL, &hidGuid, index, &ifData)) {
      DWORD err = GetLastError();
      if (err == ERROR_NO_MORE_ITEMS) {
        break;
      }
      SetLastError(err);
      PrintWin32Error(L"SetupDiEnumDeviceInterfaces");
      continue;
    }

    DWORD requiredSize = 0;
    SetupDiGetDeviceInterfaceDetailW(devs, &ifData, NULL, 0, &requiredSize, NULL);
    if (GetLastError() != ERROR_INSUFFICIENT_BUFFER || requiredSize == 0) {
      PrintWin32Error(L"SetupDiGetDeviceInterfaceDetailW(size query)");
      continue;
    }

    std::vector<uint8_t> detailBuf(requiredSize);
    SP_DEVICE_INTERFACE_DETAIL_DATA_W *detail =
        reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W *>(detailBuf.data());
    detail->cbSize = sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W);

    if (!SetupDiGetDeviceInterfaceDetailW(devs, &ifData, detail, requiredSize, NULL, NULL)) {
      PrintWin32Error(L"SetupDiGetDeviceInterfaceDetailW");
      continue;
    }

    paths.emplace_back(detail->DevicePath);
  }

  SetupDiDestroyDeviceInfoList(devs);
  return paths;
}

struct HidInfo {
  bool opened = false;
  bool hasAttributes = false;
  bool hasCaps = false;
  bool hasHidDescriptor = false;

  USHORT vid = 0;
  USHORT pid = 0;
  USHORT version = 0;

  USAGE usagePage = 0;
  USAGE usage = 0;
  USHORT inputReportLen = 0;
  USHORT outputReportLen = 0;
  USHORT featureReportLen = 0;

  USHORT reportDescriptorLen = 0;
};

static HANDLE OpenHidDevice(const std::wstring &path, bool wantWrite, bool overlapped,
                            bool *outWriteOpened) {
  if (outWriteOpened) {
    *outWriteOpened = false;
  }

  DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  DWORD flags = FILE_ATTRIBUTE_NORMAL | (overlapped ? FILE_FLAG_OVERLAPPED : 0);

  DWORD access = GENERIC_READ | (wantWrite ? GENERIC_WRITE : 0);
  HANDLE h = CreateFileW(path.c_str(), access, share, NULL, OPEN_EXISTING, flags, NULL);
  if (h != INVALID_HANDLE_VALUE) {
    if (outWriteOpened) {
      *outWriteOpened = wantWrite;
    }
    return h;
  }

  if (!wantWrite) {
    return INVALID_HANDLE_VALUE;
  }

  // Some HID devices cannot be opened with GENERIC_WRITE; fall back to read-only for info/listen.
  access = GENERIC_READ;
  h = CreateFileW(path.c_str(), access, share, NULL, OPEN_EXISTING, flags, NULL);
  if (h != INVALID_HANDLE_VALUE) {
    return h;
  }

  return INVALID_HANDLE_VALUE;
}

static HidInfo QueryHidInfo(HANDLE h) {
  HidInfo info;
  info.opened = (h != INVALID_HANDLE_VALUE);
  if (!info.opened) {
    return info;
  }

  HIDD_ATTRIBUTES attr = {};
  attr.Size = sizeof(attr);
  if (HidD_GetAttributes(h, &attr)) {
    info.hasAttributes = true;
    info.vid = attr.VendorID;
    info.pid = attr.ProductID;
    info.version = attr.VersionNumber;
  }

  PHIDP_PREPARSED_DATA ppd = NULL;
  if (HidD_GetPreparsedData(h, &ppd) && ppd) {
    HIDP_CAPS caps = {};
    if (HidP_GetCaps(ppd, &caps) == HIDP_STATUS_SUCCESS) {
      info.hasCaps = true;
      info.usagePage = caps.UsagePage;
      info.usage = caps.Usage;
      info.inputReportLen = caps.InputReportByteLength;
      info.outputReportLen = caps.OutputReportByteLength;
      info.featureReportLen = caps.FeatureReportByteLength;
    }
    HidD_FreePreparsedData(ppd);
  }

  uint8_t descBuf[256] = {};
  if (HidD_GetHidDescriptor(h, reinterpret_cast<PHID_DESCRIPTOR>(descBuf),
                           static_cast<ULONG>(sizeof(descBuf)))) {
    info.hasHidDescriptor = true;
    const HID_DESCRIPTOR *desc = reinterpret_cast<const HID_DESCRIPTOR *>(descBuf);
    for (UCHAR i = 0; i < desc->bNumDescriptors; ++i) {
      if (desc->DescriptorList[i].bReportType == HID_REPORT_DESCRIPTOR_TYPE) {
        info.reportDescriptorLen = desc->DescriptorList[i].wReportLength;
      }
    }
  }

  return info;
}

static void PrintHidInfo(const std::wstring &path, size_t index, const HidInfo &info) {
  // MSVC before VS2015 does not support %zu; %Iu works for size_t on Windows.
  wprintf(L"[%Iu]\n", index);
  wprintf(L"  Path: %ls\n", path.c_str());

  if (info.hasAttributes) {
    wprintf(L"  VID:PID: %04X:%04X (ver 0x%04X)\n", info.vid, info.pid, info.version);
  } else {
    wprintf(L"  VID:PID: (unavailable)\n");
  }

  if (info.hasCaps) {
    const wchar_t *pageName = UsagePageName(info.usagePage);
    const wchar_t *usageName = NULL;
    if (info.usagePage == 0x01) {
      usageName = GenericDesktopUsageName(info.usage);
    }

    if (pageName || usageName) {
      wprintf(L"  Usage: 0x%04X/0x%04X (%ls/%ls)\n", info.usagePage, info.usage,
              pageName ? pageName : L"?", usageName ? usageName : L"?");
    } else {
      wprintf(L"  Usage: 0x%04X/0x%04X\n", info.usagePage, info.usage);
    }

    wprintf(L"  Report lengths: input=%u output=%u feature=%u\n", info.inputReportLen,
            info.outputReportLen, info.featureReportLen);
  } else {
    wprintf(L"  Usage: (unavailable)\n");
    wprintf(L"  Report lengths: (unavailable)\n");
  }

  if (info.hasHidDescriptor) {
    wprintf(L"  Report descriptor length: %u\n", info.reportDescriptorLen);
  } else {
    wprintf(L"  Report descriptor length: (unavailable)\n");
  }
}

static void PrintUsage(const wchar_t *argv0) {
  wprintf(L"Usage:\n");
  wprintf(L"  %ls list\n", argv0);
  wprintf(L"  %ls listen <index>\n", argv0);
  wprintf(L"  %ls setleds <index> <mask>\n", argv0);
  wprintf(L"\n");
  wprintf(L"Commands:\n");
  wprintf(L"  list                 Enumerate HID devices.\n");
  wprintf(L"  listen <index>        Read input reports and decode keyboard/mouse.\n");
  wprintf(L"  setleds <index> <mask>  Send keyboard LED output report (if supported).\n");
  wprintf(L"                         mask bits: 0x01=NumLock 0x02=CapsLock 0x04=ScrollLock\n");
}

static bool ParseUlong(const wchar_t *s, unsigned long *out) {
  if (!s || !*s) {
    return false;
  }
  wchar_t *end = NULL;
  SetLastError(0);
  unsigned long v = wcstoul(s, &end, 0);
  if (GetLastError() != 0 || end == s || (end && *end)) {
    return false;
  }
  *out = v;
  return true;
}

struct KeyboardState {
  uint8_t prevMods = 0;
  std::set<uint8_t> prevKeys;
};

static void PrintModifierEvent(uint8_t bit, bool down) {
  static const wchar_t *kModNames[8] = {
      L"LCTRL", L"LSHIFT", L"LALT", L"LGUI", L"RCTRL", L"RSHIFT", L"RALT", L"RGUI",
  };
  if (bit >= 8) {
    return;
  }
  wprintf(L"kbd: mod %ls %ls\n", kModNames[bit], down ? L"down" : L"up");
}

static void PrintKeyEvent(uint8_t usage, bool down) {
  const wchar_t *name = KeyboardUsageName(usage);
  if (name) {
    wprintf(L"kbd: key %ls (0x%02X) %ls\n", name, usage, down ? L"down" : L"up");
  } else {
    wprintf(L"kbd: key 0x%02X %ls\n", usage, down ? L"down" : L"up");
  }
}

static bool DecodeBootKeyboardReport(const uint8_t *buf, size_t len, uint8_t *modsOut,
                                    uint8_t keysOut[6]) {
  // Common keyboard input report: [report_id?] [mods] [reserved] [key0..key5]
  // "reserved" is typically 0.
  if (len < 8) {
    return false;
  }

  size_t offset = 0;
  if (len >= 9) {
    // Prefer treating byte 0 as report ID when length allows.
    offset = 1;
  }
  if (len < offset + 8) {
    return false;
  }

  uint8_t mods = buf[offset + 0];
  uint8_t reserved = buf[offset + 1];
  (void)reserved;

  // If we assumed a report ID but reserved byte isn't 0, fall back to "no report ID".
  if (offset == 1 && reserved != 0 && len >= 8) {
    offset = 0;
    mods = buf[offset + 0];
    reserved = buf[offset + 1];
  }

  if (len < offset + 8) {
    return false;
  }

  *modsOut = mods;
  for (int i = 0; i < 6; ++i) {
    keysOut[i] = buf[offset + 2 + i];
  }
  return true;
}

static void HandleKeyboardReport(const uint8_t *buf, size_t len, KeyboardState *state) {
  uint8_t mods = 0;
  uint8_t keys[6] = {};
  if (!DecodeBootKeyboardReport(buf, len, &mods, keys)) {
    wprintf(L"kbd: (unrecognized report) raw=");
    PrintHex(buf, len);
    wprintf(L"\n");
    return;
  }

  uint8_t modChanged = static_cast<uint8_t>(mods ^ state->prevMods);
  for (uint8_t bit = 0; bit < 8; ++bit) {
    if (modChanged & (1u << bit)) {
      PrintModifierEvent(bit, (mods & (1u << bit)) != 0);
    }
  }

  std::set<uint8_t> curKeys;
  for (int i = 0; i < 6; ++i) {
    if (keys[i] != 0) {
      curKeys.insert(keys[i]);
    }
  }

  for (uint8_t k : curKeys) {
    if (state->prevKeys.find(k) == state->prevKeys.end()) {
      PrintKeyEvent(k, true);
    }
  }
  for (uint8_t k : state->prevKeys) {
    if (curKeys.find(k) == curKeys.end()) {
      PrintKeyEvent(k, false);
    }
  }

  state->prevMods = mods;
  state->prevKeys = curKeys;
}

struct MouseState {
  uint8_t prevButtons = 0;
};

static void PrintMouseButtonEvent(uint8_t bit, bool down) {
  const wchar_t *name = NULL;
  switch (bit) {
    case 0:
      name = L"left";
      break;
    case 1:
      name = L"right";
      break;
    case 2:
      name = L"middle";
      break;
    case 3:
      name = L"button4";
      break;
    case 4:
      name = L"button5";
      break;
    default:
      name = L"button";
      break;
  }
  wprintf(L"mouse: %ls %ls\n", name, down ? L"down" : L"up");
}

static void HandleMouseReport(const uint8_t *buf, size_t len, bool hasWheel, MouseState *state) {
  // Common mouse input report: [report_id?] [buttons] [x] [y] [wheel?]
  const size_t dataLen = hasWheel ? 4u : 3u;
  if (len < dataLen) {
    wprintf(L"mouse: (short report) raw=");
    PrintHex(buf, len);
    wprintf(L"\n");
    return;
  }

  size_t offset = 0;
  if (len >= dataLen + 1) {
    offset = 1;
  }
  if (len < offset + dataLen) {
    wprintf(L"mouse: (unexpected report length) raw=");
    PrintHex(buf, len);
    wprintf(L"\n");
    return;
  }

  uint8_t buttons = buf[offset + 0];
  int x = static_cast<int8_t>(buf[offset + 1]);
  int y = static_cast<int8_t>(buf[offset + 2]);
  int wheel = 0;
  if (hasWheel && len >= offset + 4) {
    wheel = static_cast<int8_t>(buf[offset + 3]);
  }

  uint8_t changed = static_cast<uint8_t>(buttons ^ state->prevButtons);
  for (uint8_t bit = 0; bit < 5; ++bit) {
    if (changed & (1u << bit)) {
      PrintMouseButtonEvent(bit, (buttons & (1u << bit)) != 0);
    }
  }
  state->prevButtons = buttons;

  // Print movement/wheel as a single line (even when zero if there were button changes).
  if (x != 0 || y != 0 || wheel != 0 || changed != 0) {
    if (hasWheel) {
      wprintf(L"mouse: buttons=0x%02X x=%d y=%d wheel=%d\n", buttons, x, y, wheel);
    } else {
      wprintf(L"mouse: buttons=0x%02X x=%d y=%d\n", buttons, x, y);
    }
  }
}

static bool MouseHasWheel(PHIDP_PREPARSED_DATA ppd, const HIDP_CAPS &caps) {
  USHORT valueCapsLength = caps.NumberInputValueCaps;
  if (valueCapsLength == 0) {
    return false;
  }

  std::vector<HIDP_VALUE_CAPS> valueCaps(valueCapsLength);
  NTSTATUS status = HidP_GetValueCaps(HidP_Input, valueCaps.data(), &valueCapsLength, ppd);
  if (status != HIDP_STATUS_SUCCESS) {
    return false;
  }

  for (USHORT i = 0; i < valueCapsLength; ++i) {
    const HIDP_VALUE_CAPS &vc = valueCaps[i];
    if (vc.UsagePage != 0x01) {
      continue;
    }

    // GenericDesktop Wheel is usage 0x38.
    if (vc.IsRange) {
      if (vc.Range.UsageMin <= 0x38 && vc.Range.UsageMax >= 0x38) {
        return true;
      }
    } else {
      if (vc.NotRange.Usage == 0x38) {
        return true;
      }
    }
  }

  return false;
}

static int CommandList() {
  std::vector<std::wstring> paths = EnumerateHidDevicePaths();
  wprintf(L"Found %Iu HID device interface(s).\n", paths.size());

  for (size_t i = 0; i < paths.size(); ++i) {
    bool writeOpened = false;
    HANDLE h = OpenHidDevice(paths[i], true, false, &writeOpened);
    HidInfo info;
    if (h == INVALID_HANDLE_VALUE) {
      info.opened = false;
    } else {
      info = QueryHidInfo(h);
      CloseHandle(h);
    }

    PrintHidInfo(paths[i], i, info);

    if (!info.opened) {
      wprintf(L"  Note: CreateFileW failed for this device.\n");
    } else if (!writeOpened) {
      wprintf(L"  Note: opened read-only (GENERIC_WRITE was denied).\n");
    }
  }

  return 0;
}

static int CommandListen(size_t index) {
  std::vector<std::wstring> paths = EnumerateHidDevicePaths();
  if (index >= paths.size()) {
    wprintf(L"Invalid index %Iu (only %Iu device(s)).\n\n", index, paths.size());
    return CommandList();
  }

  const std::wstring &path = paths[index];
  bool writeOpened = false;
  HANDLE h = OpenHidDevice(path, false, true, &writeOpened);
  if (h == INVALID_HANDLE_VALUE) {
    PrintWin32Error(L"CreateFileW");
    return 1;
  }

  HidInfo info = QueryHidInfo(h);
  PrintHidInfo(path, index, info);
  wprintf(L"\nPress Ctrl+C to stop.\n\n");

  if (!SetConsoleCtrlHandler(ConsoleCtrlHandler, TRUE)) {
    PrintWin32Error(L"SetConsoleCtrlHandler");
  }

  PHIDP_PREPARSED_DATA ppd = NULL;
  HIDP_CAPS caps = {};
  if (!HidD_GetPreparsedData(h, &ppd) || !ppd) {
    PrintWin32Error(L"HidD_GetPreparsedData");
    CloseHandle(h);
    return 1;
  }
  if (HidP_GetCaps(ppd, &caps) != HIDP_STATUS_SUCCESS) {
    wprintf(L"HidP_GetCaps failed.\n");
    HidD_FreePreparsedData(ppd);
    CloseHandle(h);
    return 1;
  }

  const bool isKeyboard = (caps.UsagePage == 0x01 && caps.Usage == 0x06);
  const bool isMouse = (caps.UsagePage == 0x01 && caps.Usage == 0x02);
  const bool mouseHasWheel = isMouse ? MouseHasWheel(ppd, caps) : false;

  KeyboardState kbdState;
  MouseState mouseState;

  std::vector<uint8_t> reportBuf(caps.InputReportByteLength ? caps.InputReportByteLength : 64);

  OVERLAPPED ov = {};
  ov.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (!ov.hEvent) {
    PrintWin32Error(L"CreateEventW");
    HidD_FreePreparsedData(ppd);
    CloseHandle(h);
    return 1;
  }

  while (!InterlockedCompareExchange(&g_stop, 0, 0)) {
    ResetEvent(ov.hEvent);

    DWORD bytesRead = 0;
    BOOL ok = ReadFile(h, reportBuf.data(), static_cast<DWORD>(reportBuf.size()), &bytesRead, &ov);
    if (!ok) {
      DWORD err = GetLastError();
      if (err != ERROR_IO_PENDING) {
        SetLastError(err);
        PrintWin32Error(L"ReadFile");
        break;
      }

      for (;;) {
        if (InterlockedCompareExchange(&g_stop, 0, 0)) {
          // Cancel this pending read started on the current thread.
          CancelIo(h);
        }
        DWORD w = WaitForSingleObject(ov.hEvent, 100);
        if (w == WAIT_OBJECT_0) {
          break;
        }
      }

      if (!GetOverlappedResult(h, &ov, &bytesRead, FALSE)) {
        DWORD resErr = GetLastError();
        if (resErr == ERROR_OPERATION_ABORTED && InterlockedCompareExchange(&g_stop, 0, 0)) {
          break;
        }
        SetLastError(resErr);
        PrintWin32Error(L"GetOverlappedResult");
        break;
      }
    }

    if (bytesRead == 0) {
      continue;
    }

    if (isKeyboard) {
      HandleKeyboardReport(reportBuf.data(), bytesRead, &kbdState);
    } else if (isMouse) {
      HandleMouseReport(reportBuf.data(), bytesRead, mouseHasWheel, &mouseState);
    } else {
      wprintf(L"hid: raw=");
      PrintHex(reportBuf.data(), bytesRead);
      wprintf(L"\n");
    }
  }

  CloseHandle(ov.hEvent);
  HidD_FreePreparsedData(ppd);
  CloseHandle(h);
  return 0;
}

static int CommandSetLeds(size_t index, uint8_t mask) {
  std::vector<std::wstring> paths = EnumerateHidDevicePaths();
  if (index >= paths.size()) {
    wprintf(L"Invalid index %Iu (only %Iu device(s)).\n\n", index, paths.size());
    return CommandList();
  }

  const std::wstring &path = paths[index];
  bool writeOpened = false;
  HANDLE h = OpenHidDevice(path, true, false, &writeOpened);
  if (h == INVALID_HANDLE_VALUE) {
    PrintWin32Error(L"CreateFileW");
    return 1;
  }
  if (!writeOpened) {
    wprintf(L"Device could not be opened with GENERIC_WRITE; cannot send output report.\n");
    CloseHandle(h);
    return 1;
  }

  PHIDP_PREPARSED_DATA ppd = NULL;
  HIDP_CAPS caps = {};
  if (!HidD_GetPreparsedData(h, &ppd) || !ppd) {
    PrintWin32Error(L"HidD_GetPreparsedData");
    CloseHandle(h);
    return 1;
  }
  if (HidP_GetCaps(ppd, &caps) != HIDP_STATUS_SUCCESS) {
    wprintf(L"HidP_GetCaps failed.\n");
    HidD_FreePreparsedData(ppd);
    CloseHandle(h);
    return 1;
  }

  wprintf(L"Sending LED output report to:\n");
  HidInfo info = QueryHidInfo(h);
  PrintHidInfo(path, index, info);

  if (caps.OutputReportByteLength == 0) {
    wprintf(L"This device exposes no output reports (OutputReportByteLength==0).\n");
    HidD_FreePreparsedData(ppd);
    CloseHandle(h);
    return 1;
  }

  std::vector<uint8_t> outReport(caps.OutputReportByteLength, 0);
  if (outReport.size() >= 2) {
    outReport[0] = 0;  // report ID (0 when not used)
    outReport[1] = mask;
  } else {
    // Fallback for unusual devices where the buffer does not include a report ID.
    outReport[0] = mask;
  }

  DWORD bytesWritten = 0;
  if (!WriteFile(h, outReport.data(), static_cast<DWORD>(outReport.size()), &bytesWritten, NULL)) {
    PrintWin32Error(L"WriteFile");
    wprintf(L"Tried writing: ");
    PrintHex(outReport.data(), outReport.size());
    wprintf(L"\n");
    HidD_FreePreparsedData(ppd);
    CloseHandle(h);
    return 1;
  }

  wprintf(L"Wrote %lu byte(s): ", bytesWritten);
  PrintHex(outReport.data(), outReport.size());
  wprintf(L"\n");

  HidD_FreePreparsedData(ppd);
  CloseHandle(h);
  return 0;
}

int wmain(int argc, wchar_t **argv) {
  const wchar_t *argv0 = (argc > 0 && argv[0]) ? argv[0] : L"hidtest";

  if (argc <= 1) {
    return CommandList();
  }

  const std::wstring cmd = argv[1];

  if (cmd == L"-h" || cmd == L"--help" || cmd == L"help") {
    PrintUsage(argv0);
    return 0;
  }

  if (cmd == L"list") {
    return CommandList();
  }

  if (cmd == L"listen") {
    if (argc < 3) {
      PrintUsage(argv0);
      return 2;
    }
    unsigned long index = 0;
    if (!ParseUlong(argv[2], &index)) {
      wprintf(L"Invalid index: %ls\n\n", argv[2]);
      PrintUsage(argv0);
      return 2;
    }
    return CommandListen(static_cast<size_t>(index));
  }

  if (cmd == L"setleds" || cmd == L"leds") {
    if (argc < 4) {
      PrintUsage(argv0);
      return 2;
    }
    unsigned long index = 0;
    unsigned long mask = 0;
    if (!ParseUlong(argv[2], &index) || !ParseUlong(argv[3], &mask) || mask > 0xFF) {
      wprintf(L"Invalid arguments.\n\n");
      PrintUsage(argv0);
      return 2;
    }
    return CommandSetLeds(static_cast<size_t>(index), static_cast<uint8_t>(mask));
  }

  wprintf(L"Unknown command: %ls\n\n", cmd.c_str());
  PrintUsage(argv0);
  return 2;
}
