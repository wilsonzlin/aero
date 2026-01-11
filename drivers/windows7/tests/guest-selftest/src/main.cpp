// SPDX-License-Identifier: MIT OR Apache-2.0
//
// aero-virtio-selftest: Windows 7 user-mode functional tests for Aero virtio drivers.
// Primary targets: virtio-blk + virtio-net + virtio-input + virtio-snd. Output is written to stdout, a log file, and
// COM1.

#include <windows.h>

#include <audioclient.h>
#include <functiondiscoverykeys_devpkey.h>
#include <mmdeviceapi.h>
#include <mmsystem.h>
#include <mmddk.h>
#include <propsys.h>
#include <setupapi.h>

#include <devguid.h>
#include <initguid.h>
#include <iphlpapi.h>
#include <ntddstor.h>
#include <winioctl.h>
#include <winhttp.h>
#include <ws2tcpip.h>

#include <algorithm>
#include <cmath>
#include <climits>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cwctype>
#include <optional>
#include <set>
#include <string>
#include <vector>

namespace {

struct Options {
  std::wstring http_url = L"http://10.0.2.2:18080/aero-virtio-selftest";
  // Prefer a hostname that (on many QEMU versions) resolves without relying on external internet.
  // If unavailable, the selftest will fall back to "example.com".
  std::wstring dns_host = L"host.lan";
  std::wstring log_file = L"C:\\aero-virtio-selftest.log";
  // Optional: override where the virtio-blk file I/O test writes its temporary file.
  // This must be a directory on a virtio-backed volume (e.g. "D:\\aero-test\\").
  // If empty, the selftest will attempt to auto-detect a mounted virtio volume.
  std::wstring blk_root;
  // Skip the virtio-snd test even if an audio device is present.
  bool disable_snd = false;
  // If set, missing virtio-snd causes the overall selftest to fail (instead of SKIP).
  bool require_snd = false;

  DWORD net_timeout_sec = 120;
  DWORD io_file_size_mib = 32;
  DWORD io_chunk_kib = 1024;
};

static std::wstring ToLower(std::wstring s) {
  std::transform(s.begin(), s.end(), s.begin(),
                 [](wchar_t c) { return static_cast<wchar_t>(towlower(c)); });
  return s;
}

static bool ContainsInsensitive(const std::wstring& haystack, const std::wstring& needle) {
  return ToLower(haystack).find(ToLower(needle)) != std::wstring::npos;
}

// Windows 7 SDKs do not consistently ship the HIDClass IOCTL definitions in user-mode headers.
// Define the subset we need (report descriptor read) locally so the selftest stays buildable with
// a plain Win7-compatible SDK toolchain.
#ifndef FILE_DEVICE_HID
// Some SDK-only environments don't define FILE_DEVICE_HID. The HID class IOCTLs are historically
// defined under device type 0x0000000B.
#define FILE_DEVICE_HID 0x0000000B
#endif
#ifndef HID_CTL_CODE
#define HID_CTL_CODE(id) CTL_CODE(FILE_DEVICE_HID, (id), METHOD_NEITHER, FILE_ANY_ACCESS)
#endif
#ifndef IOCTL_HID_GET_REPORT_DESCRIPTOR
// WDK `hidclass.h` defines IOCTL_HID_GET_REPORT_DESCRIPTOR as function code 1
// (pairs with IOCTL_HID_GET_DEVICE_DESCRIPTOR=0, IOCTL_HID_READ_REPORT=2, etc).
#define IOCTL_HID_GET_REPORT_DESCRIPTOR HID_CTL_CODE(1)
#endif

static std::wstring NormalizeGuidLikeString(std::wstring s) {
  s = ToLower(std::move(s));
  s.erase(std::remove_if(s.begin(), s.end(),
                          [](wchar_t c) { return c == L'{' || c == L'}' || c == L'\r' || c == L'\n'; }),
          s.end());
  return s;
}

static std::string WideToUtf8(const std::wstring& w) {
  if (w.empty()) return {};
  const int needed = WideCharToMultiByte(CP_UTF8, 0, w.c_str(), static_cast<int>(w.size()),
                                         nullptr, 0, nullptr, nullptr);
  if (needed <= 0) return {};
  std::string out(static_cast<size_t>(needed), '\0');
  WideCharToMultiByte(CP_UTF8, 0, w.c_str(), static_cast<int>(w.size()), out.data(), needed, nullptr,
                      nullptr);
  return out;
}

static std::wstring AnsiNToWide(const char* s, size_t len) {
  if (!s || len == 0) return L"";
  if (len > static_cast<size_t>(INT_MAX)) return L"";
  const int needed = MultiByteToWideChar(CP_ACP, 0, s, static_cast<int>(len), nullptr, 0);
  if (needed <= 0) return L"";
  std::wstring out(static_cast<size_t>(needed), L'\0');
  MultiByteToWideChar(CP_ACP, 0, s, static_cast<int>(len), out.data(), needed);
  return out;
}

static std::wstring AnsiToWide(const char* s) {
  if (!s) return L"";
  const int len = static_cast<int>(strlen(s));
  if (len == 0) return L"";
  const int needed = MultiByteToWideChar(CP_ACP, 0, s, len, nullptr, 0);
  if (needed <= 0) return L"";
  std::wstring out(static_cast<size_t>(needed), L'\0');
  MultiByteToWideChar(CP_ACP, 0, s, len, out.data(), needed);
  return out;
}

static size_t BoundedStrLen(const char* s, size_t max_len) {
  if (!s) return 0;
  size_t i = 0;
  for (; i < max_len; i++) {
    if (s[i] == '\0') break;
  }
  return i;
}

template <typename T>
class ComPtr {
 public:
  ComPtr() = default;
  ComPtr(const ComPtr&) = delete;
  ComPtr& operator=(const ComPtr&) = delete;

  ComPtr(ComPtr&& other) noexcept : ptr_(other.ptr_) { other.ptr_ = nullptr; }
  ComPtr& operator=(ComPtr&& other) noexcept {
    if (this != &other) {
      Reset();
      ptr_ = other.ptr_;
      other.ptr_ = nullptr;
    }
    return *this;
  }

  ~ComPtr() { Reset(); }

  T* Get() const { return ptr_; }
  T** Put() {
    Reset();
    return &ptr_;
  }

  T* operator->() const { return ptr_; }
  explicit operator bool() const { return ptr_ != nullptr; }

  void Reset(T* p = nullptr) {
    if (ptr_) ptr_->Release();
    ptr_ = p;
  }

 private:
  T* ptr_ = nullptr;
};

class ScopedCoInitialize {
 public:
  explicit ScopedCoInitialize(DWORD coinit) {
    hr_ = CoInitializeEx(nullptr, coinit);
    if (hr_ == RPC_E_CHANGED_MODE) {
      // The thread is already initialized with a different apartment model; keep going, but do not
      // call CoUninitialize() since we didn't successfully initialize.
      hr_ = S_OK;
      should_uninit_ = false;
      return;
    }
    should_uninit_ = SUCCEEDED(hr_);
  }

  ScopedCoInitialize(const ScopedCoInitialize&) = delete;
  ScopedCoInitialize& operator=(const ScopedCoInitialize&) = delete;

  ~ScopedCoInitialize() {
    if (should_uninit_) CoUninitialize();
  }

  HRESULT hr() const { return hr_; }

 private:
  HRESULT hr_ = E_FAIL;
  bool should_uninit_ = false;
};

class Logger {
 public:
  explicit Logger(const std::wstring& log_file_path) {
    stdout_handle_ = GetStdHandle(STD_OUTPUT_HANDLE);

    log_file_ = CreateFileW(log_file_path.c_str(), FILE_APPEND_DATA,
                            FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_ALWAYS,
                            FILE_ATTRIBUTE_NORMAL, nullptr);
    if (log_file_ != INVALID_HANDLE_VALUE) {
      SetFilePointer(log_file_, 0, nullptr, FILE_END);
    }

    com1_ = CreateFileW(L"\\\\.\\COM1", GENERIC_WRITE, 0, nullptr, OPEN_EXISTING, 0, nullptr);
    if (com1_ != INVALID_HANDLE_VALUE) {
      DCB dcb{};
      dcb.DCBlength = sizeof(dcb);
      if (GetCommState(com1_, &dcb)) {
        dcb.BaudRate = CBR_115200;
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        SetCommState(com1_, &dcb);
      }
      COMMTIMEOUTS timeouts{};
      timeouts.WriteTotalTimeoutConstant = 1000;
      SetCommTimeouts(com1_, &timeouts);
    }
  }

  Logger(const Logger&) = delete;
  Logger& operator=(const Logger&) = delete;

  ~Logger() {
    if (log_file_ != INVALID_HANDLE_VALUE) {
      FlushFileBuffers(log_file_);
      CloseHandle(log_file_);
    }
    if (com1_ != INVALID_HANDLE_VALUE) {
      CloseHandle(com1_);
    }
  }

  void LogLine(const std::string& line) {
    std::string out = line;
    if (out.empty() || (out.back() != '\n' && out.back() != '\r')) {
      out.append("\r\n");
    } else if (out.back() == '\n' && (out.size() < 2 || out[out.size() - 2] != '\r')) {
      out.insert(out.end() - 1, '\r');
    }

    WriteAll(stdout_handle_, out);
    if (log_file_ != INVALID_HANDLE_VALUE) {
      WriteAll(log_file_, out);
    }
    if (com1_ != INVALID_HANDLE_VALUE) {
      WriteAll(com1_, out);
    }
  }

  void Logf(const char* fmt, ...) {
    char buf[4096];
    va_list ap;
    va_start(ap, fmt);
    const int n = vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    if (n < 0) {
      LogLine(std::string("log format error: ") + fmt);
      return;
    }
    LogLine(std::string(buf, buf + std::min(n, static_cast<int>(sizeof(buf) - 1))));
  }

 private:
  static void WriteAll(HANDLE h, const std::string& bytes) {
    if (h == INVALID_HANDLE_VALUE || h == nullptr) return;
    const char* p = bytes.data();
    DWORD remaining = static_cast<DWORD>(bytes.size());
    while (remaining > 0) {
      DWORD written = 0;
      if (!WriteFile(h, p, remaining, &written, nullptr)) return;
      if (written == 0) return;
      p += written;
      remaining -= written;
    }
  }

  HANDLE stdout_handle_{INVALID_HANDLE_VALUE};
  HANDLE log_file_{INVALID_HANDLE_VALUE};
  HANDLE com1_{INVALID_HANDLE_VALUE};
};

struct TestResult {
  bool ok = false;
  std::string fail_reason;
  HRESULT hr = S_OK;
};

struct StorageIdStrings {
  STORAGE_BUS_TYPE bus_type = BusTypeUnknown;
  std::wstring vendor;
  std::wstring product;
  std::wstring revision;
};

static std::optional<StorageIdStrings> QueryStorageIdStrings(HANDLE h) {
  if (h == INVALID_HANDLE_VALUE) return std::nullopt;

  STORAGE_PROPERTY_QUERY query{};
  query.PropertyId = StorageDeviceProperty;
  query.QueryType = PropertyStandardQuery;

  std::vector<BYTE> buf(4096);
  DWORD bytes = 0;
  if (!DeviceIoControl(h, IOCTL_STORAGE_QUERY_PROPERTY, &query, sizeof(query), buf.data(),
                       static_cast<DWORD>(buf.size()), &bytes, nullptr)) {
    return std::nullopt;
  }

  if (bytes < sizeof(STORAGE_DEVICE_DESCRIPTOR)) return std::nullopt;
  const auto* desc = reinterpret_cast<const STORAGE_DEVICE_DESCRIPTOR*>(buf.data());

  auto extract = [&](DWORD offset) -> std::wstring {
    if (offset == 0) return L"";
    if (offset >= buf.size()) return L"";
    const char* s = reinterpret_cast<const char*>(buf.data() + offset);
    const size_t max_len = buf.size() - offset;
    const size_t len = BoundedStrLen(s, max_len);
    return AnsiNToWide(s, len);
  };

  StorageIdStrings out{};
  out.bus_type = desc->BusType;
  out.vendor = extract(desc->VendorIdOffset);
  out.product = extract(desc->ProductIdOffset);
  out.revision = extract(desc->ProductRevisionOffset);
  return out;
}

static bool LooksLikeVirtioStorageId(const StorageIdStrings& id) {
  if (ContainsInsensitive(id.vendor, L"virtio") || ContainsInsensitive(id.product, L"virtio")) {
    return true;
  }
  // Common virtio-win identification.
  if (ContainsInsensitive(id.vendor, L"red hat") || ContainsInsensitive(id.product, L"red hat")) {
    return true;
  }
  return false;
}

static std::vector<std::wstring> GetDevicePropertyMultiSz(HDEVINFO devinfo, SP_DEVINFO_DATA* dev,
                                                          DWORD property) {
  DWORD reg_type = 0;
  DWORD required = 0;
  SetupDiGetDeviceRegistryPropertyW(devinfo, dev, property, &reg_type, nullptr, 0, &required);
  if (required == 0) return {};

  std::vector<BYTE> buf(required);
  if (!SetupDiGetDeviceRegistryPropertyW(devinfo, dev, property, &reg_type, buf.data(), required,
                                         nullptr)) {
    return {};
  }
  if (reg_type != REG_MULTI_SZ && reg_type != REG_SZ) return {};

  const wchar_t* p = reinterpret_cast<const wchar_t*>(buf.data());
  const size_t total_wchars = required / sizeof(wchar_t);
  (void)total_wchars;

  std::vector<std::wstring> out;
  while (*p) {
    out.emplace_back(p);
    p += wcslen(p) + 1;
  }
  return out;
}

static std::optional<std::wstring> GetDevicePropertyString(HDEVINFO devinfo, SP_DEVINFO_DATA* dev,
                                                           DWORD property) {
  DWORD reg_type = 0;
  DWORD required = 0;
  SetupDiGetDeviceRegistryPropertyW(devinfo, dev, property, &reg_type, nullptr, 0, &required);
  if (required == 0) return std::nullopt;

  std::vector<BYTE> buf(required);
  if (!SetupDiGetDeviceRegistryPropertyW(devinfo, dev, property, &reg_type, buf.data(), required,
                                         nullptr)) {
    return std::nullopt;
  }
  if (reg_type != REG_SZ) return std::nullopt;
  return std::wstring(reinterpret_cast<const wchar_t*>(buf.data()));
}

static bool IsVirtioHardwareId(const std::vector<std::wstring>& hwids) {
  for (const auto& id : hwids) {
    if (ContainsInsensitive(id, L"VEN_1AF4") || ContainsInsensitive(id, L"VIRTIO")) return true;
  }
  return false;
}

static std::vector<std::wstring> GetHardwareIdsForInstanceId(const std::wstring& instance_id) {
  if (instance_id.empty()) return {};

  HDEVINFO devinfo = SetupDiCreateDeviceInfoList(nullptr, nullptr);
  if (devinfo == INVALID_HANDLE_VALUE) return {};

  SP_DEVINFO_DATA dev{};
  dev.cbSize = sizeof(dev);

  if (!SetupDiOpenDeviceInfoW(devinfo, instance_id.c_str(), nullptr, 0, &dev)) {
    SetupDiDestroyDeviceInfoList(devinfo);
    return {};
  }

  auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
  SetupDiDestroyDeviceInfoList(devinfo);
  return hwids;
}

static std::optional<std::wstring> GetDeviceInstanceIdString(HDEVINFO devinfo, SP_DEVINFO_DATA* dev) {
  if (!devinfo || devinfo == INVALID_HANDLE_VALUE || !dev) return std::nullopt;

  DWORD required = 0;
  wchar_t dummy[1]{};
  SetupDiGetDeviceInstanceIdW(devinfo, dev, dummy, static_cast<DWORD>(sizeof(dummy) / sizeof(dummy[0])),
                              &required);
  if (required == 0) return std::nullopt;

  std::vector<wchar_t> buf(required);
  if (!SetupDiGetDeviceInstanceIdW(devinfo, dev, buf.data(), required, nullptr)) {
    return std::nullopt;
  }
  return std::wstring(buf.data());
}

static bool IsVirtioSndPciHardwareId(const std::vector<std::wstring>& hwids) {
  for (const auto& id : hwids) {
    if (ContainsInsensitive(id, L"PCI\\VEN_1AF4&DEV_1059")) return true;
  }
  return false;
}

struct VirtioSndPciDevice {
  std::wstring instance_id;
  std::wstring description;
};

static std::vector<VirtioSndPciDevice> DetectVirtioSndPciDevices(Logger& log) {
  std::vector<VirtioSndPciDevice> out;

  HDEVINFO devinfo =
      // Restrict to PCI enumerated devices for speed/determinism. The virtio-snd function is a PCI
      // function, so it should always show up here if present.
      SetupDiGetClassDevsW(nullptr, L"PCI", nullptr, DIGCF_PRESENT | DIGCF_ALLCLASSES);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-snd: SetupDiGetClassDevs(enumerator=PCI) failed: %lu", GetLastError());
    return out;
  }

  for (DWORD idx = 0;; idx++) {
    SP_DEVINFO_DATA dev{};
    dev.cbSize = sizeof(dev);
    if (!SetupDiEnumDeviceInfo(devinfo, idx, &dev)) {
      if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
      continue;
    }

    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
    if (!IsVirtioSndPciHardwareId(hwids)) continue;

    VirtioSndPciDevice snd{};
    if (auto inst = GetDeviceInstanceIdString(devinfo, &dev)) {
      snd.instance_id = *inst;
    }
    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      snd.description = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      snd.description = *desc;
    }

    log.Logf("virtio-snd: detected PCI device instance_id=%s name=%s",
             WideToUtf8(snd.instance_id).c_str(), WideToUtf8(snd.description).c_str());
    if (!hwids.empty()) {
      log.Logf("virtio-snd: detected PCI device hwid0=%s", WideToUtf8(hwids[0]).c_str());
    }
    out.push_back(std::move(snd));
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return out;
}

static std::set<DWORD> DetectVirtioDiskNumbers(Logger& log) {
  std::set<DWORD> disks;

  HDEVINFO devinfo =
      SetupDiGetClassDevsW(&GUID_DEVINTERFACE_DISK, nullptr, nullptr,
                           DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: SetupDiGetClassDevs(GUID_DEVINTERFACE_DISK) failed: %lu", GetLastError());
    return disks;
  }

  for (DWORD idx = 0;; idx++) {
    SP_DEVICE_INTERFACE_DATA iface{};
    iface.cbSize = sizeof(iface);
    if (!SetupDiEnumDeviceInterfaces(devinfo, nullptr, &GUID_DEVINTERFACE_DISK, idx, &iface)) {
      if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
      continue;
    }

    DWORD detail_size = 0;
    SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, nullptr, 0, &detail_size, nullptr);
    if (detail_size == 0) continue;

    std::vector<BYTE> detail_buf(detail_size);
    auto* detail = reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W*>(detail_buf.data());
    detail->cbSize = sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W);

    SP_DEVINFO_DATA dev{};
    dev.cbSize = sizeof(dev);

    if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, detail_size, nullptr, &dev)) {
      continue;
    }

    HANDLE h = CreateFileW(detail->DevicePath, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                           OPEN_EXISTING, 0, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
      log.Logf("virtio-blk: CreateFile(%s) failed: %lu", WideToUtf8(detail->DevicePath).c_str(),
               GetLastError());
      continue;
    }

    bool is_virtio = false;
    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
    if (IsVirtioHardwareId(hwids)) is_virtio = true;
    if (const auto sid = QueryStorageIdStrings(h); sid.has_value() && LooksLikeVirtioStorageId(*sid)) {
      is_virtio = true;
    }

    if (!is_virtio) {
      CloseHandle(h);
      continue;
    }

    STORAGE_DEVICE_NUMBER devnum{};
    DWORD bytes = 0;
    if (DeviceIoControl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0, &devnum, sizeof(devnum),
                        &bytes, nullptr)) {
      disks.insert(devnum.DeviceNumber);
      if (const auto sid = QueryStorageIdStrings(h); sid.has_value()) {
        log.Logf("virtio-blk: detected disk device_number=%lu path=%s vendor=%s product=%s",
                 devnum.DeviceNumber, WideToUtf8(detail->DevicePath).c_str(),
                 WideToUtf8(sid->vendor).c_str(), WideToUtf8(sid->product).c_str());
      } else {
        log.Logf("virtio-blk: detected disk device_number=%lu path=%s", devnum.DeviceNumber,
                 WideToUtf8(detail->DevicePath).c_str());
      }
    } else {
      log.Logf("virtio-blk: IOCTL_STORAGE_GET_DEVICE_NUMBER failed: %lu", GetLastError());
    }

    CloseHandle(h);
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return disks;
}

static std::optional<wchar_t> FindMountedDriveLetterOnDisks(Logger& log,
                                                            const std::set<DWORD>& disk_numbers) {
  if (disk_numbers.empty()) return std::nullopt;

  DWORD mask = GetLogicalDrives();
  if (mask == 0) {
    log.Logf("virtio-blk: GetLogicalDrives failed: %lu", GetLastError());
    return std::nullopt;
  }

  for (wchar_t letter = L'C'; letter <= L'Z'; letter++) {
    if ((mask & (1u << (letter - L'A'))) == 0) continue;

    wchar_t root[] = L"X:\\";
    root[0] = letter;
    const UINT drive_type = GetDriveTypeW(root);
    if (drive_type != DRIVE_FIXED) continue;

    wchar_t vol_path[] = L"\\\\.\\X:";
    vol_path[4] = letter;

    HANDLE h =
        CreateFileW(vol_path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0,
                    nullptr);
    if (h == INVALID_HANDLE_VALUE) continue;

    STORAGE_DEVICE_NUMBER devnum{};
    DWORD bytes = 0;
    if (DeviceIoControl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0, &devnum, sizeof(devnum),
                        &bytes, nullptr)) {
      if (disk_numbers.count(devnum.DeviceNumber) != 0) {
        CloseHandle(h);
        return letter;
      }

      // As a fallback, check the storage descriptor strings. This helps if the disk does not expose
      // a virtio-looking hardware ID via SetupAPI but does identify itself as VirtIO/Red Hat.
      if (const auto sid = QueryStorageIdStrings(h); sid.has_value() && LooksLikeVirtioStorageId(*sid)) {
        log.Logf("virtio-blk: drive %lc: looks virtio via storage id vendor=%s product=%s", letter,
                 WideToUtf8(sid->vendor).c_str(), WideToUtf8(sid->product).c_str());
        CloseHandle(h);
        return letter;
      }
    }

    CloseHandle(h);
  }

  return std::nullopt;
}

static std::optional<DWORD> DiskNumberForDriveLetter(wchar_t letter) {
  wchar_t vol_path[] = L"\\\\.\\X:";
  vol_path[4] = letter;

  HANDLE h =
      CreateFileW(vol_path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0,
                  nullptr);
  if (h == INVALID_HANDLE_VALUE) return std::nullopt;

  STORAGE_DEVICE_NUMBER devnum{};
  DWORD bytes = 0;
  const bool ok = DeviceIoControl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0, &devnum,
                                  sizeof(devnum), &bytes, nullptr) != 0;
  CloseHandle(h);
  if (!ok) return std::nullopt;
  return devnum.DeviceNumber;
}

static bool DriveLetterLooksLikeVirtio(Logger& log, wchar_t letter) {
  wchar_t vol_path[] = L"\\\\.\\X:";
  vol_path[4] = letter;

  HANDLE h =
      CreateFileW(vol_path, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0, nullptr);
  if (h == INVALID_HANDLE_VALUE) return false;

  const auto sid = QueryStorageIdStrings(h);
  CloseHandle(h);
  if (!sid.has_value()) return false;

  if (LooksLikeVirtioStorageId(*sid)) {
    log.Logf("virtio-blk: drive %lc: looks virtio via storage id vendor=%s product=%s", letter,
             WideToUtf8(sid->vendor).c_str(), WideToUtf8(sid->product).c_str());
    return true;
  }
  return false;
}

static std::optional<wchar_t> DriveLetterFromPath(const std::wstring& path) {
  if (path.size() < 2) return std::nullopt;
  const wchar_t c = path[0];
  if (path[1] != L':') return std::nullopt;
  if (!iswalpha(c)) return std::nullopt;
  return static_cast<wchar_t>(towupper(c));
}

static bool EnsureDirectory(Logger& log, const std::wstring& dir) {
  if (dir.empty()) return false;

  if (CreateDirectoryW(dir.c_str(), nullptr)) return true;
  if (GetLastError() == ERROR_ALREADY_EXISTS) return true;

  log.Logf("failed to create directory: %s err=%lu", WideToUtf8(dir).c_str(), GetLastError());
  return false;
}

static std::wstring JoinPath(const std::wstring& a, const std::wstring& b) {
  if (a.empty()) return b;
  if (b.empty()) return a;
  if (a.back() == L'\\' || a.back() == L'/') return a + b;
  return a + L'\\' + b;
}

struct PerfTimer {
  LARGE_INTEGER freq{};
  LARGE_INTEGER start{};

  PerfTimer() {
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&start);
  }

  double SecondsSinceStart() const {
    LARGE_INTEGER now{};
    QueryPerformanceCounter(&now);
    return static_cast<double>(now.QuadPart - start.QuadPart) / static_cast<double>(freq.QuadPart);
  }
};

static bool VirtioBlkTest(Logger& log, const Options& opt) {
  const auto disks = DetectVirtioDiskNumbers(log);
  if (disks.empty()) {
    log.LogLine("virtio-blk: no virtio disk devices detected");
    return false;
  }

  std::wstring base_dir;

  wchar_t temp_path[MAX_PATH]{};
  if (GetTempPathW(MAX_PATH, temp_path) == 0) {
    wcscpy_s(temp_path, L"C:\\Windows\\Temp\\");
  }

  if (!opt.blk_root.empty()) {
    base_dir = opt.blk_root;
    (void)EnsureDirectory(log, base_dir);
  } else if (const auto drive_letter = FindMountedDriveLetterOnDisks(log, disks);
             drive_letter.has_value()) {
    base_dir = std::wstring(1, *drive_letter) + L":\\aero-virtio-selftest\\";
    (void)EnsureDirectory(log, base_dir);
  } else {
    base_dir = temp_path;
  }

  const auto base_drive = DriveLetterFromPath(base_dir);
  if (!base_drive.has_value()) {
    log.Logf("virtio-blk: unable to determine drive letter for test dir: %s",
             WideToUtf8(base_dir).c_str());
    log.LogLine("virtio-blk: specify --blk-root (e.g. D:\\aero-test\\) on a virtio volume");
    return false;
  }

  const auto base_disk = DiskNumberForDriveLetter(*base_drive);
  if (!base_disk.has_value()) {
    log.Logf("virtio-blk: unable to query disk number for %lc:", *base_drive);
    log.LogLine("virtio-blk: specify --blk-root (e.g. D:\\aero-test\\) on a virtio volume");
    return false;
  }

  if (disks.count(*base_disk) == 0 && !DriveLetterLooksLikeVirtio(log, *base_drive)) {
    log.Logf("virtio-blk: test dir is on disk %lu (not detected as virtio)", *base_disk);
    log.LogLine("virtio-blk: ensure a virtio disk is formatted/mounted with a drive letter, or pass --blk-root");
    return false;
  }

  const std::wstring test_file = JoinPath(base_dir, L"virtio-blk-test.bin");
  log.Logf("virtio-blk: test_file=%s size_mib=%lu chunk_kib=%lu", WideToUtf8(test_file).c_str(),
           opt.io_file_size_mib, opt.io_chunk_kib);

  const uint64_t total_bytes = static_cast<uint64_t>(opt.io_file_size_mib) * 1024ull * 1024ull;
  const uint32_t chunk_bytes = std::max<DWORD>(1, opt.io_chunk_kib) * 1024u;

  std::vector<uint8_t> buf(chunk_bytes);

  HANDLE h = CreateFileW(test_file.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                         FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: CreateFile failed: %lu", GetLastError());
    return false;
  }

  // Sequential write.
  {
    PerfTimer t;
    uint64_t written_total = 0;
    while (written_total < total_bytes) {
      const uint32_t to_write =
          static_cast<uint32_t>(std::min<uint64_t>(chunk_bytes, total_bytes - written_total));
      for (uint32_t i = 0; i < to_write; i++) {
        buf[i] = static_cast<uint8_t>((written_total + i) & 0xFF);
      }

      DWORD written = 0;
      if (!WriteFile(h, buf.data(), to_write, &written, nullptr) || written != to_write) {
        log.Logf("virtio-blk: WriteFile failed at offset=%llu err=%lu", written_total,
                 GetLastError());
        CloseHandle(h);
        DeleteFileW(test_file.c_str());
        return false;
      }
      written_total += written;
    }
    const double sec = std::max(0.000001, t.SecondsSinceStart());
    log.Logf("virtio-blk: write ok bytes=%llu mbps=%.2f", written_total,
             (written_total / (1024.0 * 1024.0)) / sec);
  }

  if (!FlushFileBuffers(h)) {
    log.Logf("virtio-blk: FlushFileBuffers failed: %lu", GetLastError());
    CloseHandle(h);
    DeleteFileW(test_file.c_str());
    return false;
  }
  log.LogLine("virtio-blk: flush ok");

  // Readback verify.
  if (SetFilePointer(h, 0, nullptr, FILE_BEGIN) == INVALID_SET_FILE_POINTER &&
      GetLastError() != NO_ERROR) {
    log.Logf("virtio-blk: SetFilePointer failed: %lu", GetLastError());
    CloseHandle(h);
    DeleteFileW(test_file.c_str());
    return false;
  }

  {
    uint64_t read_total = 0;
    while (read_total < total_bytes) {
      const uint32_t to_read =
          static_cast<uint32_t>(std::min<uint64_t>(chunk_bytes, total_bytes - read_total));
      DWORD read = 0;
      if (!ReadFile(h, buf.data(), to_read, &read, nullptr) || read != to_read) {
        log.Logf("virtio-blk: ReadFile failed at offset=%llu err=%lu", read_total, GetLastError());
        CloseHandle(h);
        DeleteFileW(test_file.c_str());
        return false;
      }
      for (uint32_t i = 0; i < to_read; i++) {
        const uint8_t expected = static_cast<uint8_t>((read_total + i) & 0xFF);
        if (buf[i] != expected) {
          log.Logf("virtio-blk: data mismatch at offset=%llu expected=0x%02x got=0x%02x",
                   (read_total + i), expected, buf[i]);
          CloseHandle(h);
          DeleteFileW(test_file.c_str());
          return false;
        }
      }
      read_total += read;
    }
    log.Logf("virtio-blk: readback verify ok bytes=%llu", read_total);
  }

  CloseHandle(h);

  // Separate sequential read pass (reopen file).
  h = CreateFileW(test_file.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                  FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: reopen for read failed: %lu", GetLastError());
    DeleteFileW(test_file.c_str());
    return false;
  }

  {
    PerfTimer t;
    uint64_t read_total = 0;
    while (true) {
      DWORD read = 0;
      if (!ReadFile(h, buf.data(), chunk_bytes, &read, nullptr)) {
        log.Logf("virtio-blk: sequential ReadFile failed err=%lu", GetLastError());
        CloseHandle(h);
        DeleteFileW(test_file.c_str());
        return false;
      }
      if (read == 0) break;
      read_total += read;
    }
    const double sec = std::max(0.000001, t.SecondsSinceStart());
    log.Logf("virtio-blk: sequential read ok bytes=%llu mbps=%.2f", read_total,
             (read_total / (1024.0 * 1024.0)) / sec);
  }

  CloseHandle(h);
  DeleteFileW(test_file.c_str());
  return true;
}

struct VirtioInputTestResult {
  bool ok = false;
  int matched_devices = 0;
  int keyboard_devices = 0;
  int mouse_devices = 0;
  int ambiguous_devices = 0;
  int unknown_devices = 0;
  int keyboard_collections = 0;
  int mouse_collections = 0;
  std::string reason;
};

static bool IsVirtioInputHardwareId(const std::vector<std::wstring>& hwids) {
  for (const auto& id : hwids) {
    if (ContainsInsensitive(id, L"VEN_1AF4&DEV_1052")) return true;
    if (ContainsInsensitive(id, L"VEN_1AF4&DEV_1011")) return true;
    // Some stacks may expose HID-style IDs (VID/PID) instead of PCI-style VEN/DEV.
    if (ContainsInsensitive(id, L"VID_1AF4&PID_1052")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_1011")) return true;
  }
  return false;
}

static bool LooksLikeVirtioInputInterfacePath(const std::wstring& device_path) {
  return ContainsInsensitive(device_path, L"VEN_1AF4&DEV_1052") ||
         ContainsInsensitive(device_path, L"VEN_1AF4&DEV_1011") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_1052") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_1011");
}

static HANDLE OpenHidDeviceForIoctl(const wchar_t* path) {
  const DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  const DWORD flags = FILE_ATTRIBUTE_NORMAL;
  const DWORD desired_accesses[] = {GENERIC_READ | GENERIC_WRITE, GENERIC_READ, 0};

  for (const DWORD access : desired_accesses) {
    HANDLE h = CreateFileW(path, access, share, nullptr, OPEN_EXISTING, flags, nullptr);
    if (h != INVALID_HANDLE_VALUE) return h;
  }
  return INVALID_HANDLE_VALUE;
}

static std::optional<std::vector<uint8_t>> ReadHidReportDescriptor(Logger& log, HANDLE h) {
  if (h == INVALID_HANDLE_VALUE) return std::nullopt;

  std::vector<uint8_t> buf(8192);
  DWORD bytes = 0;
  if (!DeviceIoControl(h, IOCTL_HID_GET_REPORT_DESCRIPTOR, nullptr, 0, buf.data(),
                       static_cast<DWORD>(buf.size()), &bytes, nullptr)) {
    log.Logf("virtio-input: IOCTL_HID_GET_REPORT_DESCRIPTOR failed err=%lu", GetLastError());
    return std::nullopt;
  }
  if (bytes == 0 || bytes > buf.size()) {
    log.Logf("virtio-input: IOCTL_HID_GET_REPORT_DESCRIPTOR returned unexpected size=%lu", bytes);
    return std::nullopt;
  }

  buf.resize(bytes);
  return buf;
}

struct HidReportDescriptorSummary {
  int keyboard_app_collections = 0;
  int mouse_app_collections = 0;
};

static HidReportDescriptorSummary SummarizeHidReportDescriptor(const std::vector<uint8_t>& desc) {
  HidReportDescriptorSummary out{};

  uint32_t usage_page = 0;
  std::vector<uint32_t> usage_page_stack;
  std::vector<uint32_t> local_usages;
  std::optional<uint32_t> local_usage_min;

  auto clear_locals = [&]() {
    local_usages.clear();
    local_usage_min.reset();
  };

  size_t i = 0;
  while (i < desc.size()) {
    const uint8_t prefix = desc[i++];
    if (prefix == 0xFE) {
      // Long item: 0xFE, size, tag, data...
      if (i + 2 > desc.size()) break;
      const uint8_t size = desc[i++];
      i++; // long item tag (ignored)
      if (i + size > desc.size()) break;
      i += size;
      continue;
    }

    const uint8_t size_code = prefix & 0x3;
    const uint8_t type = (prefix >> 2) & 0x3;
    const uint8_t tag = (prefix >> 4) & 0xF;

    const size_t data_size = (size_code == 3) ? 4 : size_code;
    if (i + data_size > desc.size()) break;

    uint32_t value = 0;
    for (size_t j = 0; j < data_size; j++) {
      value |= static_cast<uint32_t>(desc[i + j]) << (8u * j);
    }
    i += data_size;

    switch (type) {
      case 0: { // Main
        // Collection (tag 0xA) + Application (0x01)
        if (tag == 0xA) {
          const uint8_t collection_type = static_cast<uint8_t>(value & 0xFF);
          if (collection_type == 0x01) {
            std::optional<uint32_t> usage;
            if (!local_usages.empty()) {
              usage = local_usages.front();
            } else if (local_usage_min.has_value()) {
              usage = *local_usage_min;
            }

            if (usage.has_value()) {
              // Generic Desktop Page (0x01): Keyboard (0x06), Mouse (0x02)
              if (usage_page == 0x01 && *usage == 0x06) out.keyboard_app_collections++;
              if (usage_page == 0x01 && *usage == 0x02) out.mouse_app_collections++;
            }
          }
        }
        // Local items are cleared after each main item per HID spec.
        clear_locals();
        break;
      }
      case 1: { // Global
        if (tag == 0x0) { // Usage Page
          usage_page = value;
        } else if (tag == 0xA) { // Push
          usage_page_stack.push_back(usage_page);
        } else if (tag == 0xB) { // Pop
          if (!usage_page_stack.empty()) {
            usage_page = usage_page_stack.back();
            usage_page_stack.pop_back();
          }
        }
        break;
      }
      case 2: { // Local
        if (tag == 0x0) { // Usage
          local_usages.push_back(value);
        } else if (tag == 0x1) { // Usage Minimum
          local_usage_min = value;
        }
        break;
      }
      default:
        break;
    }
  }

  return out;
}

static VirtioInputTestResult VirtioInputTest(Logger& log) {
  VirtioInputTestResult out{};

  // {4D1E55B2-F16F-11CF-88CB-001111000030}
  static const GUID kHidInterfaceGuid = {0x4D1E55B2,
                                         0xF16F,
                                         0x11CF,
                                         {0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30}};

  HDEVINFO devinfo = SetupDiGetClassDevsW(&kHidInterfaceGuid, nullptr, nullptr,
                                         DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    out.reason = "setupapi_classdevs_failed";
    log.Logf("virtio-input: SetupDiGetClassDevs(GUID_DEVINTERFACE_HID) failed: %lu", GetLastError());
    return out;
  }

  bool had_error = false;

  for (DWORD idx = 0;; idx++) {
    SP_DEVICE_INTERFACE_DATA iface{};
    iface.cbSize = sizeof(iface);
    if (!SetupDiEnumDeviceInterfaces(devinfo, nullptr, &kHidInterfaceGuid, idx, &iface)) {
      if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
      continue;
    }

    DWORD detail_size = 0;
    SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, nullptr, 0, &detail_size, nullptr);
    if (detail_size == 0) continue;

    std::vector<BYTE> detail_buf(detail_size);
    auto* detail = reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W*>(detail_buf.data());
    detail->cbSize = sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W);

    SP_DEVINFO_DATA dev{};
    dev.cbSize = sizeof(dev);

    if (!SetupDiGetDeviceInterfaceDetailW(devinfo, &iface, detail, detail_size, nullptr, &dev)) {
      continue;
    }

    const std::wstring device_path = detail->DevicePath;
    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);

    if (!IsVirtioInputHardwareId(hwids) && !LooksLikeVirtioInputInterfacePath(device_path)) {
      continue;
    }

    out.matched_devices++;

    auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC);
    if (desc) {
      log.Logf("virtio-input: HID device match desc=%s path=%s", WideToUtf8(*desc).c_str(),
               WideToUtf8(device_path).c_str());
    } else {
      log.Logf("virtio-input: HID device match path=%s", WideToUtf8(device_path).c_str());
    }

    HANDLE h = OpenHidDeviceForIoctl(device_path.c_str());
    if (h == INVALID_HANDLE_VALUE) {
      had_error = true;
      log.Logf("virtio-input: CreateFile(%s) failed err=%lu", WideToUtf8(device_path).c_str(),
               GetLastError());
      continue;
    }

    const auto report_desc = ReadHidReportDescriptor(log, h);
    CloseHandle(h);
    if (!report_desc.has_value()) {
      had_error = true;
      continue;
    }

    const auto summary = SummarizeHidReportDescriptor(*report_desc);
    const bool has_keyboard = summary.keyboard_app_collections > 0;
    const bool has_mouse = summary.mouse_app_collections > 0;
    if (has_keyboard && has_mouse) {
      out.ambiguous_devices++;
    } else if (has_keyboard) {
      out.keyboard_devices++;
    } else if (has_mouse) {
      out.mouse_devices++;
    } else {
      out.unknown_devices++;
    }
    out.keyboard_collections += summary.keyboard_app_collections;
    out.mouse_collections += summary.mouse_app_collections;

    log.Logf("virtio-input: report_descriptor bytes=%zu keyboard_app_collections=%d "
             "mouse_app_collections=%d",
             report_desc->size(), summary.keyboard_app_collections, summary.mouse_app_collections);
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  if (out.matched_devices == 0) {
    out.reason = "no_matching_hid_devices";
    log.LogLine("virtio-input: no virtio-input HID devices detected");
    return out;
  }
  if (had_error) {
    out.reason = "ioctl_or_open_failed";
    return out;
  }
  if (out.keyboard_devices <= 0) {
    out.reason = "missing_keyboard_device";
    return out;
  }
  if (out.mouse_devices <= 0) {
    out.reason = "missing_mouse_device";
    return out;
  }
  if (out.ambiguous_devices > 0) {
    out.reason = "ambiguous_device";
    return out;
  }
  if (out.unknown_devices > 0) {
    out.reason = "unknown_device";
    return out;
  }

  out.ok = true;
  return out;
}

struct VirtioNetAdapter {
  std::wstring instance_id;   // e.g. "{GUID}"
  std::wstring friendly_name; // optional
};

static std::vector<VirtioNetAdapter> DetectVirtioNetAdapters(Logger& log) {
  std::vector<VirtioNetAdapter> out;

  HDEVINFO devinfo = SetupDiGetClassDevsW(&GUID_DEVCLASS_NET, nullptr, nullptr, DIGCF_PRESENT);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-net: SetupDiGetClassDevs(GUID_DEVCLASS_NET) failed: %lu", GetLastError());
    return out;
  }

  for (DWORD idx = 0;; idx++) {
    SP_DEVINFO_DATA dev{};
    dev.cbSize = sizeof(dev);
    if (!SetupDiEnumDeviceInfo(devinfo, idx, &dev)) {
      if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
      continue;
    }

    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
    if (!IsVirtioHardwareId(hwids)) continue;

    VirtioNetAdapter adapter{};
    if (auto inst = GetDevicePropertyString(devinfo, &dev, SPDRP_NETCFG_INSTANCE_ID)) {
      adapter.instance_id = *inst;
    }
    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      adapter.friendly_name = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      adapter.friendly_name = *desc;
    }

    if (!adapter.instance_id.empty()) {
      log.Logf("virtio-net: detected adapter instance_id=%s name=%s",
               WideToUtf8(adapter.instance_id).c_str(), WideToUtf8(adapter.friendly_name).c_str());
      out.push_back(std::move(adapter));
    }
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return out;
}

static bool IsApipaV4(const IN_ADDR& addr) {
  const uint32_t host = ntohl(addr.S_un.S_addr);
  const uint8_t a = static_cast<uint8_t>((host >> 24) & 0xFF);
  const uint8_t b = static_cast<uint8_t>((host >> 16) & 0xFF);
  return a == 169 && b == 254;
}

static std::optional<IN_ADDR> FindIpv4AddressForAdapterGuid(const std::wstring& adapter_guid,
                                                            bool* oper_up_out,
                                                            std::wstring* friendly_out) {
  if (oper_up_out) *oper_up_out = false;
  if (friendly_out) friendly_out->clear();

  ULONG size = 0;
  GetAdaptersAddresses(AF_INET, GAA_FLAG_INCLUDE_PREFIX, nullptr, nullptr, &size);
  if (size == 0) return std::nullopt;

  std::vector<BYTE> buf(size);
  auto* addrs = reinterpret_cast<IP_ADAPTER_ADDRESSES*>(buf.data());
  if (GetAdaptersAddresses(AF_INET, GAA_FLAG_INCLUDE_PREFIX, nullptr, addrs, &size) != NO_ERROR) {
    return std::nullopt;
  }

  const auto needle = NormalizeGuidLikeString(adapter_guid);

  for (auto* a = addrs; a != nullptr; a = a->Next) {
    const std::wstring name = NormalizeGuidLikeString(AnsiToWide(a->AdapterName));
    if (name != needle) continue;

    if (oper_up_out) *oper_up_out = (a->OperStatus == IfOperStatusUp);
    if (friendly_out && a->FriendlyName) *friendly_out = a->FriendlyName;

    for (auto* u = a->FirstUnicastAddress; u != nullptr; u = u->Next) {
      if (!u->Address.lpSockaddr) continue;
      if (u->Address.lpSockaddr->sa_family != AF_INET) continue;

      const auto* sin = reinterpret_cast<const sockaddr_in*>(u->Address.lpSockaddr);
      if (sin->sin_addr.S_un.S_addr == 0) continue;
      if (IsApipaV4(sin->sin_addr)) continue;
      return sin->sin_addr;
    }
  }

  return std::nullopt;
}

static std::optional<bool> IsDhcpEnabledForAdapterGuid(const std::wstring& adapter_guid) {
  ULONG size = 0;
  if (GetAdaptersInfo(nullptr, &size) != ERROR_BUFFER_OVERFLOW || size == 0) {
    return std::nullopt;
  }

  std::vector<BYTE> buf(size);
  auto* info = reinterpret_cast<IP_ADAPTER_INFO*>(buf.data());
  if (GetAdaptersInfo(info, &size) != NO_ERROR) {
    return std::nullopt;
  }

  const auto needle = NormalizeGuidLikeString(adapter_guid);

  for (auto* a = info; a != nullptr; a = a->Next) {
    const auto name = NormalizeGuidLikeString(AnsiToWide(a->AdapterName));
    if (name != needle) continue;
    return a->DhcpEnabled != 0;
  }

  return std::nullopt;
}

static bool DnsResolve(Logger& log, const std::wstring& hostname) {
  addrinfoW hints{};
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  addrinfoW* res = nullptr;
  const int rc = GetAddrInfoW(hostname.c_str(), nullptr, &hints, &res);
  if (rc != 0) {
    log.Logf("virtio-net: DNS resolve failed host=%s rc=%d", WideToUtf8(hostname).c_str(), rc);
    return false;
  }

  int count = 0;
  for (addrinfoW* it = res; it != nullptr && count < 4; it = it->ai_next) {
    if (!it->ai_addr) continue;
    if (it->ai_family == AF_INET) {
      const auto* sin = reinterpret_cast<const sockaddr_in*>(it->ai_addr);
      const uint32_t host = ntohl(sin->sin_addr.S_un.S_addr);
      const uint8_t a = static_cast<uint8_t>((host >> 24) & 0xFF);
      const uint8_t b = static_cast<uint8_t>((host >> 16) & 0xFF);
      const uint8_t c = static_cast<uint8_t>((host >> 8) & 0xFF);
      const uint8_t d = static_cast<uint8_t>(host & 0xFF);
      log.Logf("virtio-net: DNS A[%d]=%u.%u.%u.%u", count, a, b, c, d);
      count++;
    }
  }

  FreeAddrInfoW(res);
  log.Logf("virtio-net: DNS resolve ok host=%s", WideToUtf8(hostname).c_str());
  return true;
}

static bool DnsResolveWithFallback(Logger& log, const std::wstring& primary_host) {
  std::vector<std::wstring> candidates;
  auto add_unique = [&](const std::wstring& h) {
    if (h.empty()) return;
    for (const auto& existing : candidates) {
      if (ToLower(existing) == ToLower(h)) return;
    }
    candidates.push_back(h);
  };

  add_unique(primary_host);
  add_unique(L"host.lan");
  add_unique(L"gateway.lan");
  add_unique(L"dns.lan");
  add_unique(L"example.com");

  for (const auto& host : candidates) {
    if (DnsResolve(log, host)) return true;
  }
  return false;
}

static bool HttpGet(Logger& log, const std::wstring& url) {
  URL_COMPONENTS comp{};
  comp.dwStructSize = sizeof(comp);
  comp.dwSchemeLength = static_cast<DWORD>(-1);
  comp.dwHostNameLength = static_cast<DWORD>(-1);
  comp.dwUrlPathLength = static_cast<DWORD>(-1);
  comp.dwExtraInfoLength = static_cast<DWORD>(-1);

  if (!WinHttpCrackUrl(url.c_str(), 0, 0, &comp)) {
    log.Logf("virtio-net: WinHttpCrackUrl failed url=%s err=%lu", WideToUtf8(url).c_str(),
             GetLastError());
    return false;
  }

  std::wstring scheme(comp.lpszScheme, comp.dwSchemeLength);
  std::wstring host(comp.lpszHostName, comp.dwHostNameLength);
  std::wstring path(comp.lpszUrlPath, comp.dwUrlPathLength);
  if (comp.dwExtraInfoLength > 0) path.append(comp.lpszExtraInfo, comp.dwExtraInfoLength);
  const INTERNET_PORT port = comp.nPort;

  const bool secure = (comp.nScheme == INTERNET_SCHEME_HTTPS);
  if (secure) {
    log.LogLine("virtio-net: https urls are supported by WinHTTP, but are discouraged for tests "
                "(certificate store variability). Prefer http.");
  }

  HINTERNET session =
      // Use NO_PROXY for determinism. In some environments WinHTTP proxy settings can be
      // configured system-wide and interfere with connectivity checks.
      WinHttpOpen(L"AeroVirtioSelftest/1.0", WINHTTP_ACCESS_TYPE_NO_PROXY,
                  WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
  if (!session) {
    log.Logf("virtio-net: WinHttpOpen failed err=%lu", GetLastError());
    return false;
  }

  WinHttpSetTimeouts(session, 15000, 15000, 15000, 15000);

  HINTERNET connect = WinHttpConnect(session, host.c_str(), port, 0);
  if (!connect) {
    log.Logf("virtio-net: WinHttpConnect failed host=%s port=%u err=%lu",
             WideToUtf8(host).c_str(), port, GetLastError());
    WinHttpCloseHandle(session);
    return false;
  }

  const DWORD flags = secure ? WINHTTP_FLAG_SECURE : 0;
  HINTERNET request = WinHttpOpenRequest(connect, L"GET", path.c_str(), nullptr,
                                         WINHTTP_NO_REFERER, WINHTTP_DEFAULT_ACCEPT_TYPES, flags);
  if (!request) {
    log.Logf("virtio-net: WinHttpOpenRequest failed err=%lu", GetLastError());
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  if (!WinHttpSendRequest(request, WINHTTP_NO_ADDITIONAL_HEADERS, 0, WINHTTP_NO_REQUEST_DATA, 0,
                          0, 0)) {
    log.Logf("virtio-net: WinHttpSendRequest failed err=%lu", GetLastError());
    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  if (!WinHttpReceiveResponse(request, nullptr)) {
    log.Logf("virtio-net: WinHttpReceiveResponse failed err=%lu", GetLastError());
    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  DWORD status = 0;
  DWORD status_size = sizeof(status);
  if (!WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                           WINHTTP_HEADER_NAME_BY_INDEX, &status, &status_size,
                           WINHTTP_NO_HEADER_INDEX)) {
    log.Logf("virtio-net: WinHttpQueryHeaders(status) failed err=%lu", GetLastError());
    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  // Read some bytes to ensure data path works.
  DWORD total_read = 0;
  for (;;) {
    DWORD available = 0;
    if (!WinHttpQueryDataAvailable(request, &available)) break;
    if (available == 0) break;

    std::vector<uint8_t> tmp(std::min<DWORD>(available, 4096));
    DWORD read = 0;
    if (!WinHttpReadData(request, tmp.data(), static_cast<DWORD>(tmp.size()), &read)) break;
    if (read == 0) break;
    total_read += read;
    if (total_read >= 4096) break;
  }

  log.Logf("virtio-net: HTTP GET ok url=%s status=%lu bytes_read=%lu", WideToUtf8(url).c_str(),
           status, total_read);

  WinHttpCloseHandle(request);
  WinHttpCloseHandle(connect);
  WinHttpCloseHandle(session);

  return status >= 200 && status < 300;
}

static bool VirtioNetTest(Logger& log, const Options& opt) {
  const auto adapters = DetectVirtioNetAdapters(log);
  if (adapters.empty()) {
    log.LogLine("virtio-net: no virtio net adapters detected");
    return false;
  }

  log.Logf("virtio-net: waiting for link+dhcp timeout_sec=%lu", opt.net_timeout_sec);

  const DWORD deadline_ms = GetTickCount() + (opt.net_timeout_sec * 1000);
  std::optional<VirtioNetAdapter> chosen;
  IN_ADDR chosen_ip{};
  std::wstring chosen_friendly;

  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    for (const auto& a : adapters) {
      bool up = false;
      std::wstring friendly;
      const auto ip = FindIpv4AddressForAdapterGuid(a.instance_id, &up, &friendly);
      if (up && ip.has_value()) {
        chosen = a;
        chosen_ip = *ip;
        chosen_friendly = friendly.empty() ? a.friendly_name : friendly;
        break;
      }
    }
    if (chosen.has_value()) break;
    Sleep(2000);
  }

  if (!chosen.has_value()) {
    log.LogLine("virtio-net: timed out waiting for adapter to be UP with non-APIPA IPv4");
    return false;
  }

  const auto dhcp_enabled = IsDhcpEnabledForAdapterGuid(chosen->instance_id);
  if (!dhcp_enabled.has_value()) {
    log.LogLine("virtio-net: failed to query DHCP enabled state");
    return false;
  }
  if (!*dhcp_enabled) {
    log.LogLine("virtio-net: DHCP is not enabled for the virtio adapter");
    return false;
  }

  const uint32_t host = ntohl(chosen_ip.S_un.S_addr);
  const uint8_t a = static_cast<uint8_t>((host >> 24) & 0xFF);
  const uint8_t b = static_cast<uint8_t>((host >> 16) & 0xFF);
  const uint8_t c = static_cast<uint8_t>((host >> 8) & 0xFF);
  const uint8_t d = static_cast<uint8_t>(host & 0xFF);
  log.Logf("virtio-net: adapter up name=%s guid=%s ipv4=%u.%u.%u.%u",
           WideToUtf8(chosen_friendly).c_str(), WideToUtf8(chosen->instance_id).c_str(), a, b, c,
           d);

  if (!DnsResolveWithFallback(log, opt.dns_host)) return false;
  if (!HttpGet(log, opt.http_url)) return false;
  return true;
}

static const char* MmDeviceStateToString(DWORD state) {
  switch (state) {
    case DEVICE_STATE_ACTIVE:
      return "ACTIVE";
    case DEVICE_STATE_DISABLED:
      return "DISABLED";
    case DEVICE_STATE_NOTPRESENT:
      return "NOTPRESENT";
    case DEVICE_STATE_UNPLUGGED:
      return "UNPLUGGED";
    default:
      return "UNKNOWN";
  }
}

static std::wstring GetPropertyString(IPropertyStore* store, const PROPERTYKEY& key) {
  if (!store) return L"";
  PROPVARIANT var{};
  const HRESULT hr = store->GetValue(key, &var);
  if (FAILED(hr)) return L"";
  std::wstring out;
  if (var.vt == VT_LPWSTR && var.pwszVal) out = var.pwszVal;
  PropVariantClear(&var);
  return out;
}

static bool LooksLikeVirtioSndEndpoint(const std::wstring& friendly_name, const std::wstring& instance_id,
                                       const std::vector<std::wstring>& hwids,
                                       const std::vector<std::wstring>& match_names) {
  if (ContainsInsensitive(friendly_name, L"virtio") || ContainsInsensitive(friendly_name, L"aero")) return true;
  if (ContainsInsensitive(friendly_name, L"snd")) return true;
  for (const auto& m : match_names) {
    if (!m.empty() && ContainsInsensitive(friendly_name, m)) return true;
  }
  if (ContainsInsensitive(instance_id, L"DEV_1059") || ContainsInsensitive(instance_id, L"VEN_1AF4&DEV_1059")) {
    return true;
  }
  if (IsVirtioSndPciHardwareId(hwids)) return true;
  if (ContainsInsensitive(instance_id, L"VEN_1AF4") || ContainsInsensitive(instance_id, L"VIRTIO")) return true;
  if (IsVirtioHardwareId(hwids)) return true;
  return false;
}

static bool WaveFormatIsExtensible(const WAVEFORMATEX* fmt) {
  if (!fmt) return false;
  if (fmt->wFormatTag != WAVE_FORMAT_EXTENSIBLE) return false;
  return fmt->cbSize >= (sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
}

static const GUID kWaveSubFormatPcm = {0x00000001, 0x0000, 0x0010,
                                       {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};
static const GUID kWaveSubFormatIeeeFloat = {0x00000003, 0x0000, 0x0010,
                                             {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};

static bool WaveFormatIsPcm(const WAVEFORMATEX* fmt) {
  if (!fmt) return false;
  if (fmt->wFormatTag == WAVE_FORMAT_PCM) return true;
  if (WaveFormatIsExtensible(fmt)) {
    const auto* ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(fmt);
    return IsEqualGUID(ext->SubFormat, kWaveSubFormatPcm) != 0;
  }
  return false;
}

static bool WaveFormatIsFloat(const WAVEFORMATEX* fmt) {
  if (!fmt) return false;
  if (fmt->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) return true;
  if (WaveFormatIsExtensible(fmt)) {
    const auto* ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(fmt);
    return IsEqualGUID(ext->SubFormat, kWaveSubFormatIeeeFloat) != 0;
  }
  return false;
}

static std::string WaveFormatToString(const WAVEFORMATEX* fmt) {
  if (!fmt) return "<null>";
  const char* type = WaveFormatIsFloat(fmt)   ? "float"
                     : WaveFormatIsPcm(fmt)   ? "pcm"
                     : fmt->wFormatTag == 0x0 ? "unknown"
                                              : "other";

  char buf[256];
  snprintf(buf, sizeof(buf), "tag=0x%04x type=%s rate=%lu ch=%u bits=%u align=%u",
           static_cast<unsigned>(fmt->wFormatTag), type, static_cast<unsigned long>(fmt->nSamplesPerSec),
           static_cast<unsigned>(fmt->nChannels), static_cast<unsigned>(fmt->wBitsPerSample),
           static_cast<unsigned>(fmt->nBlockAlign));
  return std::string(buf);
}

static bool FillToneInterleaved(BYTE* dst, UINT32 frames, const WAVEFORMATEX* fmt, double freq_hz,
                                double* phase_io) {
  if (!dst || !fmt) return false;
  if (fmt->nChannels == 0 || fmt->nBlockAlign == 0) return false;
  const WORD channels = fmt->nChannels;
  const WORD bytes_per_sample = static_cast<WORD>(fmt->nBlockAlign / channels);
  if (bytes_per_sample == 0 || channels * bytes_per_sample != fmt->nBlockAlign) return false;
  if (fmt->nSamplesPerSec == 0) return false;

  const bool is_float = WaveFormatIsFloat(fmt);
  const bool is_pcm = WaveFormatIsPcm(fmt);
  if (!is_float && !is_pcm) return false;

  if (is_float && bytes_per_sample != 4) return false;
  if (is_pcm && bytes_per_sample != 1 && bytes_per_sample != 2 && bytes_per_sample != 3 &&
      bytes_per_sample != 4) {
    return false;
  }

  constexpr double kTwoPi = 6.28318530717958647692;
  constexpr double kAmplitude = 0.20; // -14 dBFS-ish; avoid clipping even with conversion.

  double phase = phase_io ? *phase_io : 0.0;
  const double inc = kTwoPi * freq_hz / static_cast<double>(fmt->nSamplesPerSec);

  for (UINT32 i = 0; i < frames; i++) {
    const double sample = std::sin(phase) * kAmplitude;
    phase += inc;
    if (phase >= kTwoPi) phase -= kTwoPi;

    BYTE* frame = dst + (static_cast<size_t>(i) * fmt->nBlockAlign);
    for (WORD ch = 0; ch < channels; ch++) {
      BYTE* out = frame + (static_cast<size_t>(ch) * bytes_per_sample);
      if (is_float) {
        const float v = static_cast<float>(sample);
        memcpy(out, &v, sizeof(v));
        continue;
      }

      // PCM.
      if (bytes_per_sample == 1) {
        // 8-bit PCM is unsigned [0,255].
        const double clamped = std::max(-1.0, std::min(1.0, sample));
        const uint8_t v = static_cast<uint8_t>(std::lround((clamped * 0.5 + 0.5) * 255.0));
        out[0] = v;
      } else if (bytes_per_sample == 2) {
        const double clamped = std::max(-1.0, std::min(1.0, sample));
        const int16_t v = static_cast<int16_t>(std::lround(clamped * 32767.0));
        memcpy(out, &v, sizeof(v));
      } else if (bytes_per_sample == 3) {
        const double clamped = std::max(-1.0, std::min(1.0, sample));
        const int32_t v = static_cast<int32_t>(std::lround(clamped * 8388607.0));
        out[0] = static_cast<BYTE>(v & 0xFF);
        out[1] = static_cast<BYTE>((v >> 8) & 0xFF);
        out[2] = static_cast<BYTE>((v >> 16) & 0xFF);
      } else if (bytes_per_sample == 4) {
        const double clamped = std::max(-1.0, std::min(1.0, sample));
        const int32_t v = static_cast<int32_t>(std::lround(clamped * 2147483647.0));
        memcpy(out, &v, sizeof(v));
      }
    }
  }

  if (phase_io) *phase_io = phase;
  return true;
}

static TestResult VirtioSndTest(Logger& log, const std::vector<std::wstring>& match_names) {
  TestResult out;

  ScopedCoInitialize com(COINIT_MULTITHREADED);
  if (FAILED(com.hr())) {
    out.fail_reason = "com_init_failed";
    out.hr = com.hr();
    log.Logf("virtio-snd: CoInitializeEx failed hr=0x%08lx", static_cast<unsigned long>(out.hr));
    return out;
  }

  ComPtr<IMMDeviceEnumerator> enumerator;
  HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                                __uuidof(IMMDeviceEnumerator),
                                reinterpret_cast<void**>(enumerator.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "create_device_enumerator_failed";
    out.hr = hr;
    log.Logf("virtio-snd: CoCreateInstance(MMDeviceEnumerator) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  ComPtr<IMMDevice> chosen;
  std::wstring chosen_friendly;
  std::wstring chosen_id;
  int best_score = -1;

  const DWORD deadline_ms = GetTickCount() + 20000;
  int attempt = 0;

  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    attempt++;

    ComPtr<IMMDeviceCollection> collection;
    const DWORD state_mask =
        DEVICE_STATE_ACTIVE | DEVICE_STATE_DISABLED | DEVICE_STATE_NOTPRESENT | DEVICE_STATE_UNPLUGGED;
    hr = enumerator->EnumAudioEndpoints(eRender, state_mask, collection.Put());
    if (FAILED(hr)) {
      log.Logf("virtio-snd: EnumAudioEndpoints(eRender) failed hr=0x%08lx attempt=%d",
               static_cast<unsigned long>(hr), attempt);
      Sleep(1000);
      continue;
    }

    UINT count = 0;
    hr = collection->GetCount(&count);
    if (FAILED(hr)) {
      log.Logf("virtio-snd: IMMDeviceCollection::GetCount failed hr=0x%08lx", static_cast<unsigned long>(hr));
      Sleep(1000);
      continue;
    }

    log.Logf("virtio-snd: render endpoints count=%u attempt=%d", count, attempt);

    best_score = -1;
    chosen.Reset();

    for (UINT i = 0; i < count; i++) {
      ComPtr<IMMDevice> dev;
      hr = collection->Item(i, dev.Put());
      if (FAILED(hr)) continue;

      DWORD state = 0;
      hr = dev->GetState(&state);
      if (FAILED(hr)) state = 0;

      LPWSTR dev_id_raw = nullptr;
      std::wstring dev_id;
      hr = dev->GetId(&dev_id_raw);
      if (SUCCEEDED(hr) && dev_id_raw) {
        dev_id = dev_id_raw;
        CoTaskMemFree(dev_id_raw);
      }

      ComPtr<IPropertyStore> props;
      hr = dev->OpenPropertyStore(STGM_READ, props.Put());

      std::wstring friendly;
      std::wstring instance_id;
      if (SUCCEEDED(hr)) {
        friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
        if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
        instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
      }

      const auto hwids = GetHardwareIdsForInstanceId(instance_id);
      const bool hwid_virtio_snd = IsVirtioSndPciHardwareId(hwids);
      const bool hwid_virtio = IsVirtioHardwareId(hwids);

      log.Logf("virtio-snd: endpoint idx=%u state=%s name=%s id=%s instance_id=%s",
               static_cast<unsigned>(i), MmDeviceStateToString(state), WideToUtf8(friendly).c_str(),
               WideToUtf8(dev_id).c_str(), WideToUtf8(instance_id).c_str());
      if (!hwids.empty()) {
        log.Logf("virtio-snd: endpoint idx=%u hwid0=%s", static_cast<unsigned>(i),
                 WideToUtf8(hwids[0]).c_str());
      }

      if (state != DEVICE_STATE_ACTIVE) continue;

      int score = 0;
      if (ContainsInsensitive(friendly, L"virtio")) score += 100;
      if (ContainsInsensitive(friendly, L"aero")) score += 50;
      if (ContainsInsensitive(friendly, L"snd")) score += 20;
      for (const auto& m : match_names) {
        if (!m.empty() && ContainsInsensitive(friendly, m)) score += 200;
      }
      if (ContainsInsensitive(instance_id, L"DEV_1059") || ContainsInsensitive(instance_id, L"VEN_1AF4&DEV_1059")) {
        score += 150;
      }
      if (ContainsInsensitive(instance_id, L"VEN_1AF4") || ContainsInsensitive(instance_id, L"VIRTIO")) {
        score += 80;
      }
      if (hwid_virtio_snd) score += 200;
      if (hwid_virtio) score += 90;

      if (score <= 0) continue;

      if (score > best_score && LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names)) {
        best_score = score;
        chosen = std::move(dev);
        chosen_friendly = friendly;
        chosen_id = dev_id;
      }
    }

    if (chosen) break;
    Sleep(1000);
  }

  if (!chosen) {
    log.LogLine("virtio-snd: no matching ACTIVE render endpoint found; checking default endpoint");
    hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, chosen.Put());
    if (FAILED(hr) || !chosen) {
      out.fail_reason = "no_matching_endpoint";
      out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
      log.LogLine("virtio-snd: no default render endpoint available");
      return out;
    }

    ComPtr<IPropertyStore> props;
    hr = chosen->OpenPropertyStore(STGM_READ, props.Put());
    std::wstring friendly = L"";
    std::wstring instance_id = L"";
    if (SUCCEEDED(hr)) {
      friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
      if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
      instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
    }
    const auto hwids = GetHardwareIdsForInstanceId(instance_id);
    if (!LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names)) {
      out.fail_reason = "no_matching_endpoint";
      out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
      log.Logf("virtio-snd: default endpoint does not look like virtio-snd (name=%s instance_id=%s)",
               WideToUtf8(friendly).c_str(), WideToUtf8(instance_id).c_str());
      return out;
    }

    best_score = 0;
    chosen_friendly.clear();
    chosen_id.clear();

    LPWSTR dev_id_raw = nullptr;
    hr = chosen->GetId(&dev_id_raw);
    if (SUCCEEDED(hr) && dev_id_raw) {
      chosen_id = dev_id_raw;
      CoTaskMemFree(dev_id_raw);
    }

    props.Reset();
    hr = chosen->OpenPropertyStore(STGM_READ, props.Put());
    if (SUCCEEDED(hr)) {
      chosen_friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
      if (chosen_friendly.empty()) chosen_friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
    }
  }

  log.Logf("virtio-snd: selected endpoint name=%s id=%s score=%d", WideToUtf8(chosen_friendly).c_str(),
           WideToUtf8(chosen_id).c_str(), best_score);

  ComPtr<IAudioClient> client;
  hr = chosen->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                        reinterpret_cast<void**>(client.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "activate_audio_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: IMMDevice::Activate(IAudioClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  constexpr REFERENCE_TIME kBufferDuration100ms = 1000000; // 100ms in 100ns units

  std::vector<BYTE> fmt_bytes;
  fmt_bytes.resize(sizeof(WAVEFORMATEX));
  auto* desired = reinterpret_cast<WAVEFORMATEX*>(fmt_bytes.data());
  *desired = {};
  desired->wFormatTag = WAVE_FORMAT_PCM;
  desired->nChannels = 2;
  desired->nSamplesPerSec = 48000;
  desired->wBitsPerSample = 16;
  desired->nBlockAlign = static_cast<WORD>((desired->nChannels * desired->wBitsPerSample) / 8);
  desired->nAvgBytesPerSec = desired->nSamplesPerSec * desired->nBlockAlign;
  desired->cbSize = 0;

  hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
  if (FAILED(hr)) {
    out.fail_reason = "initialize_shared_failed";
    out.hr = hr;
    log.Logf("virtio-snd: Initialize(shared 48kHz S16 stereo) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  const bool used_desired_format = true;
  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  log.Logf("virtio-snd: stream format=%s", WaveFormatToString(fmt).c_str());

  UINT32 buffer_frames = 0;
  hr = client->GetBufferSize(&buffer_frames);
  if (FAILED(hr) || buffer_frames == 0) {
    out.fail_reason = "get_buffer_size_failed";
    out.hr = FAILED(hr) ? hr : E_FAIL;
    log.Logf("virtio-snd: GetBufferSize failed hr=0x%08lx buffer_frames=%u",
             static_cast<unsigned long>(FAILED(hr) ? hr : E_FAIL), buffer_frames);
    return out;
  }

  ComPtr<IAudioRenderClient> render;
  hr = client->GetService(__uuidof(IAudioRenderClient), reinterpret_cast<void**>(render.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "get_render_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: GetService(IAudioRenderClient) failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }

  ComPtr<IAudioClock> clock;
  hr = client->GetService(__uuidof(IAudioClock), reinterpret_cast<void**>(clock.Put()));
  const bool have_clock = SUCCEEDED(hr) && clock;

  const double sample_rate = static_cast<double>(fmt->nSamplesPerSec);
  const UINT32 tone_frames_total = static_cast<UINT32>(sample_rate * 0.25); // 250ms
  if (tone_frames_total == 0) {
    out.fail_reason = "invalid_format";
    out.hr = E_FAIL;
    log.LogLine("virtio-snd: invalid format (nSamplesPerSec=0)");
    return out;
  }

  const UINT32 prefill = std::min(buffer_frames, tone_frames_total);
  double phase = 0.0;
  UINT32 frames_written = 0;

  if (prefill > 0) {
    BYTE* data = nullptr;
    hr = render->GetBuffer(prefill, &data);
    if (FAILED(hr)) {
      out.fail_reason = "get_buffer_prefill_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::GetBuffer(prefill) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
    if (!FillToneInterleaved(data, prefill, fmt, 440.0, &phase)) {
      render->ReleaseBuffer(prefill, AUDCLNT_BUFFERFLAGS_SILENT);
      out.fail_reason = "unsupported_stream_format";
      out.hr = E_FAIL;
      log.Logf("virtio-snd: unsupported stream format for tone generation: %s", WaveFormatToString(fmt).c_str());
      return out;
    }
    hr = render->ReleaseBuffer(prefill, 0);
    if (FAILED(hr)) {
      out.fail_reason = "release_buffer_prefill_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::ReleaseBuffer(prefill) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
    frames_written += prefill;
  }

  if (prefill < buffer_frames) {
    const UINT32 silent_frames = buffer_frames - prefill;
    BYTE* data = nullptr;
    hr = render->GetBuffer(silent_frames, &data);
    if (FAILED(hr)) {
      out.fail_reason = "get_buffer_silence_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::GetBuffer(silence) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
    hr = render->ReleaseBuffer(silent_frames, AUDCLNT_BUFFERFLAGS_SILENT);
    if (FAILED(hr)) {
      out.fail_reason = "release_buffer_silence_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::ReleaseBuffer(silence) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
  }

  hr = client->Start();
  if (FAILED(hr)) {
    out.fail_reason = "start_failed";
    out.hr = hr;
    log.Logf("virtio-snd: IAudioClient::Start failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }

  UINT64 clock_pos0 = 0;
  if (have_clock) {
    UINT64 qpc0 = 0;
    if (FAILED(clock->GetPosition(&clock_pos0, &qpc0))) {
      clock_pos0 = 0;
    }
  }

  bool padding_changed = false;
  UINT32 last_padding = 0;
  bool have_last_padding = false;

  const DWORD write_deadline = GetTickCount() + 2000;
  while (frames_written < tone_frames_total &&
         static_cast<int32_t>(GetTickCount() - write_deadline) < 0) {
    UINT32 padding = 0;
    hr = client->GetCurrentPadding(&padding);
    if (FAILED(hr)) {
      out.fail_reason = "get_current_padding_failed";
      out.hr = hr;
      log.Logf("virtio-snd: GetCurrentPadding failed hr=0x%08lx", static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }

    if (have_last_padding && padding != last_padding) padding_changed = true;
    have_last_padding = true;
    last_padding = padding;

    const UINT32 available = (padding < buffer_frames) ? (buffer_frames - padding) : 0;
    if (available == 0) {
      Sleep(5);
      continue;
    }

    const UINT32 to_write = std::min(available, tone_frames_total - frames_written);
    BYTE* data = nullptr;
    hr = render->GetBuffer(to_write, &data);
    if (FAILED(hr)) {
      out.fail_reason = "get_buffer_stream_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::GetBuffer(stream) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }
    if (!FillToneInterleaved(data, to_write, fmt, 440.0, &phase)) {
      render->ReleaseBuffer(to_write, AUDCLNT_BUFFERFLAGS_SILENT);
      out.fail_reason = "unsupported_stream_format";
      out.hr = E_FAIL;
      log.Logf("virtio-snd: unsupported stream format for tone generation: %s", WaveFormatToString(fmt).c_str());
      client->Stop();
      return out;
    }
    hr = render->ReleaseBuffer(to_write, 0);
    if (FAILED(hr)) {
      out.fail_reason = "release_buffer_stream_failed";
      out.hr = hr;
      log.Logf("virtio-snd: IAudioRenderClient::ReleaseBuffer(stream) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }
    frames_written += to_write;
  }

  if (frames_written < tone_frames_total) {
    out.fail_reason = "render_timeout";
    out.hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
    log.LogLine("virtio-snd: timed out writing tone frames");
    client->Stop();
    return out;
  }

  const DWORD drain_deadline = GetTickCount() + 1500;
  while (static_cast<int32_t>(GetTickCount() - drain_deadline) < 0) {
    UINT32 padding = 0;
    if (FAILED(client->GetCurrentPadding(&padding))) break;
    if (have_last_padding && padding != last_padding) padding_changed = true;
    have_last_padding = true;
    last_padding = padding;
    if (padding == 0) break;
    Sleep(10);
  }

  if (have_clock && clock_pos0 != 0) {
    UINT64 clock_pos1 = 0;
    UINT64 qpc1 = 0;
    if (SUCCEEDED(clock->GetPosition(&clock_pos1, &qpc1)) && clock_pos1 > clock_pos0) {
      log.Logf("virtio-snd: audio clock advanced pos0=%llu pos1=%llu", clock_pos0, clock_pos1);
    } else {
      log.Logf("virtio-snd: audio clock did not advance (optional check) pos0=%llu pos1=%llu", clock_pos0,
               clock_pos1);
    }
  }

  if (!padding_changed) {
    log.LogLine("virtio-snd: warning: GetCurrentPadding did not change (optional check)");
  }

  hr = client->Stop();
  if (FAILED(hr)) {
    out.fail_reason = "stop_failed";
    out.hr = hr;
    log.Logf("virtio-snd: IAudioClient::Stop failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }
  client->Reset();

  out.ok = true;
  out.hr = S_OK;
  out.fail_reason.clear();
  log.Logf("virtio-snd: render smoke ok (format=%s, used_desired=%d)", WaveFormatToString(fmt).c_str(),
           used_desired_format ? 1 : 0);
  return out;
}

static std::wstring WinmmErrorToWide(MMRESULT rc) {
  wchar_t buf[256]{};
  if (waveOutGetErrorTextW(rc, buf, static_cast<UINT>(sizeof(buf) / sizeof(buf[0]))) ==
      MMSYSERR_NOERROR) {
    return std::wstring(buf);
  }
  return L"";
}

static std::optional<std::wstring> WaveOutDeviceInstanceId(UINT device_id) {
  wchar_t buf[512]{};
  const MMRESULT rc = waveOutMessage(reinterpret_cast<HWAVEOUT>(static_cast<UINT_PTR>(device_id)),
                                     DRV_QUERYDEVICEINSTANCEID, reinterpret_cast<DWORD_PTR>(buf),
                                     sizeof(buf));
  if (rc != MMSYSERR_NOERROR) return std::nullopt;
  buf[(sizeof(buf) / sizeof(buf[0])) - 1] = L'\0';
  if (buf[0] == L'\0') return std::nullopt;
  return std::wstring(buf);
}

static bool WaveOutToneTest(Logger& log, const std::vector<std::wstring>& match_names) {
  const UINT num = waveOutGetNumDevs();
  log.Logf("virtio-snd: waveOut devices=%u", num);
  if (num == 0) return false;

  auto name_matches = [&](const std::wstring& n) -> bool {
    if (ContainsInsensitive(n, L"virtio") || ContainsInsensitive(n, L"aero")) return true;
    for (const auto& m : match_names) {
      if (!m.empty() && ContainsInsensitive(n, m)) return true;
    }
    return false;
  };

  UINT device_id = UINT_MAX;
  int best_score = 0;
  for (UINT i = 0; i < num; i++) {
    WAVEOUTCAPSW caps{};
    const MMRESULT rc = waveOutGetDevCapsW(i, &caps, sizeof(caps));
    if (rc != MMSYSERR_NOERROR) continue;

    int score = 0;
    if (name_matches(caps.szPname)) score += 100;

    const auto inst_id = WaveOutDeviceInstanceId(i);
    if (inst_id.has_value()) {
      log.Logf("virtio-snd: waveOut[%u]=%s instance_id=%s", i, WideToUtf8(caps.szPname).c_str(),
               WideToUtf8(*inst_id).c_str());
      if (ContainsInsensitive(*inst_id, L"DEV_1059") || ContainsInsensitive(*inst_id, L"VEN_1AF4&DEV_1059")) {
        score += 500;
      }
      const auto hwids = GetHardwareIdsForInstanceId(*inst_id);
      if (IsVirtioSndPciHardwareId(hwids)) score += 1000;
      if (IsVirtioHardwareId(hwids)) score += 200;
    } else {
      log.Logf("virtio-snd: waveOut[%u]=%s instance_id=<unavailable>", i,
               WideToUtf8(caps.szPname).c_str());
    }

    if (score > best_score) {
      best_score = score;
      device_id = i;
    }
  }

  if (device_id == UINT_MAX || best_score <= 0) {
    log.LogLine("virtio-snd: waveOut no matching device found");
    return false;
  } else {
    log.Logf("virtio-snd: waveOut using device_id=%u score=%d", device_id, best_score);
  }

  HANDLE done_event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (!done_event) {
    log.Logf("virtio-snd: CreateEvent failed err=%lu", GetLastError());
    return false;
  }

  WAVEFORMATEX fmt{};
  fmt.wFormatTag = WAVE_FORMAT_PCM;
  fmt.nChannels = 2;
  fmt.nSamplesPerSec = 48000;
  fmt.wBitsPerSample = 16;
  fmt.nBlockAlign = static_cast<WORD>((fmt.nChannels * fmt.wBitsPerSample) / 8);
  fmt.nAvgBytesPerSec = fmt.nSamplesPerSec * fmt.nBlockAlign;

  HWAVEOUT hwo = nullptr;
  MMRESULT rc =
      waveOutOpen(&hwo, device_id, &fmt, reinterpret_cast<DWORD_PTR>(done_event), 0, CALLBACK_EVENT);
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutOpen failed rc=%u text=%s", rc,
             WideToUtf8(WinmmErrorToWide(rc)).c_str());
    CloseHandle(done_event);
    return false;
  }
  ResetEvent(done_event);

  const UINT32 frames = fmt.nSamplesPerSec / 4; // 250ms
  std::vector<BYTE> data(static_cast<size_t>(frames) * fmt.nBlockAlign);
  double phase = 0.0;
  if (!FillToneInterleaved(data.data(), frames, &fmt, 440.0, &phase)) {
    log.LogLine("virtio-snd: waveOut tone generation failed");
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  WAVEHDR hdr{};
  hdr.lpData = reinterpret_cast<LPSTR>(data.data());
  hdr.dwBufferLength = static_cast<DWORD>(data.size());

  rc = waveOutPrepareHeader(hwo, &hdr, sizeof(hdr));
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutPrepareHeader failed rc=%u text=%s", rc,
             WideToUtf8(WinmmErrorToWide(rc)).c_str());
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  rc = waveOutWrite(hwo, &hdr, sizeof(hdr));
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutWrite failed rc=%u text=%s", rc,
             WideToUtf8(WinmmErrorToWide(rc)).c_str());
    waveOutUnprepareHeader(hwo, &hdr, sizeof(hdr));
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  const DWORD wait_rc = WaitForSingleObject(done_event, 5000);
  if (wait_rc != WAIT_OBJECT_0) {
    log.Logf("virtio-snd: waveOut timed out wait_rc=%lu", wait_rc);
    waveOutReset(hwo);
    waveOutUnprepareHeader(hwo, &hdr, sizeof(hdr));
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  waveOutReset(hwo);
  waveOutUnprepareHeader(hwo, &hdr, sizeof(hdr));
  waveOutClose(hwo);
  CloseHandle(done_event);
  log.LogLine("virtio-snd: waveOut playback ok");
  return true;
}

static void PrintUsage() {
  printf(
      "aero-virtio-selftest.exe [options]\n"
      "\n"
      "Options:\n"
      "  --blk-root <path>         Directory to use for virtio-blk file I/O test\n"
      "  --http-url <url>          HTTP URL for TCP connectivity test\n"
      "  --dns-host <hostname>     Hostname for DNS resolution test\n"
      "  --log-file <path>         Log file path (default C:\\\\aero-virtio-selftest.log)\n"
      "  --disable-snd             Skip virtio-snd test (emit SKIP)\n"
      "  --require-snd             Fail if virtio-snd is missing (default: SKIP)\n"
      "  --net-timeout-sec <sec>   Wait time for DHCP/link\n"
      "  --io-size-mib <mib>       virtio-blk test file size\n"
      "  --io-chunk-kib <kib>      virtio-blk chunk size\n"
      "  --help                    Show this help\n");
}

static std::optional<uint32_t> ParseU32(const wchar_t* s) {
  if (!s || !*s) return std::nullopt;
  wchar_t* end = nullptr;
  unsigned long val = wcstoul(s, &end, 10);
  if (end == s || *end != L'\0') return std::nullopt;
  return static_cast<uint32_t>(val);
}

} // namespace

int wmain(int argc, wchar_t** argv) {
  // Avoid interactive error dialogs that can hang headless/automation runs.
  SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);

  Options opt;

  for (int i = 1; i < argc; i++) {
    const std::wstring arg = argv[i];
    auto next = [&]() -> const wchar_t* {
      if (i + 1 >= argc) return nullptr;
      return argv[++i];
    };

    if (arg == L"--help" || arg == L"-h" || arg == L"/?") {
      PrintUsage();
      return 0;
    } else if (arg == L"--http-url") {
      const wchar_t* v = next();
      if (!v) {
        PrintUsage();
        return 2;
      }
      opt.http_url = v;
    } else if (arg == L"--blk-root") {
      const wchar_t* v = next();
      if (!v) {
        PrintUsage();
        return 2;
      }
      opt.blk_root = v;
    } else if (arg == L"--dns-host") {
      const wchar_t* v = next();
      if (!v) {
        PrintUsage();
        return 2;
      }
      opt.dns_host = v;
    } else if (arg == L"--log-file") {
      const wchar_t* v = next();
      if (!v) {
        PrintUsage();
        return 2;
      }
      opt.log_file = v;
    } else if (arg == L"--disable-snd") {
      opt.disable_snd = true;
    } else if (arg == L"--require-snd") {
      opt.require_snd = true;
    } else if (arg == L"--net-timeout-sec") {
      const wchar_t* v = next();
      const auto parsed = ParseU32(v);
      if (!parsed) {
        PrintUsage();
        return 2;
      }
      opt.net_timeout_sec = *parsed;
    } else if (arg == L"--io-size-mib") {
      const wchar_t* v = next();
      const auto parsed = ParseU32(v);
      if (!parsed) {
        PrintUsage();
        return 2;
      }
      opt.io_file_size_mib = *parsed;
    } else if (arg == L"--io-chunk-kib") {
      const wchar_t* v = next();
      const auto parsed = ParseU32(v);
      if (!parsed) {
        PrintUsage();
        return 2;
      }
      opt.io_chunk_kib = *parsed;
    } else {
      printf("unknown arg: %ls\n", arg.c_str());
      PrintUsage();
      return 2;
    }
  }

  if (opt.disable_snd && opt.require_snd) {
    printf("--disable-snd and --require-snd cannot both be set\n");
    PrintUsage();
    return 2;
  }

  Logger log(opt.log_file);

  log.LogLine("AERO_VIRTIO_SELFTEST|START|version=1");
  log.Logf("AERO_VIRTIO_SELFTEST|CONFIG|http_url=%s|dns_host=%s|blk_root=%s",
           WideToUtf8(opt.http_url).c_str(), WideToUtf8(opt.dns_host).c_str(),
           WideToUtf8(opt.blk_root).c_str());

  bool all_ok = true;

  const bool blk_ok = VirtioBlkTest(log, opt);
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk|%s", blk_ok ? "PASS" : "FAIL");
  all_ok = all_ok && blk_ok;

  const auto input = VirtioInputTest(log);
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input|%s|devices=%d|keyboard_devices=%d|"
           "mouse_devices=%d|ambiguous_devices=%d|unknown_devices=%d|keyboard_collections=%d|"
           "mouse_collections=%d|reason=%s",
           input.ok ? "PASS" : "FAIL", input.matched_devices, input.keyboard_devices, input.mouse_devices,
           input.ambiguous_devices, input.unknown_devices, input.keyboard_collections, input.mouse_collections,
           input.reason.empty() ? "-" : input.reason.c_str());
  all_ok = all_ok && input.ok;

  if (opt.disable_snd) {
    log.LogLine("virtio-snd: disabled by --disable-snd");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
  } else {
    const auto snd_pci = DetectVirtioSndPciDevices(log);
    if (snd_pci.empty()) {
      log.LogLine("virtio-snd: PCI\\VEN_1AF4&DEV_1059 device not detected");
      if (opt.require_snd) {
        log.LogLine("virtio-snd: --require-snd set; failing");
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL");
        all_ok = false;
      } else {
        log.LogLine("virtio-snd: skipping (use --require-snd to require device)");
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
      }
    } else {
      std::vector<std::wstring> match_names;
      for (const auto& d : snd_pci) {
        if (!d.description.empty()) match_names.push_back(d.description);
      }

      bool snd_ok = false;
      const auto snd = VirtioSndTest(log, match_names);
      if (snd.ok) {
        snd_ok = true;
      } else {
        log.Logf("virtio-snd: WASAPI failed reason=%s hr=0x%08lx",
                 snd.fail_reason.empty() ? "unknown" : snd.fail_reason.c_str(),
                 static_cast<unsigned long>(snd.hr));
        log.LogLine("virtio-snd: trying waveOut fallback");
        snd_ok = WaveOutToneTest(log, match_names);
      }

      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|%s", snd_ok ? "PASS" : "FAIL");
      all_ok = all_ok && snd_ok;
    }
  }

  // Network tests require Winsock initialized for getaddrinfo.
  WSADATA wsa{};
  const int wsa_rc = WSAStartup(MAKEWORD(2, 2), &wsa);
  if (wsa_rc != 0) {
    log.Logf("virtio-net: WSAStartup failed rc=%d", wsa_rc);
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL");
    all_ok = false;
  } else {
    const bool net_ok = VirtioNetTest(log, opt);
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net|%s", net_ok ? "PASS" : "FAIL");
    all_ok = all_ok && net_ok;
    WSACleanup();
  }

  log.Logf("AERO_VIRTIO_SELFTEST|RESULT|%s", all_ok ? "PASS" : "FAIL");
  return all_ok ? 0 : 1;
}
