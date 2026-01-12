// SPDX-License-Identifier: MIT OR Apache-2.0
//
// aero-virtio-selftest: Windows 7 user-mode functional tests for Aero virtio drivers.
// Primary targets: virtio-blk + virtio-net + virtio-input + virtio-snd. Output is written to stdout, a log file, and
// COM1.

#include <windows.h>

#include <audioclient.h>
#include <audiopolicy.h>
#include <cfgmgr32.h>
#include <endpointvolume.h>
#include <functiondiscoverykeys_devpkey.h>
#include <mmdeviceapi.h>
#include <mmsystem.h>
#include <mmddk.h>
#include <propsys.h>
#include <setupapi.h>
#include <winsvc.h>

#include <devguid.h>
#include <initguid.h>
#include <iphlpapi.h>
#include <ntddstor.h>
#include <winioctl.h>
#include <ntddscsi.h>
#include <winhttp.h>
#include <ws2tcpip.h>

#include <algorithm>
#include <cmath>
#include <climits>
#include <cstdarg>
#include <cstddef>
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
  // Skip the virtio-snd test (emits a SKIP marker).
  bool disable_snd = false;
  // Skip the virtio-snd capture test (emits a SKIP marker).
  bool disable_snd_capture = false;
  // If set, missing virtio-snd device causes the overall selftest to fail (instead of SKIP).
  bool require_snd = false;
  // If set, missing virtio-snd capture endpoint causes the overall selftest to fail (instead of SKIP).
  bool require_snd_capture = false;
  // If set, run a capture smoke test when a virtio-snd capture endpoint is present.
  bool test_snd_capture = false;
  // Allow matching virtio-snd transitional PCI IDs (PCI\VEN_1AF4&DEV_1018). Aero contract v1 is modern-only.
  bool allow_virtio_snd_transitional = false;
  // When running a capture smoke test, require at least one non-silent capture buffer.
  bool require_non_silence = false;
  // If set, run a WASAPI stress test that attempts to initialize a render stream with an intentionally large
  // buffer duration/period. This is used to exercise virtio-snd buffer sizing limits.
  bool test_snd_buffer_limits = false;

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

static bool StartsWithInsensitive(const std::wstring& s, const std::wstring& prefix) {
  if (s.size() < prefix.size()) return false;
  for (size_t i = 0; i < prefix.size(); i++) {
    if (static_cast<wchar_t>(towlower(s[i])) != static_cast<wchar_t>(towlower(prefix[i]))) return false;
  }
  return true;
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

static bool EqualsInsensitive(const std::wstring& a, const std::wstring& b) { return ToLower(a) == ToLower(b); }

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

static size_t BoundedWcsLen(const wchar_t* s, size_t max_len) {
  if (!s) return 0;
  size_t i = 0;
  for (; i < max_len; i++) {
    if (s[i] == L'\0') break;
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
  // For endpoint-based tests (virtio-snd render/capture), indicates an endpoint was selected.
  bool endpoint_found = false;
  // Capture-only diagnostics (only meaningful when a smoke test runs).
  bool captured_silence_only = false;
  bool captured_non_silence = false;
  UINT64 captured_frames = 0;
};

struct StorageIdStrings {
  STORAGE_BUS_TYPE bus_type = BusTypeUnknown;
  std::wstring vendor;
  std::wstring product;
  std::wstring revision;
};

// Userspace mirror of `drivers/windows7/virtio-blk/include/aero_virtio_blk.h` IOCTL contract.
static constexpr const char kAerovblkSrbIoSig[8] = {'A', 'E', 'R', 'O', 'V', 'B', 'L', 'K'};
static constexpr ULONG kAerovblkIoctlQuery = 0x8000A001u;

struct AEROVBLK_QUERY_INFO {
  ULONGLONG NegotiatedFeatures;
  USHORT QueueSize;
  USHORT NumFree;
  USHORT AvailIdx;
  USHORT UsedIdx;
};

static std::string VirtioFeaturesToString(ULONGLONG f) {
  char buf[64];
  // Windows 7 MSVCRT lacks `%llx` in some configurations; use I64 explicitly.
  snprintf(buf, sizeof(buf), "0x%I64x", static_cast<unsigned long long>(f));
  return std::string(buf);
}

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

static HANDLE OpenPhysicalDriveForIoctl(Logger& log, DWORD disk_number) {
  wchar_t path[64];
  swprintf_s(path, L"\\\\.\\PhysicalDrive%lu", static_cast<unsigned long>(disk_number));

  const DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  const DWORD flags = FILE_ATTRIBUTE_NORMAL;
  const DWORD desired_accesses[] = {GENERIC_READ | GENERIC_WRITE, GENERIC_READ, 0};
  for (const DWORD access : desired_accesses) {
    HANDLE h = CreateFileW(path, access, share, nullptr, OPEN_EXISTING, flags, nullptr);
    if (h != INVALID_HANDLE_VALUE) {
      return h;
    }
  }
  log.Logf("virtio-blk: CreateFile(PhysicalDrive%lu) failed err=%lu", static_cast<unsigned long>(disk_number),
           GetLastError());
  return INVALID_HANDLE_VALUE;
}

static std::optional<AEROVBLK_QUERY_INFO> QueryAerovblkMiniportInfo(Logger& log, HANDLE hPhysicalDrive) {
  if (hPhysicalDrive == INVALID_HANDLE_VALUE) return std::nullopt;

  std::vector<BYTE> buf(sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO));
  auto* ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  ctrl->HeaderLength = sizeof(SRB_IO_CONTROL);
  memcpy(ctrl->Signature, kAerovblkSrbIoSig, sizeof(ctrl->Signature));
  ctrl->Timeout = 10;
  ctrl->ControlCode = kAerovblkIoctlQuery;
  ctrl->ReturnCode = 0;
  ctrl->Length = sizeof(AEROVBLK_QUERY_INFO);

  DWORD bytes = 0;
  if (!DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_MINIPORT, buf.data(), static_cast<DWORD>(buf.size()),
                       buf.data(), static_cast<DWORD>(buf.size()), &bytes, nullptr)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT(AEROVBLK_IOCTL_QUERY) failed err=%lu", GetLastError());
    return std::nullopt;
  }
  if (bytes < sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned too few bytes=%lu", bytes);
    return std::nullopt;
  }

  ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  if (ctrl->ReturnCode != 0) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned ReturnCode=0x%08lx", ctrl->ReturnCode);
    return std::nullopt;
  }
  if (ctrl->Length < sizeof(AEROVBLK_QUERY_INFO)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned Length=%lu (expected >=%zu)", ctrl->Length,
             sizeof(AEROVBLK_QUERY_INFO));
    return std::nullopt;
  }

  const auto* info = reinterpret_cast<const AEROVBLK_QUERY_INFO*>(buf.data() + sizeof(SRB_IO_CONTROL));
  return *info;
}

static bool ValidateAerovblkMiniportInfo(Logger& log, const AEROVBLK_QUERY_INFO& info) {
  const ULONGLONG required_features =
      (1ull << 32) | // VIRTIO_F_VERSION_1
      (1ull << 28) | // VIRTIO_F_RING_INDIRECT_DESC
      (1ull << 2) |  // VIRTIO_BLK_F_SEG_MAX
      (1ull << 6) |  // VIRTIO_BLK_F_BLK_SIZE
      (1ull << 9);   // VIRTIO_BLK_F_FLUSH

  if (info.QueueSize != 128) {
    log.Logf("virtio-blk: miniport query FAIL QueueSize=%u (expected 128)", info.QueueSize);
    return false;
  }
  if ((info.NegotiatedFeatures & required_features) != required_features) {
    const ULONGLONG missing = required_features & ~info.NegotiatedFeatures;
    log.Logf("virtio-blk: miniport query FAIL NegotiatedFeatures=%s missing=%s",
             VirtioFeaturesToString(info.NegotiatedFeatures).c_str(), VirtioFeaturesToString(missing).c_str());
    return false;
  }
  if (info.NumFree > info.QueueSize) {
    log.Logf("virtio-blk: miniport query FAIL NumFree=%u > QueueSize=%u", info.NumFree, info.QueueSize);
    return false;
  }

  log.Logf("virtio-blk: miniport query PASS queue_size=%u num_free=%u avail_idx=%u used_idx=%u features=%s",
           info.QueueSize, info.NumFree, info.AvailIdx, info.UsedIdx,
           VirtioFeaturesToString(info.NegotiatedFeatures).c_str());
  return true;
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
  const wchar_t* end = p + total_wchars;

  if (reg_type == REG_SZ) {
    const size_t len = BoundedWcsLen(p, total_wchars);
    if (len == 0) return {};
    return {std::wstring(p, p + len)};
  }

  std::vector<std::wstring> out;
  while (p < end && *p) {
    const size_t len = BoundedWcsLen(p, static_cast<size_t>(end - p));
    if (len == 0 || p + len >= end) break;
    out.emplace_back(p, p + len);
    p += len + 1;
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

struct VirtioSndPciIdInfo {
  bool modern = false;
  bool modern_rev01 = false;
  bool transitional = false;
};

static VirtioSndPciIdInfo GetVirtioSndPciIdInfoFromString(const std::wstring& s) {
  VirtioSndPciIdInfo out{};
  if (StartsWithInsensitive(s, L"PCI\\VEN_1AF4&DEV_1059")) {
    out.modern = true;
    // The Aero contract v1 in-tree INF matches PCI\VEN_1AF4&DEV_1059&REV_01, but some callers may only surface the
    // device+subsystem IDs. Treat REV_01 as a "nice to have" signal for logging/scoring.
    if (ContainsInsensitive(s, L"&REV_01")) out.modern_rev01 = true;
  }
  if (StartsWithInsensitive(s, L"PCI\\VEN_1AF4&DEV_1018")) {
    out.transitional = true;
  }
  return out;
}

static VirtioSndPciIdInfo GetVirtioSndPciIdInfoFromHwids(const std::vector<std::wstring>& hwids) {
  VirtioSndPciIdInfo out{};
  for (const auto& id : hwids) {
    const auto info = GetVirtioSndPciIdInfoFromString(id);
    out.modern = out.modern || info.modern;
    out.modern_rev01 = out.modern_rev01 || info.modern_rev01;
    out.transitional = out.transitional || info.transitional;
  }
  return out;
}

static bool IsAllowedVirtioSndPciId(const VirtioSndPciIdInfo& info, bool allow_transitional) {
  if (info.modern) return true;
  return allow_transitional && info.transitional;
}

static bool IsAllowedVirtioSndPciHardwareId(const std::vector<std::wstring>& hwids, bool allow_transitional,
                                            VirtioSndPciIdInfo* info_out = nullptr) {
  const auto info = GetVirtioSndPciIdInfoFromHwids(hwids);
  if (info_out) *info_out = info;
  return IsAllowedVirtioSndPciId(info, allow_transitional);
}

static constexpr const wchar_t* kVirtioSndExpectedServiceModern = L"aero_virtio_snd";
static constexpr const wchar_t* kVirtioSndExpectedServiceTransitional = L"aeroviosnd_legacy";

static const char* CmProblemCodeToName(DWORD code) {
  switch (code) {
    case MAXDWORD:
      return "STATUS_QUERY_FAILED";
    case 0:
      return "OK";
    case 1:
      return "NOT_CONFIGURED";
    case 2:
      return "DEVLOADER_FAILED";
    case 3:
      return "OUT_OF_MEMORY";
    case 4:
      return "ENTRY_IS_WRONG_TYPE";
    case 5:
      return "LACKED_ARBITRATOR";
    case 6:
      return "BOOT_CONFIG_CONFLICT";
    case 7:
      return "FAILED_FILTER";
    case 8:
      return "DEVLOADER_NOT_FOUND";
    case 9:
      return "INVALID_DATA";
    case 10:
      return "FAILED_START";
    case 11:
      return "LIAR";
    case 12:
      return "NORMAL_CONFLICT";
    case 13:
      return "NOT_VERIFIED";
    case 14:
      return "NEED_RESTART";
    case 15:
      return "REENUMERATION";
    case 16:
      return "PARTIAL_LOG_CONF";
    case 17:
      return "UNKNOWN_RESOURCE";
    case 18:
      return "REINSTALL";
    case 19:
      return "REGISTRY";
    case 20:
      return "VXDLDR";
    case 21:
      return "WILL_BE_REMOVED";
    case 22:
      return "DISABLED";
    case 23:
      return "DEVLOADER_NOT_READY";
    case 24:
      return "DEVICE_NOT_THERE";
    case 25:
      return "MOVED";
    case 26:
      return "TOO_EARLY";
    case 27:
      return "NO_VALID_LOG_CONF";
    case 28:
      return "FAILED_INSTALL";
    case 29:
      return "HARDWARE_DISABLED";
    case 30:
      return "CANT_SHARE_IRQ";
    case 31:
      return "FAILED_ADD";
    case 32:
      return "DISABLED_SERVICE";
    case 33:
      return "TRANSLATION_FAILED";
    case 34:
      return "NO_SOFTCONFIG";
    case 35:
      return "BIOS_TABLE";
    case 36:
      return "IRQ_TRANSLATION_FAILED";
    case 37:
      return "FAILED_DRIVER_ENTRY";
    case 38:
      return "DRIVER_FAILED_PRIOR_UNLOAD";
    case 39:
      return "DRIVER_FAILED_LOAD";
    case 40:
      return "DRIVER_SERVICE_KEY_INVALID";
    case 41:
      return "LEGACY_SERVICE_NO_DEVICES";
    case 42:
      return "DUPLICATE_DEVICE";
    case 43:
      return "FAILED_POST_START";
    case 44:
      return "HALTED";
    case 45:
      return "PHANTOM";
    case 46:
      return "SYSTEM_SHUTDOWN";
    case 47:
      return "HELD_FOR_EJECT";
    case 48:
      return "DRIVER_BLOCKED";
    case 49:
      return "REGISTRY_TOO_LARGE";
    case 50:
      return "SETPROPERTIES_FAILED";
    case 51:
      return "WAITING_ON_DEPENDENCY";
    case 52:
      return "UNSIGNED_DRIVER";
    default:
      return "UNKNOWN";
  }
}

static const char* CmProblemCodeToMeaning(DWORD code) {
  switch (code) {
    case MAXDWORD:
      return "CM_Get_DevNode_Status failed";
    case 0:
      return "device started";
    case 1:
      return "device is not configured";
    case 2:
      return "devloader failed";
    case 3:
      return "out of memory";
    case 4:
      return "device entry is wrong type";
    case 5:
      return "device lacked an arbitrator";
    case 6:
      return "boot configuration conflict";
    case 7:
      return "filter failed";
    case 8:
      return "devloader not found";
    case 9:
      return "invalid device data";
    case 10:
      return "device cannot start";
    case 11:
      return "device reported invalid data";
    case 12:
      return "resource conflict";
    case 13:
      return "driver/device could not be verified";
    case 14:
      return "requires restart";
    case 15:
      return "reenumeration required";
    case 16:
      return "partial log configuration";
    case 17:
      return "unknown resource";
    case 18:
      return "reinstall the drivers for this device";
    case 19:
      return "registry error";
    case 20:
      return "VxD loader error";
    case 21:
      return "device will be removed";
    case 22:
      return "device is disabled";
    case 23:
      return "devloader not ready";
    case 24:
      return "device is not present / not working properly";
    case 25:
      return "device moved";
    case 26:
      return "device enumerated too early";
    case 27:
      return "no valid log configuration";
    case 28:
      return "drivers for this device are not installed";
    case 29:
      return "hardware disabled";
    case 30:
      return "can't share IRQ";
    case 31:
      return "device could not be added";
    case 32:
      return "driver service is disabled";
    case 33:
      return "resource translation failed";
    case 34:
      return "no soft configuration";
    case 35:
      return "BIOS table problem";
    case 36:
      return "IRQ translation failed";
    case 37:
      return "failed driver entry";
    case 38:
      return "driver failed prior unload";
    case 39:
      return "driver failed to load";
    case 40:
      return "driver service key invalid";
    case 41:
      return "legacy service has no associated devices";
    case 42:
      return "duplicate device";
    case 43:
      return "failed post-start";
    case 44:
      return "device halted";
    case 45:
      return "phantom device";
    case 46:
      return "system shutdown";
    case 47:
      return "held for eject";
    case 48:
      return "driver blocked";
    case 49:
      return "registry too large";
    case 50:
      return "failed to set device properties";
    case 51:
      return "waiting on a dependency";
    case 52:
      return "driver is unsigned (enable test signing / install a signed driver)";
    default:
      return "";
  }
}

static std::string CmStatusFlagsToString(ULONG status) {
  std::string out;
  auto add = [&](const char* s) {
    if (!out.empty()) out.push_back('|');
    out.append(s);
  };
  auto add_flag = [&](ULONG flag, const char* name) {
    if (status & flag) add(name);
  };

  add_flag(DN_STARTED, "STARTED");
  add_flag(DN_DRIVER_LOADED, "DRIVER_LOADED");
  add_flag(DN_HAS_PROBLEM, "HAS_PROBLEM");
  add_flag(DN_DISABLED, "DISABLED");
  add_flag(DN_REMOVABLE, "REMOVABLE");
  add_flag(DN_PRIVATE_PROBLEM, "PRIVATE_PROBLEM");
  add_flag(DN_MF_PARENT, "MF_PARENT");
#ifdef DN_MF_CHILD
  add_flag(DN_MF_CHILD, "MF_CHILD");
#endif
#ifdef DN_DISABLEABLE
  add_flag(DN_DISABLEABLE, "DISABLEABLE");
#endif
#ifdef DN_WILL_BE_REMOVED
  add_flag(DN_WILL_BE_REMOVED, "WILL_BE_REMOVED");
#endif
#ifdef DN_NO_SHOW_IN_DM
  add_flag(DN_NO_SHOW_IN_DM, "NO_SHOW_IN_DM");
#endif
#ifdef DN_DRIVER_BLOCKED
  add_flag(DN_DRIVER_BLOCKED, "DRIVER_BLOCKED");
#endif
#ifdef DN_NEED_TO_ENUM
  add_flag(DN_NEED_TO_ENUM, "NEED_TO_ENUM");
#endif
#ifdef DN_NOT_FIRST_TIME
  add_flag(DN_NOT_FIRST_TIME, "NOT_FIRST_TIME");
#endif
#ifdef DN_HARDWARE_ENUM
  add_flag(DN_HARDWARE_ENUM, "HARDWARE_ENUM");
#endif
#ifdef DN_ROOT_ENUMERATED
  add_flag(DN_ROOT_ENUMERATED, "ROOT_ENUMERATED");
#endif
  if (out.empty()) out = "0";
  return out;
}

static std::optional<std::wstring> QueryDeviceDriverRegString(HDEVINFO devinfo, SP_DEVINFO_DATA* dev,
                                                              const wchar_t* value_name) {
  if (!devinfo || devinfo == INVALID_HANDLE_VALUE || !dev || !value_name) return std::nullopt;

  HKEY key = SetupDiOpenDevRegKey(devinfo, dev, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_QUERY_VALUE);
  if (key == INVALID_HANDLE_VALUE) return std::nullopt;

  DWORD type = 0;
  DWORD bytes = 0;
  LONG rc = RegQueryValueExW(key, value_name, nullptr, &type, nullptr, &bytes);
  if (rc != ERROR_SUCCESS || bytes == 0 || (type != REG_SZ && type != REG_EXPAND_SZ)) {
    RegCloseKey(key);
    return std::nullopt;
  }

  std::vector<wchar_t> buf((bytes / sizeof(wchar_t)) + 1, L'\0');
  rc = RegQueryValueExW(key, value_name, nullptr, &type, reinterpret_cast<LPBYTE>(buf.data()), &bytes);
  RegCloseKey(key);
  if (rc != ERROR_SUCCESS) return std::nullopt;
  buf.back() = L'\0';
  if (buf[0] == L'\0') return std::nullopt;
  return std::wstring(buf.data());
}

static std::optional<DWORD> QueryDeviceDevRegDword(HDEVINFO devinfo, SP_DEVINFO_DATA* dev, const wchar_t* value_name) {
  if (!devinfo || devinfo == INVALID_HANDLE_VALUE || !dev || !value_name) return std::nullopt;

  HKEY key = SetupDiOpenDevRegKey(devinfo, dev, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_QUERY_VALUE);
  if (key == INVALID_HANDLE_VALUE) return std::nullopt;

  DWORD type = 0;
  DWORD data = 0;
  DWORD bytes = sizeof(data);
  const LONG rc = RegQueryValueExW(key, value_name, nullptr, &type, reinterpret_cast<LPBYTE>(&data), &bytes);
  RegCloseKey(key);
  if (rc != ERROR_SUCCESS || type != REG_DWORD || bytes < sizeof(DWORD)) return std::nullopt;
  return data;
}

struct VirtioSndPciDevice {
  std::wstring instance_id;
  std::wstring description;
  std::vector<std::wstring> hwids;
  std::wstring service;
  std::wstring inf_path;
  std::wstring inf_section;
  std::wstring driver_desc;
  std::wstring provider_name;
  std::wstring driver_version;
  std::wstring driver_date;
  std::wstring matching_device_id;
  DWORD cm_problem = 0;
  ULONG cm_status = 0;
  bool is_modern = false;
  bool has_rev_01 = false;
  bool is_transitional = false;
  std::optional<DWORD> force_null_backend;
};

// KSCATEGORY_TOPOLOGY {DDA54A40-1E4C-11D1-A050-405705C10000}
static const GUID kKsCategoryTopology = {0xdda54a40,
                                          0x1e4c,
                                          0x11d1,
                                          {0xa0, 0x50, 0x40, 0x57, 0x05, 0xc1, 0x00, 0x00}};

static std::vector<VirtioSndPciDevice> DetectVirtioSndPciDevices(Logger& log, bool allow_transitional,
                                                                 bool verbose = true) {
  std::vector<VirtioSndPciDevice> out;
  std::vector<VirtioSndPciDevice> ignored_transitional;

  HDEVINFO devinfo =
      // Restrict to PCI enumerated devices for speed/determinism. The virtio-snd function is a PCI
      // function, so it should always show up here if present.
      SetupDiGetClassDevsW(nullptr, L"PCI", nullptr, DIGCF_PRESENT | DIGCF_ALLCLASSES);
  if (devinfo == INVALID_HANDLE_VALUE) {
    if (verbose) {
      log.Logf("virtio-snd: SetupDiGetClassDevs(enumerator=PCI) failed: %lu", GetLastError());
    }
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
    VirtioSndPciIdInfo id_info{};
    const bool allowed = IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional, &id_info);
    if (!id_info.modern && !id_info.transitional) continue;

    VirtioSndPciDevice snd{};
    snd.hwids = hwids;
    snd.is_modern = id_info.modern;
    snd.has_rev_01 = id_info.modern_rev01;
    snd.is_transitional = id_info.transitional;
    if (auto inst = GetDeviceInstanceIdString(devinfo, &dev)) {
      snd.instance_id = *inst;
    }
    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      snd.description = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      snd.description = *desc;
    }

    if (auto svc = GetDevicePropertyString(devinfo, &dev, SPDRP_SERVICE)) {
      snd.service = *svc;
    }
    if (auto inf = QueryDeviceDriverRegString(devinfo, &dev, L"InfPath")) {
      snd.inf_path = *inf;
    }
    if (auto sec = QueryDeviceDriverRegString(devinfo, &dev, L"InfSection")) {
      snd.inf_section = *sec;
    }
    if (auto desc = QueryDeviceDriverRegString(devinfo, &dev, L"DriverDesc")) {
      snd.driver_desc = *desc;
    }
    if (auto provider = QueryDeviceDriverRegString(devinfo, &dev, L"ProviderName")) {
      snd.provider_name = *provider;
    }
    if (auto ver = QueryDeviceDriverRegString(devinfo, &dev, L"DriverVersion")) {
      snd.driver_version = *ver;
    }
    if (auto date = QueryDeviceDriverRegString(devinfo, &dev, L"DriverDate")) {
      snd.driver_date = *date;
    }
    if (auto match = QueryDeviceDriverRegString(devinfo, &dev, L"MatchingDeviceId")) {
      snd.matching_device_id = *match;
    }
    if (auto force = QueryDeviceDevRegDword(devinfo, &dev, L"ForceNullBackend")) {
      snd.force_null_backend = *force;
    }

    ULONG status = 0;
    ULONG problem = 0;
    const CONFIGRET cr = CM_Get_DevNode_Status(&status, &problem, dev.DevInst, 0);
    if (cr == CR_SUCCESS) {
      snd.cm_status = status;
      snd.cm_problem = static_cast<DWORD>(problem);
    } else {
      if (verbose) {
        log.Logf("virtio-snd: CM_Get_DevNode_Status failed pnp_id=%s cr=%lu",
                 WideToUtf8(snd.instance_id).c_str(), static_cast<unsigned long>(cr));
      }
      snd.cm_status = 0;
      snd.cm_problem = MAXDWORD;
    }

    if (verbose) {
      log.Logf(
          "virtio-snd: detected PCI device instance_id=%s name=%s modern=%d rev01=%d transitional=%d allowed=%d",
          WideToUtf8(snd.instance_id).c_str(), WideToUtf8(snd.description).c_str(), id_info.modern ? 1 : 0,
          id_info.modern_rev01 ? 1 : 0, id_info.transitional ? 1 : 0, allowed ? 1 : 0);
      if (!hwids.empty()) {
        log.Logf("virtio-snd: detected PCI device hwid0=%s", WideToUtf8(hwids[0]).c_str());
      }
      if (snd.force_null_backend.has_value()) {
        log.Logf("virtio-snd: detected PCI device ForceNullBackend=%lu",
                 static_cast<unsigned long>(*snd.force_null_backend));
      }
    }
    const std::wstring expected_service = snd.is_transitional && !snd.is_modern
                                              ? kVirtioSndExpectedServiceTransitional
                                              : kVirtioSndExpectedServiceModern;
    if (verbose) {
      if (id_info.modern && !id_info.modern_rev01) {
        log.Logf(
            "virtio-snd: pci device pnp_id=%s missing REV_01 (Aero contract v1 expects REV_01; QEMU needs x-pci-revision=0x01)",
            WideToUtf8(snd.instance_id).c_str());
      }
      log.Logf("virtio-snd: pci driver service=%s inf=%s section=%s (expected service=%s)",
               WideToUtf8(snd.service).c_str(), WideToUtf8(snd.inf_path).c_str(),
               WideToUtf8(snd.inf_section).c_str(), WideToUtf8(expected_service).c_str());
      if (!snd.driver_desc.empty() || !snd.provider_name.empty() || !snd.driver_version.empty() ||
          !snd.driver_date.empty() || !snd.matching_device_id.empty()) {
        log.Logf("virtio-snd: pci driver desc=%s provider=%s version=%s date=%s match_id=%s",
                 WideToUtf8(snd.driver_desc).c_str(), WideToUtf8(snd.provider_name).c_str(),
                 WideToUtf8(snd.driver_version).c_str(), WideToUtf8(snd.driver_date).c_str(),
                 WideToUtf8(snd.matching_device_id).c_str());
      }
      log.Logf("virtio-snd: pci cm_status=0x%08lx(%s) cm_problem=%lu(%s: %s)",
               static_cast<unsigned long>(snd.cm_status), CmStatusFlagsToString(snd.cm_status).c_str(),
               static_cast<unsigned long>(snd.cm_problem), CmProblemCodeToName(snd.cm_problem),
               CmProblemCodeToMeaning(snd.cm_problem));
    }
    if (allowed) {
      out.push_back(std::move(snd));
    } else {
      ignored_transitional.push_back(std::move(snd));
    }
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  if (verbose && !allow_transitional && out.empty() && !ignored_transitional.empty()) {
    log.LogLine("virtio-snd: found transitional PCI\\VEN_1AF4&DEV_1018 device(s) but ignoring them "
                "(contract v1 modern-only)");
    log.LogLine(
        "virtio-snd: QEMU hint: use disable-legacy=on,x-pci-revision=0x01 for virtio-snd (recommended); "
        "or use --allow-virtio-snd-transitional + the legacy driver package for backcompat");
  }
  return out;
}

static bool HasDeviceInterfaceForInstance(Logger& log, const GUID& iface_guid,
                                          const std::wstring& target_instance_id,
                                          const char* iface_name_for_log) {
  HDEVINFO devinfo =
      SetupDiGetClassDevsW(&iface_guid, nullptr, nullptr, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-snd: SetupDiGetClassDevs(%s) failed: %lu", iface_name_for_log, GetLastError());
    return false;
  }

  bool found = false;
  for (DWORD idx = 0;; idx++) {
    SP_DEVICE_INTERFACE_DATA iface{};
    iface.cbSize = sizeof(iface);
    if (!SetupDiEnumDeviceInterfaces(devinfo, nullptr, &iface_guid, idx, &iface)) {
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

    const auto inst_id = GetDeviceInstanceIdString(devinfo, &dev);
    if (!inst_id) continue;
    if (!EqualsInsensitive(*inst_id, target_instance_id)) continue;

    log.Logf("virtio-snd: found %s interface path=%s", iface_name_for_log,
             WideToUtf8(std::wstring(detail->DevicePath)).c_str());
    found = true;
    break;
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return found;
}

static bool VirtioSndHasTopologyInterface(Logger& log, const std::vector<VirtioSndPciDevice>& devices) {
  constexpr DWORD kWaitMs = 5000;
  const DWORD deadline_ms = GetTickCount() + kWaitMs;
  int attempt = 0;

  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    attempt++;
    bool found_any = false;
    for (const auto& dev : devices) {
      if (dev.instance_id.empty()) continue;
      if (HasDeviceInterfaceForInstance(log, kKsCategoryTopology, dev.instance_id, "KSCATEGORY_TOPOLOGY")) {
        found_any = true;
      }
    }
    if (found_any) return true;
    Sleep(250);
  }

  log.Logf("virtio-snd: topology interface not found after %lu ms", static_cast<unsigned long>(kWaitMs));
  return false;
}

struct VirtioSndBindingCheckResult {
  bool ok = false;
  bool any_wrong_service = false;
  bool any_missing_service = false;
  bool any_problem = false;
};

static VirtioSndBindingCheckResult SummarizeVirtioSndPciBinding(const std::vector<VirtioSndPciDevice>& devices) {
  VirtioSndBindingCheckResult out;
  for (const auto& dev : devices) {
    const std::wstring expected_service = dev.is_transitional && !dev.is_modern
                                              ? kVirtioSndExpectedServiceTransitional
                                              : kVirtioSndExpectedServiceModern;
    const bool has_service = !dev.service.empty();
    const bool service_ok = has_service && EqualsInsensitive(dev.service, expected_service);
    const bool problem_ok = (dev.cm_problem == 0) && ((dev.cm_status & DN_HAS_PROBLEM) == 0);

    if (!has_service) {
      out.any_missing_service = true;
    } else if (!service_ok) {
      out.any_wrong_service = true;
    }
    if (!problem_ok) {
      out.any_problem = true;
    }
    if (service_ok && problem_ok) {
      out.ok = true;
    }
  }
  return out;
}

static VirtioSndBindingCheckResult CheckVirtioSndPciBinding(Logger& log,
                                                            const std::vector<VirtioSndPciDevice>& devices) {
  VirtioSndBindingCheckResult out;

  for (const auto& dev : devices) {
    const std::wstring expected_service = dev.is_transitional && !dev.is_modern
                                              ? kVirtioSndExpectedServiceTransitional
                                              : kVirtioSndExpectedServiceModern;
    const bool has_service = !dev.service.empty();
    const bool service_ok = has_service && EqualsInsensitive(dev.service, expected_service);
    const bool problem_ok = (dev.cm_problem == 0) && ((dev.cm_status & DN_HAS_PROBLEM) == 0);

    if (!has_service) {
      out.any_missing_service = true;
      log.Logf("virtio-snd: pci device pnp_id=%s has no bound service (expected %s)",
               WideToUtf8(dev.instance_id).c_str(), WideToUtf8(expected_service).c_str());
    } else if (!service_ok) {
      out.any_wrong_service = true;
      log.Logf("virtio-snd: pci device pnp_id=%s bound_service=%s (expected %s)",
               WideToUtf8(dev.instance_id).c_str(), WideToUtf8(dev.service).c_str(),
               WideToUtf8(expected_service).c_str());
    }
    if (!problem_ok) {
      out.any_problem = true;
      log.Logf("virtio-snd: pci device pnp_id=%s has ConfigManagerErrorCode=%lu (%s: %s)",
               WideToUtf8(dev.instance_id).c_str(), static_cast<unsigned long>(dev.cm_problem),
               CmProblemCodeToName(dev.cm_problem), CmProblemCodeToMeaning(dev.cm_problem));
    }

    if (service_ok && problem_ok) {
      out.ok = true;
    }
  }

  if (!out.ok) {
    log.LogLine("virtio-snd: no virtio-snd PCI device is healthy and bound to the expected driver");
    log.LogLine("virtio-snd: troubleshooting hints:");
    log.LogLine("virtio-snd: - check Device Manager for Code 28/52/10 and inspect setupapi.dev.log");
    log.LogLine("virtio-snd: - for QEMU contract v1: use disable-legacy=on,x-pci-revision=0x01 and install aero_virtio_snd.inf");
    log.LogLine("virtio-snd: - for transitional QEMU: install aero-virtio-snd-legacy.inf and pass --allow-virtio-snd-transitional");
  }

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

#ifndef SCSIOP_REPORT_LUNS
#define SCSIOP_REPORT_LUNS 0xA0
#endif

struct ScsiPassThroughDirectWithSense {
  SCSI_PASS_THROUGH_DIRECT sptd{};
  ULONG filler{};
  UCHAR sense[32]{};
};

static bool VirtioBlkReportLuns(Logger& log, HANDLE hPhysicalDrive) {
  if (hPhysicalDrive == INVALID_HANDLE_VALUE) {
    log.LogLine("virtio-blk: REPORT_LUNS FAIL invalid PhysicalDrive handle");
    return false;
  }

  // Query the SCSI address for this physical drive so PathId/TargetId/Lun are correct.
  // Some stacks require these fields to be populated for pass-through IOCTLs.
  SCSI_ADDRESS addr{};
  DWORD addr_bytes = 0;
  if (DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_GET_ADDRESS, nullptr, 0, &addr, sizeof(addr), &addr_bytes,
                      nullptr)) {
    log.Logf("virtio-blk: REPORT_LUNS scsi_address port=%u path=%u target=%u lun=%u",
             static_cast<unsigned>(addr.PortNumber), static_cast<unsigned>(addr.PathId),
             static_cast<unsigned>(addr.TargetId), static_cast<unsigned>(addr.Lun));
  } else {
    // Not fatal; the values default to 0.
    log.Logf("virtio-blk: REPORT_LUNS warning: IOCTL_SCSI_GET_ADDRESS failed err=%lu (using 0/0/0)",
             GetLastError());
    addr.PortNumber = 0;
    addr.PathId = 0;
    addr.TargetId = 0;
    addr.Lun = 0;
  }

  constexpr uint32_t kAllocLen = 64;
  std::vector<uint8_t> resp(kAllocLen, 0);

  // SPC REPORT LUNS (0xA0) CDB is 12 bytes. Allocation length is a big-endian u32 at CDB[6..9].
  uint8_t cdb[12]{};
  cdb[0] = static_cast<uint8_t>(SCSIOP_REPORT_LUNS);
  cdb[6] = static_cast<uint8_t>((kAllocLen >> 24) & 0xFF);
  cdb[7] = static_cast<uint8_t>((kAllocLen >> 16) & 0xFF);
  cdb[8] = static_cast<uint8_t>((kAllocLen >> 8) & 0xFF);
  cdb[9] = static_cast<uint8_t>(kAllocLen & 0xFF);

  ScsiPassThroughDirectWithSense pkt{};
  pkt.sptd.Length = sizeof(pkt.sptd);
  pkt.sptd.PathId = addr.PathId;
  pkt.sptd.TargetId = addr.TargetId;
  pkt.sptd.Lun = addr.Lun;
  pkt.sptd.CdbLength = sizeof(cdb);
  pkt.sptd.SenseInfoLength = sizeof(pkt.sense);
  pkt.sptd.DataIn = SCSI_IOCTL_DATA_IN;
  pkt.sptd.DataTransferLength = kAllocLen;
  pkt.sptd.TimeOutValue = 5;
  pkt.sptd.DataBuffer = resp.data();
  pkt.sptd.SenseInfoOffset = static_cast<ULONG>(offsetof(ScsiPassThroughDirectWithSense, sense));
  memcpy(pkt.sptd.Cdb, cdb, sizeof(cdb));

  DWORD returned = 0;
  const BOOL ok = DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_PASS_THROUGH_DIRECT, &pkt, sizeof(pkt), &pkt,
                                  sizeof(pkt), &returned, nullptr);
  const DWORD err = ok ? ERROR_SUCCESS : GetLastError();

  if (!ok) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL DeviceIoControl(IOCTL_SCSI_PASS_THROUGH_DIRECT) err=%lu",
             static_cast<unsigned long>(err));
    return false;
  }

  if (pkt.sptd.ScsiStatus != 0) {
    const uint8_t sk = (sizeof(pkt.sense) >= 3) ? (pkt.sense[2] & 0x0F) : 0;
    const uint8_t asc = (sizeof(pkt.sense) >= 13) ? pkt.sense[12] : 0;
    const uint8_t ascq = (sizeof(pkt.sense) >= 14) ? pkt.sense[13] : 0;
    log.Logf("virtio-blk: REPORT_LUNS FAIL scsi_status=0x%02x sense_key=0x%02x asc=0x%02x ascq=0x%02x",
             static_cast<unsigned>(pkt.sptd.ScsiStatus), static_cast<unsigned>(sk),
             static_cast<unsigned>(asc), static_cast<unsigned>(ascq));
    return false;
  }

  auto read_be_u32 = [](const uint8_t* p) -> uint32_t {
    return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) |
           (static_cast<uint32_t>(p[2]) << 8) | static_cast<uint32_t>(p[3]);
  };

  const uint32_t list_len = read_be_u32(resp.data());
  const uint32_t reserved = read_be_u32(resp.data() + 4);
  if (list_len != 8) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL unexpected list_length=%lu (expected 8)",
             static_cast<unsigned long>(list_len));
    return false;
  }
  if (reserved != 0) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL nonzero reserved=0x%08lx", static_cast<unsigned long>(reserved));
    return false;
  }

  bool lun0_all_zero = true;
  for (size_t i = 8; i < 16; i++) {
    if (resp[i] != 0) lun0_all_zero = false;
  }
  if (!lun0_all_zero) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL LUN0 entry not all zeros: %02x %02x %02x %02x %02x %02x %02x %02x",
             static_cast<unsigned>(resp[8]), static_cast<unsigned>(resp[9]), static_cast<unsigned>(resp[10]),
             static_cast<unsigned>(resp[11]), static_cast<unsigned>(resp[12]), static_cast<unsigned>(resp[13]),
             static_cast<unsigned>(resp[14]), static_cast<unsigned>(resp[15]));
    return false;
  }

  log.LogLine("virtio-blk: REPORT_LUNS PASS");
  return true;
}

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

  // Exercise aero_virtio_blk.sys miniport IOCTL_SCSI_MINIPORT query contract via \\.\PhysicalDrive<N>.
  {
    HANDLE pd = OpenPhysicalDriveForIoctl(log, *base_disk);
    if (pd == INVALID_HANDLE_VALUE) {
      log.LogLine("virtio-blk: miniport query FAIL (unable to open PhysicalDrive)");
      return false;
    }

    const auto info = QueryAerovblkMiniportInfo(log, pd);
    bool query_ok = false;
    if (!info.has_value()) {
      log.LogLine("virtio-blk: miniport query FAIL (IOCTL_SCSI_MINIPORT query failed)");
    } else {
      query_ok = ValidateAerovblkMiniportInfo(log, *info);
    }

    // Optional: cover flush path explicitly, but don't fail overall test if the flush ioctl is blocked.
    DWORD bytes = 0;
    if (DeviceIoControl(pd, IOCTL_DISK_FLUSH_CACHE, nullptr, 0, nullptr, 0, &bytes, nullptr)) {
      log.LogLine("virtio-blk: IOCTL_DISK_FLUSH_CACHE ok");
    } else {
      log.Logf("virtio-blk: IOCTL_DISK_FLUSH_CACHE failed err=%lu", GetLastError());
    }

    const bool report_luns_ok = VirtioBlkReportLuns(log, pd);

    CloseHandle(pd);

    if (!query_ok) return false;
    if (!report_luns_ok) return false;
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
    // The in-tree Aero virtio-input HID minidriver uses:
    //   - Keyboard: VID_1AF4&PID_0001
    //   - Mouse:    VID_1AF4&PID_0002
    if (ContainsInsensitive(id, L"VID_1AF4&PID_0001")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_0002")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_1052")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_1011")) return true;
  }
  return false;
}

static bool LooksLikeVirtioInputInterfacePath(const std::wstring& device_path) {
  return ContainsInsensitive(device_path, L"VEN_1AF4&DEV_1052") ||
         ContainsInsensitive(device_path, L"VEN_1AF4&DEV_1011") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_0001") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_0002") ||
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

struct VirtioInputEventsTestResult {
  bool ok = false;
  bool saw_key_a_down = false;
  bool saw_key_a_up = false;
  bool saw_mouse_move = false;
  bool saw_mouse_left_down = false;
  bool saw_mouse_left_up = false;
  int keyboard_reports = 0;
  int mouse_reports = 0;
  std::string reason;
  DWORD win32_error = 0;
};

struct VirtioInputHidPaths {
  std::wstring keyboard_path;
  std::wstring mouse_path;
  std::string reason;
};

static std::optional<VirtioInputHidPaths> FindVirtioInputHidPaths(Logger& log) {
  // {4D1E55B2-F16F-11CF-88CB-001111000030}
  static const GUID kHidInterfaceGuid = {0x4D1E55B2,
                                         0xF16F,
                                         0x11CF,
                                         {0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30}};

  HDEVINFO devinfo = SetupDiGetClassDevsW(&kHidInterfaceGuid, nullptr, nullptr,
                                         DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-input-events: SetupDiGetClassDevs(GUID_DEVINTERFACE_HID) failed: %lu", GetLastError());
    return std::nullopt;
  }

  VirtioInputHidPaths out{};
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

    HANDLE h = OpenHidDeviceForIoctl(device_path.c_str());
    if (h == INVALID_HANDLE_VALUE) {
      had_error = true;
      log.Logf("virtio-input-events: CreateFile(%s) failed err=%lu", WideToUtf8(device_path).c_str(),
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

    if (has_keyboard && !has_mouse && out.keyboard_path.empty()) {
      out.keyboard_path = device_path;
      log.Logf("virtio-input-events: selected keyboard HID interface: %s", WideToUtf8(device_path).c_str());
    } else if (has_mouse && !has_keyboard && out.mouse_path.empty()) {
      out.mouse_path = device_path;
      log.Logf("virtio-input-events: selected mouse HID interface: %s", WideToUtf8(device_path).c_str());
    }

    if (!out.keyboard_path.empty() && !out.mouse_path.empty()) break;
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  if (had_error) {
    out.reason = "ioctl_or_open_failed";
    return out;
  }
  if (out.keyboard_path.empty()) {
    out.reason = "missing_keyboard_device";
    return out;
  }
  if (out.mouse_path.empty()) {
    out.reason = "missing_mouse_device";
    return out;
  }

  return out;
}

static HANDLE OpenHidDeviceForRead(const wchar_t* path) {
  const DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  const DWORD flags = FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED;
  const DWORD desired_accesses[] = {GENERIC_READ | GENERIC_WRITE, GENERIC_READ};

  for (const DWORD access : desired_accesses) {
    HANDLE h = CreateFileW(path, access, share, nullptr, OPEN_EXISTING, flags, nullptr);
    if (h != INVALID_HANDLE_VALUE) return h;
  }
  return INVALID_HANDLE_VALUE;
}

struct HidOverlappedReader {
  HANDLE h = INVALID_HANDLE_VALUE;
  HANDLE ev = nullptr;
  OVERLAPPED ov{};
  std::vector<uint8_t> buf;
  DWORD bytes = 0;
  bool pending = false;
  DWORD last_error = 0;

  bool StartRead() {
    if (h == INVALID_HANDLE_VALUE) return false;
    if (!ev) {
      ev = CreateEventW(nullptr, TRUE, FALSE, nullptr);
      if (!ev) {
        last_error = GetLastError();
        return false;
      }
      ZeroMemory(&ov, sizeof(ov));
      ov.hEvent = ev;
    }

    ResetEvent(ev);
    bytes = 0;
    pending = false;

    BOOL ok = ReadFile(h, buf.data(), static_cast<DWORD>(buf.size()), &bytes, &ov);
    if (ok) {
      pending = false;
      // Some drivers don't reliably signal the overlapped event for synchronous completion; ensure the wait
      // loop sees it.
      SetEvent(ev);
      return true;
    }

    const DWORD err = GetLastError();
    if (err == ERROR_IO_PENDING) {
      pending = true;
      return true;
    }

    last_error = err;
    return false;
  }

  bool FinishRead(DWORD& out_bytes) {
    if (!pending) {
      out_bytes = bytes;
      return true;
    }

    DWORD n = 0;
    if (!GetOverlappedResult(h, &ov, &n, FALSE)) {
      last_error = GetLastError();
      return false;
    }
    pending = false;
    out_bytes = n;
    return true;
  }

  void CancelAndClose() {
    if (h != INVALID_HANDLE_VALUE) {
      // Best-effort: cancel any outstanding overlapped reads so CloseHandle doesn't block.
      CancelIo(h);
      CloseHandle(h);
      h = INVALID_HANDLE_VALUE;
    }
    if (ev) {
      CloseHandle(ev);
      ev = nullptr;
    }
  }
};

static void ProcessKeyboardReport(VirtioInputEventsTestResult& out, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  size_t off = 0;
  if (buf[0] == 1) off = 1; // ReportID=1 (virtio-input keyboard)
  if (len < off + 2) return;

  const uint8_t modifiers = buf[off];
  const uint8_t* keys = buf + off + 2;
  const size_t key_count = static_cast<size_t>(len) - (off + 2);

  bool saw_a = false;
  bool all_zero = true;
  for (size_t i = 0; i < key_count; i++) {
    if (keys[i] == 0x04) saw_a = true; // HID Usage ID for 'A'
    if (keys[i] != 0) all_zero = false;
  }

  if (saw_a) out.saw_key_a_down = true;
  if (out.saw_key_a_down && modifiers == 0 && all_zero) {
    out.saw_key_a_up = true;
  }
}

static void ProcessMouseReport(VirtioInputEventsTestResult& out, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  size_t off = 0;
  if (buf[0] == 2) off = 1; // ReportID=2 (virtio-input mouse)
  if (len < off + 3) return;

  const uint8_t buttons = buf[off + 0];
  const int8_t dx = static_cast<int8_t>(buf[off + 1]);
  const int8_t dy = static_cast<int8_t>(buf[off + 2]);

  if (dx != 0 || dy != 0) out.saw_mouse_move = true;

  const bool left = (buttons & 0x01) != 0;
  if (left) out.saw_mouse_left_down = true;
  if (out.saw_mouse_left_down && !left) out.saw_mouse_left_up = true;
}

static VirtioInputEventsTestResult VirtioInputEventsTest(Logger& log) {
  VirtioInputEventsTestResult out{};

  const auto paths = FindVirtioInputHidPaths(log);
  if (!paths.has_value()) {
    out.reason = "hid_enum_failed";
    out.win32_error = GetLastError();
    return out;
  }
  if (!paths->reason.empty()) {
    out.reason = paths->reason;
    return out;
  }

  HidOverlappedReader kbd{};
  HidOverlappedReader mouse{};
  kbd.buf.resize(64);
  mouse.buf.resize(64);

  kbd.h = OpenHidDeviceForRead(paths->keyboard_path.c_str());
  if (kbd.h == INVALID_HANDLE_VALUE) {
    out.reason = "open_keyboard_failed";
    out.win32_error = GetLastError();
    return out;
  }
  mouse.h = OpenHidDeviceForRead(paths->mouse_path.c_str());
  if (mouse.h == INVALID_HANDLE_VALUE) {
    out.reason = "open_mouse_failed";
    out.win32_error = GetLastError();
    kbd.CancelAndClose();
    return out;
  }

  if (!kbd.StartRead()) {
    out.reason = "read_keyboard_failed";
    out.win32_error = kbd.last_error;
    kbd.CancelAndClose();
    mouse.CancelAndClose();
    return out;
  }
  if (!mouse.StartRead()) {
    out.reason = "read_mouse_failed";
    out.win32_error = mouse.last_error;
    kbd.CancelAndClose();
    mouse.CancelAndClose();
    return out;
  }

  log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY");

  const DWORD deadline_ms = GetTickCount() + 10000;
  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    if (out.saw_key_a_down && out.saw_key_a_up && out.saw_mouse_move && out.saw_mouse_left_down &&
        out.saw_mouse_left_up) {
      out.ok = true;
      break;
    }

    const DWORD now = GetTickCount();
    const int32_t diff = static_cast<int32_t>(deadline_ms - now);
    const DWORD timeout = diff > 0 ? static_cast<DWORD>(diff) : 0;

    HANDLE evs[2] = {kbd.ev, mouse.ev};
    const DWORD wait = WaitForMultipleObjects(2, evs, FALSE, timeout);
    if (wait == WAIT_TIMEOUT) break;
    if (wait == WAIT_FAILED) {
      out.reason = "wait_failed";
      out.win32_error = GetLastError();
      break;
    }

    const int which = static_cast<int>(wait - WAIT_OBJECT_0);
    HidOverlappedReader* reader = (which == 0) ? &kbd : &mouse;

    DWORD n = 0;
    if (!reader->FinishRead(n)) {
      out.reason = (which == 0) ? "read_keyboard_failed" : "read_mouse_failed";
      out.win32_error = reader->last_error;
      break;
    }

    if (which == 0) {
      out.keyboard_reports++;
      ProcessKeyboardReport(out, reader->buf.data(), n);
    } else {
      out.mouse_reports++;
      ProcessMouseReport(out, reader->buf.data(), n);
    }

    if (!reader->StartRead()) {
      out.reason = (which == 0) ? "read_keyboard_failed" : "read_mouse_failed";
      out.win32_error = reader->last_error;
      break;
    }
  }

  kbd.CancelAndClose();
  mouse.CancelAndClose();

  if (out.ok) return out;

  if (out.reason.empty()) {
    out.reason = "timeout";
  }
  return out;
}

struct VirtioNetAdapter {
  std::wstring instance_id;   // e.g. "{GUID}"
  std::wstring friendly_name; // optional
  std::wstring service;       // SPDRP_SERVICE (bound driver service name)
  std::vector<std::wstring> hardware_ids; // SPDRP_HARDWAREID (optional; for debugging/contract checks)
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
    adapter.hardware_ids = hwids;
    if (auto inst = GetDevicePropertyString(devinfo, &dev, SPDRP_NETCFG_INSTANCE_ID)) {
      adapter.instance_id = *inst;
    }
    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      adapter.friendly_name = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      adapter.friendly_name = *desc;
    }
    if (auto svc = GetDevicePropertyString(devinfo, &dev, SPDRP_SERVICE)) {
      adapter.service = *svc;
    }

    if (!adapter.instance_id.empty()) {
      log.Logf("virtio-net: detected adapter instance_id=%s name=%s service=%s",
               WideToUtf8(adapter.instance_id).c_str(), WideToUtf8(adapter.friendly_name).c_str(),
               adapter.service.empty() ? "<missing>" : WideToUtf8(adapter.service).c_str());
      for (size_t i = 0; i < adapter.hardware_ids.size(); i++) {
        log.Logf("virtio-net:   hwid[%zu]=%s", i, WideToUtf8(adapter.hardware_ids[i]).c_str());
      }
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

static uint64_t Fnv1a64Update(uint64_t hash, const uint8_t* data, size_t len) {
  static const uint64_t kPrime = 1099511628211ull;
  for (size_t i = 0; i < len; i++) {
    hash ^= static_cast<uint64_t>(data[i]);
    hash *= kPrime;
  }
  return hash;
}

static std::wstring UrlAppendSuffix(const std::wstring& url, const std::wstring& suffix) {
  // Best-effort: append a suffix to the URL path while preserving any query/fragment.
  //
  // The host harness exposes `${HttpPath}-large`, so the default URL
  //   http://10.0.2.2:18080/aero-virtio-selftest
  // becomes
  //   http://10.0.2.2:18080/aero-virtio-selftest-large
  const size_t q = url.find(L'?');
  const size_t h = url.find(L'#');
  size_t insert_pos = std::wstring::npos;
  if (q != std::wstring::npos && h != std::wstring::npos) {
    insert_pos = std::min(q, h);
  } else if (q != std::wstring::npos) {
    insert_pos = q;
  } else if (h != std::wstring::npos) {
    insert_pos = h;
  }
  if (insert_pos == std::wstring::npos) return url + suffix;

  std::wstring out = url;
  out.insert(insert_pos, suffix);
  return out;
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

static bool HttpGetLargeDeterministic(Logger& log, const std::wstring& url) {
  static const uint64_t kExpectedBytes = 1024ull * 1024ull;
  // FNV-1a 64-bit hash of bytes 0..255 repeated to 1 MiB.
  static const uint64_t kExpectedHash = 0x8505ae4435522325ull;
  static const uint64_t kFnvOffsetBasis = 14695981039346656037ull; // 0xcbf29ce484222325

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
      WinHttpOpen(L"AeroVirtioSelftest/1.0", WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_NO_PROXY_NAME,
                  WINHTTP_NO_PROXY_BYPASS, 0);
  if (!session) {
    log.Logf("virtio-net: WinHttpOpen failed err=%lu", GetLastError());
    return false;
  }

  WinHttpSetTimeouts(session, 15000, 15000, 15000, 15000);

  HINTERNET connect = WinHttpConnect(session, host.c_str(), port, 0);
  if (!connect) {
    log.Logf("virtio-net: WinHttpConnect failed host=%s port=%u err=%lu", WideToUtf8(host).c_str(),
             port, GetLastError());
    WinHttpCloseHandle(session);
    return false;
  }

  const DWORD flags = secure ? WINHTTP_FLAG_SECURE : 0;
  HINTERNET request = WinHttpOpenRequest(connect, L"GET", path.c_str(), nullptr, WINHTTP_NO_REFERER,
                                         WINHTTP_DEFAULT_ACCEPT_TYPES, flags);
  if (!request) {
    log.Logf("virtio-net: WinHttpOpenRequest failed err=%lu", GetLastError());
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  if (!WinHttpSendRequest(request, WINHTTP_NO_ADDITIONAL_HEADERS, 0, WINHTTP_NO_REQUEST_DATA, 0, 0, 0)) {
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

  DWORD content_len = 0;
  DWORD content_len_size = sizeof(content_len);
  bool has_content_len = WinHttpQueryHeaders(request, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
                                             WINHTTP_HEADER_NAME_BY_INDEX, &content_len, &content_len_size,
                                             WINHTTP_NO_HEADER_INDEX) != 0;

  uint64_t total_read = 0;
  uint64_t hash = kFnvOffsetBasis;
  bool read_ok = true;
  std::vector<uint8_t> buf(64 * 1024);
  PerfTimer timer;

  for (;;) {
    DWORD available = 0;
    if (!WinHttpQueryDataAvailable(request, &available)) {
      log.Logf("virtio-net: WinHttpQueryDataAvailable failed err=%lu", GetLastError());
      read_ok = false;
      break;
    }
    if (available == 0) break;

    while (available > 0) {
      const DWORD to_read = std::min<DWORD>(available, static_cast<DWORD>(buf.size()));
      DWORD read = 0;
      if (!WinHttpReadData(request, buf.data(), to_read, &read)) {
        log.Logf("virtio-net: WinHttpReadData failed err=%lu", GetLastError());
        read_ok = false;
        break;
      }
      if (read == 0) {
        available = 0;
        break;
      }
      total_read += static_cast<uint64_t>(read);
      hash = Fnv1a64Update(hash, buf.data(), read);
      available -= read;
    }
    if (!read_ok) break;
  }

  WinHttpCloseHandle(request);
  WinHttpCloseHandle(connect);
  WinHttpCloseHandle(session);

  const double sec = std::max(0.000001, timer.SecondsSinceStart());
  const double mbps = (static_cast<double>(total_read) / (1024.0 * 1024.0)) / sec;
  log.Logf("virtio-net: HTTP GET large done url=%s status=%lu bytes_read=%llu sec=%.2f mbps=%.2f "
           "fnv1a64=0x%016llx%s",
           WideToUtf8(url).c_str(), status, static_cast<unsigned long long>(total_read), sec, mbps,
           static_cast<unsigned long long>(hash),
           has_content_len ? "" : " (missing Content-Length)");

  bool header_ok = false;
  if (!has_content_len) {
    log.Logf("virtio-net: HTTP GET large missing Content-Length expected=%llu",
             static_cast<unsigned long long>(kExpectedBytes));
  } else if (content_len != kExpectedBytes) {
    log.Logf("virtio-net: HTTP GET large Content-Length mismatch got=%lu expected=%llu",
             static_cast<unsigned long>(content_len), static_cast<unsigned long long>(kExpectedBytes));
  } else {
    header_ok = true;
  }

  if (!(status >= 200 && status < 300)) {
    if (status == 404) {
      log.LogLine("virtio-net: HTTP GET large endpoint not found (404). Ensure the host harness serves "
                  "`<http_url>-large`.");
    }
    return false;
  }
  if (!read_ok) return false;
  if (!header_ok) return false;
  if (total_read != kExpectedBytes || hash != kExpectedHash) {
    log.Logf("virtio-net: HTTP GET large body mismatch bytes_read=%llu expected_bytes=%llu hash=0x%016llx "
             "expected_hash=0x%016llx",
             static_cast<unsigned long long>(total_read), static_cast<unsigned long long>(kExpectedBytes),
             static_cast<unsigned long long>(hash), static_cast<unsigned long long>(kExpectedHash));
    return false;
  }

  log.LogLine("virtio-net: HTTP GET large ok (size+hash match)");
  return true;
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

  // Ensure the selected NIC is using the in-tree Aero virtio-net miniport, not a third-party
  // virtio driver (e.g. virtio-win netkvm). Also ensure the device matches the Aero contract HWID.
  static const wchar_t kExpectedService[] = L"aero_virtio_net";
  const bool service_ok = EqualsInsensitive(chosen->service, kExpectedService);

  bool contract_hwid_ok = false;
  bool contract_rev01 = false;
  for (const auto& id : chosen->hardware_ids) {
    if (ContainsInsensitive(id, L"PCI\\VEN_1AF4&DEV_1041")) {
      contract_hwid_ok = true;
      if (ContainsInsensitive(id, L"&REV_01")) contract_rev01 = true;
    }
  }

  if (!service_ok || !contract_hwid_ok) {
    log.Logf("virtio-net: FAIL: selected adapter does not match Aero virtio-net binding/contract");
    log.Logf("virtio-net: selected name=%s guid=%s", WideToUtf8(chosen_friendly).c_str(),
             WideToUtf8(chosen->instance_id).c_str());
    if (!service_ok) {
      log.Logf("virtio-net: FAIL: expected_service=%s actual_service=%s",
               WideToUtf8(kExpectedService).c_str(),
               chosen->service.empty() ? "<missing>" : WideToUtf8(chosen->service).c_str());
    }
    if (!contract_hwid_ok) {
      log.LogLine("virtio-net: FAIL: missing contract HWID substring PCI\\VEN_1AF4&DEV_1041 in hardware IDs");
    }
    for (size_t i = 0; i < chosen->hardware_ids.size(); i++) {
      log.Logf("virtio-net: selected hwid[%zu]=%s", i, WideToUtf8(chosen->hardware_ids[i]).c_str());
    }
    return false;
  }
  if (!contract_rev01) {
    log.LogLine("virtio-net: note: contract HWID matched but no &REV_01 entry was found");
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
  if (!HttpGetLargeDeterministic(log, UrlAppendSuffix(opt.http_url, L"-large"))) return false;
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

static bool QueryServiceIsRunning(SC_HANDLE svc, DWORD* state_out) {
  if (state_out) *state_out = 0;
  if (!svc) return false;

  SERVICE_STATUS_PROCESS ssp{};
  DWORD bytes_needed = 0;
  if (!QueryServiceStatusEx(svc, SC_STATUS_PROCESS_INFO, reinterpret_cast<LPBYTE>(&ssp), sizeof(ssp),
                            &bytes_needed)) {
    return false;
  }
  if (state_out) *state_out = ssp.dwCurrentState;
  return ssp.dwCurrentState == SERVICE_RUNNING;
}

static bool TryStartService(Logger& log, SC_HANDLE svc, const wchar_t* name) {
  if (!svc || !name) return false;
  if (StartServiceW(svc, 0, nullptr)) {
    log.Logf("virtio-snd: StartService(%s) ok", WideToUtf8(name).c_str());
    return true;
  }

  const DWORD err = GetLastError();
  if (err == ERROR_SERVICE_ALREADY_RUNNING) {
    log.Logf("virtio-snd: StartService(%s) already running", WideToUtf8(name).c_str());
    return true;
  }
  if (err == ERROR_SERVICE_DISABLED) {
    log.Logf("virtio-snd: StartService(%s) failed: disabled", WideToUtf8(name).c_str());
    return false;
  }

  log.Logf("virtio-snd: StartService(%s) failed err=%lu", WideToUtf8(name).c_str(),
           static_cast<unsigned long>(err));
  return false;
}

static void WaitForWindowsAudioServices(Logger& log, DWORD wait_ms) {
  if (wait_ms == 0) return;

  SC_HANDLE scm = OpenSCManagerW(nullptr, nullptr, SC_MANAGER_CONNECT);
  if (!scm) {
    log.Logf("virtio-snd: OpenSCManager failed err=%lu", GetLastError());
    return;
  }

  const DWORD desired_access = SERVICE_QUERY_STATUS | SERVICE_START;
  SC_HANDLE audiosrv = OpenServiceW(scm, L"AudioSrv", desired_access);
  SC_HANDLE builder = OpenServiceW(scm, L"AudioEndpointBuilder", desired_access);

  if (!audiosrv || !builder) {
    log.Logf("virtio-snd: OpenService(AudioSrv/AudioEndpointBuilder) failed err=%lu", GetLastError());
    if (audiosrv) CloseServiceHandle(audiosrv);
    if (builder) CloseServiceHandle(builder);
    CloseServiceHandle(scm);
    return;
  }

  const DWORD deadline_ms = GetTickCount() + wait_ms;
  int attempt = 0;
  DWORD state_audio = 0;
  DWORD state_builder = 0;
  bool audio_running = false;
  bool builder_running = false;
  bool tried_start_audio = false;
  bool tried_start_builder = false;

  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    attempt++;
    audio_running = QueryServiceIsRunning(audiosrv, &state_audio);
    builder_running = QueryServiceIsRunning(builder, &state_builder);
    if (!builder_running && state_builder == SERVICE_STOPPED && !tried_start_builder) {
      tried_start_builder = true;
      (void)TryStartService(log, builder, L"AudioEndpointBuilder");
    }
    if (!audio_running && state_audio == SERVICE_STOPPED && !tried_start_audio) {
      tried_start_audio = true;
      (void)TryStartService(log, audiosrv, L"AudioSrv");
    }
    if (audio_running && builder_running) break;
    Sleep(500);
  }

  log.Logf("virtio-snd: audio services AudioSrv=%s (state=%lu) AudioEndpointBuilder=%s (state=%lu) attempt=%d",
           audio_running ? "RUNNING" : "NOT_RUNNING", static_cast<unsigned long>(state_audio),
           builder_running ? "RUNNING" : "NOT_RUNNING", static_cast<unsigned long>(state_builder), attempt);

  CloseServiceHandle(audiosrv);
  CloseServiceHandle(builder);
  CloseServiceHandle(scm);
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

static void TryEnsureEndpointVolumeAudible(Logger& log, IMMDevice* endpoint, const char* tag) {
  if (!endpoint || !tag) return;

  ComPtr<IAudioEndpointVolume> vol;
  HRESULT hr = endpoint->Activate(__uuidof(IAudioEndpointVolume), CLSCTX_INPROC_SERVER, nullptr,
                                  reinterpret_cast<void**>(vol.Put()));
  if (FAILED(hr) || !vol) {
    log.Logf("virtio-snd: %s endpoint IAudioEndpointVolume unavailable hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
    return;
  }

  BOOL mute = FALSE;
  hr = vol->GetMute(&mute);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s endpoint mute=%d", tag, mute ? 1 : 0);
  }

  if (mute) {
    hr = vol->SetMute(FALSE, nullptr);
    log.Logf("virtio-snd: %s endpoint SetMute(FALSE) hr=0x%08lx", tag, static_cast<unsigned long>(hr));
  }

  float before = 0.0f;
  hr = vol->GetMasterVolumeLevelScalar(&before);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s endpoint volume=%.3f", tag, before);
  }

  // Some Win7 images can have the master volume muted/at 0, which results in silent host-side wav
  // captures even though waveOut/WASAPI calls succeed. Force a non-trivial master volume so the
  // harness can validate end-to-end audio output deterministically.
  hr = vol->SetMasterVolumeLevelScalar(0.50f, nullptr);
  if (FAILED(hr)) {
    log.Logf("virtio-snd: %s endpoint SetMasterVolumeLevelScalar(0.50) failed hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
  }
}

static void TryEnsureEndpointSessionAudible(Logger& log, IMMDevice* endpoint, const char* tag) {
  if (!endpoint || !tag) return;

  ComPtr<IAudioSessionManager2> mgr;
  HRESULT hr = endpoint->Activate(__uuidof(IAudioSessionManager2), CLSCTX_INPROC_SERVER, nullptr,
                                  reinterpret_cast<void**>(mgr.Put()));
  if (FAILED(hr) || !mgr) {
    log.Logf("virtio-snd: %s endpoint IAudioSessionManager2 unavailable hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
    return;
  }

  ComPtr<ISimpleAudioVolume> vol;
  hr = mgr->GetSimpleAudioVolume(nullptr, 0, vol.Put());
  if (FAILED(hr) || !vol) {
    log.Logf("virtio-snd: %s endpoint ISimpleAudioVolume unavailable hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
    return;
  }

  BOOL mute = FALSE;
  hr = vol->GetMute(&mute);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s session mute=%d", tag, mute ? 1 : 0);
  }

  if (mute) {
    hr = vol->SetMute(FALSE, nullptr);
    log.Logf("virtio-snd: %s session SetMute(FALSE) hr=0x%08lx", tag, static_cast<unsigned long>(hr));
  }

  float before = 0.0f;
  hr = vol->GetMasterVolume(&before);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s session volume=%.3f", tag, before);
  }

  hr = vol->SetMasterVolume(1.0f, nullptr);
  if (FAILED(hr)) {
    log.Logf("virtio-snd: %s session SetMasterVolume(1.0) failed hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
  }
}

static void TryEnsureAudioClientSessionAudible(Logger& log, IAudioClient* client, const char* tag) {
  if (!client || !tag) return;

  ComPtr<ISimpleAudioVolume> vol;
  HRESULT hr = client->GetService(__uuidof(ISimpleAudioVolume), reinterpret_cast<void**>(vol.Put()));
  if (FAILED(hr) || !vol) {
    log.Logf("virtio-snd: %s audio client ISimpleAudioVolume unavailable hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
    return;
  }

  BOOL mute = FALSE;
  hr = vol->GetMute(&mute);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s audio client session mute=%d", tag, mute ? 1 : 0);
  }

  if (mute) {
    hr = vol->SetMute(FALSE, nullptr);
    log.Logf("virtio-snd: %s audio client session SetMute(FALSE) hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
  }

  float before = 0.0f;
  hr = vol->GetMasterVolume(&before);
  if (SUCCEEDED(hr)) {
    log.Logf("virtio-snd: %s audio client session volume=%.3f", tag, before);
  }

  hr = vol->SetMasterVolume(1.0f, nullptr);
  if (FAILED(hr)) {
    log.Logf("virtio-snd: %s audio client session SetMasterVolume(1.0) failed hr=0x%08lx", tag,
             static_cast<unsigned long>(hr));
  }
}

static void TryEnsureDefaultRenderEndpointAudible(Logger& log) {
  ScopedCoInitialize com(COINIT_MULTITHREADED);
  if (FAILED(com.hr())) {
    log.Logf("virtio-snd: default render endpoint volume: CoInitializeEx failed hr=0x%08lx",
             static_cast<unsigned long>(com.hr()));
    return;
  }

  ComPtr<IMMDeviceEnumerator> enumerator;
  HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                                __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(enumerator.Put()));
  if (FAILED(hr) || !enumerator) {
    log.Logf("virtio-snd: default render endpoint volume: CoCreateInstance failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return;
  }

  ComPtr<IMMDevice> endpoint;
  hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, endpoint.Put());
  if (FAILED(hr) || !endpoint) {
    log.Logf("virtio-snd: default render endpoint volume: GetDefaultAudioEndpoint failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return;
  }

  TryEnsureEndpointVolumeAudible(log, endpoint.Get(), "default-render");
  TryEnsureEndpointSessionAudible(log, endpoint.Get(), "default-render");
}

static bool LooksLikeVirtioSndEndpoint(const std::wstring& friendly_name, const std::wstring& instance_id,
                                       const std::vector<std::wstring>& hwids,
                                       const std::vector<std::wstring>& match_names,
                                       bool allow_transitional) {
  // Prefer the PCI IDs (PKEY_Device_InstanceId + SetupAPI hardware IDs) to avoid false-positive
  // matches against unrelated audio devices.
  VirtioSndPciIdInfo hwid_info{};
  const bool hwid_allowed = IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional, &hwid_info);
  const auto inst_info = GetVirtioSndPciIdInfoFromString(instance_id);
  const bool inst_allowed = IsAllowedVirtioSndPciId(inst_info, allow_transitional);

  // If the caller did not allow transitional devices, actively reject a transitional match even if
  // the friendly name looks plausible.
  if (!allow_transitional &&
      ((hwid_info.transitional && !hwid_info.modern) || (inst_info.transitional && !inst_info.modern))) {
    return false;
  }

  if (hwid_allowed || inst_allowed) return true;

  if (ContainsInsensitive(friendly_name, L"virtio") || ContainsInsensitive(friendly_name, L"aero")) return true;
  for (const auto& m : match_names) {
    if (!m.empty() && ContainsInsensitive(friendly_name, m)) return true;
  }
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

static bool BufferContainsNonSilence(const WAVEFORMATEX* fmt, const BYTE* data, size_t bytes) {
  if (!fmt || !data || bytes == 0) return false;
  // For PCM/floating-point formats, silence is a stable byte pattern:
  // - all zeros (most formats)
  // - 0x80 for 8-bit unsigned PCM.
  BYTE silence = 0;
  if (WaveFormatIsPcm(fmt) && fmt->wBitsPerSample == 8) silence = 0x80;
  for (size_t i = 0; i < bytes; i++) {
    if (data[i] != silence) return true;
  }
  return false;
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

struct SelectedVirtioSndEndpoint {
  ComPtr<IMMDevice> device;
  std::wstring friendly;
  std::wstring id;
  std::wstring instance_id;
  std::wstring pci_hwid;
  int score = -1;
};

static std::optional<SelectedVirtioSndEndpoint> FindVirtioSndRenderEndpoint(
    Logger& log, IMMDeviceEnumerator* enumerator, const std::vector<std::wstring>& match_names,
    bool allow_transitional, DWORD wait_ms = 20000) {
  if (!enumerator) return std::nullopt;

  SelectedVirtioSndEndpoint best;
  int best_score = -1;

  const DWORD deadline_ms = GetTickCount() + wait_ms;
  int attempt = 0;

  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    attempt++;

    ComPtr<IMMDeviceCollection> collection;
    const DWORD state_mask =
        DEVICE_STATE_ACTIVE | DEVICE_STATE_DISABLED | DEVICE_STATE_NOTPRESENT | DEVICE_STATE_UNPLUGGED;
    HRESULT hr = enumerator->EnumAudioEndpoints(eRender, state_mask, collection.Put());
    if (FAILED(hr) || !collection) {
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
    best.device.Reset();

    for (UINT i = 0; i < count; i++) {
      ComPtr<IMMDevice> dev;
      hr = collection->Item(i, dev.Put());
      if (FAILED(hr) || !dev) continue;

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
      if (SUCCEEDED(hr) && props) {
        friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
        if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
        instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
      }

      const auto hwids = GetHardwareIdsForInstanceId(instance_id);
      VirtioSndPciIdInfo hwid_info{};
      const bool hwid_allowed = IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional, &hwid_info);
      const auto inst_info = GetVirtioSndPciIdInfoFromString(instance_id);
      const bool inst_allowed = IsAllowedVirtioSndPciId(inst_info, allow_transitional);

      log.Logf("virtio-snd: endpoint idx=%u state=%s name=%s id=%s instance_id=%s",
               static_cast<unsigned>(i), MmDeviceStateToString(state), WideToUtf8(friendly).c_str(),
               WideToUtf8(dev_id).c_str(), WideToUtf8(instance_id).c_str());
      std::wstring pci_hwid;
      for (const auto& hwid : hwids) {
        if (ContainsInsensitive(hwid, L"PCI\\")) {
          pci_hwid = hwid;
          break;
        }
      }
      if (!pci_hwid.empty()) {
        log.Logf("virtio-snd: endpoint idx=%u pci_hwid=%s", static_cast<unsigned>(i),
                 WideToUtf8(pci_hwid).c_str());
      } else if (!hwids.empty()) {
        log.Logf("virtio-snd: endpoint idx=%u hwid0=%s", static_cast<unsigned>(i),
                 WideToUtf8(hwids[0]).c_str());
      }
      log.Logf(
          "virtio-snd: endpoint idx=%u virtio_snd_match inst(modern=%d rev01=%d transitional=%d allowed=%d) "
          "hw(modern=%d rev01=%d transitional=%d allowed=%d)",
          static_cast<unsigned>(i), inst_info.modern ? 1 : 0, inst_info.modern_rev01 ? 1 : 0,
          inst_info.transitional ? 1 : 0, inst_allowed ? 1 : 0, hwid_info.modern ? 1 : 0,
          hwid_info.modern_rev01 ? 1 : 0, hwid_info.transitional ? 1 : 0, hwid_allowed ? 1 : 0);

      if (state != DEVICE_STATE_ACTIVE) continue;

      int score = 0;
      if (ContainsInsensitive(friendly, L"virtio")) score += 100;
      if (ContainsInsensitive(friendly, L"aero")) score += 50;
      for (const auto& m : match_names) {
        if (!m.empty() && ContainsInsensitive(friendly, m)) score += 200;
      }
      if (hwid_info.modern) score += 1000;
      if (hwid_info.modern_rev01) score += 50;
      if (allow_transitional && hwid_info.transitional) score += 900;
      if (inst_info.modern) score += 800;
      if (inst_info.modern_rev01) score += 50;
      if (allow_transitional && inst_info.transitional) score += 700;

      if (score <= 0) continue;

      if (score > best_score && LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names, allow_transitional)) {
        best_score = score;
        best.device = std::move(dev);
        best.friendly = friendly;
        best.id = dev_id;
        best.instance_id = instance_id;
        best.pci_hwid = pci_hwid;
        best.score = score;
      }
    }

    if (best.device) return best;
    Sleep(1000);
  }

  return std::nullopt;
}

static TestResult VirtioSndTest(Logger& log, const std::vector<std::wstring>& match_names, bool allow_transitional) {
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

  const auto chosen_opt =
      FindVirtioSndRenderEndpoint(log, enumerator.Get(), match_names, allow_transitional, 20000);
  if (!chosen_opt.has_value()) {
    log.LogLine("virtio-snd: no matching ACTIVE render endpoint found");

    // Log the default endpoint (if any) for debugging.
    ComPtr<IMMDevice> def;
    hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, def.Put());
    if (SUCCEEDED(hr) && def) {
      ComPtr<IPropertyStore> props;
      if (SUCCEEDED(def->OpenPropertyStore(STGM_READ, props.Put())) && props) {
        const std::wstring friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
        const std::wstring instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
        log.Logf("virtio-snd: default endpoint name=%s instance_id=%s", WideToUtf8(friendly).c_str(),
                 WideToUtf8(instance_id).c_str());
      }
    } else {
      log.LogLine("virtio-snd: no default render endpoint available");
    }

    out.fail_reason = "no_matching_endpoint";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    return out;
  }

  const auto& chosen = *chosen_opt;
  out.endpoint_found = true;
  log.Logf("virtio-snd: selected endpoint name=%s id=%s instance_id=%s pci_hwid=%s score=%d",
           WideToUtf8(chosen.friendly).c_str(), WideToUtf8(chosen.id).c_str(),
           WideToUtf8(chosen.instance_id).c_str(), WideToUtf8(chosen.pci_hwid).c_str(), chosen.score);
  TryEnsureEndpointVolumeAudible(log, chosen.device.Get(), "render");
  TryEnsureEndpointSessionAudible(log, chosen.device.Get(), "render");

  ComPtr<IAudioClient> client;
  hr = chosen.device->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
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

  bool used_desired_format = false;
  hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
  if (SUCCEEDED(hr)) {
    used_desired_format = true;
  } else {
    log.Logf(
        "virtio-snd: Initialize(shared desired 48kHz S16 stereo) failed hr=0x%08lx; trying WAVE_FORMAT_EXTENSIBLE",
        static_cast<unsigned long>(hr));

    fmt_bytes.resize(sizeof(WAVEFORMATEXTENSIBLE));
    auto* ext = reinterpret_cast<WAVEFORMATEXTENSIBLE*>(fmt_bytes.data());
    *ext = {};
    ext->Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    ext->Format.nChannels = 2;
    ext->Format.nSamplesPerSec = 48000;
    ext->Format.wBitsPerSample = 16;
    ext->Format.nBlockAlign = static_cast<WORD>((ext->Format.nChannels * ext->Format.wBitsPerSample) / 8);
    ext->Format.nAvgBytesPerSec = ext->Format.nSamplesPerSec * ext->Format.nBlockAlign;
    ext->Format.cbSize = static_cast<WORD>(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
    ext->Samples.wValidBitsPerSample = 16;
    ext->dwChannelMask = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT;
    ext->SubFormat = kWaveSubFormatPcm;
    desired = &ext->Format;

    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_shared_failed";
      out.hr = hr;
      log.Logf("virtio-snd: Initialize(shared desired extensible) failed hr=0x%08lx", static_cast<unsigned long>(hr));
      return out;
    }
  }

  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  log.Logf("virtio-snd: stream format=%s used_desired=%d", WaveFormatToString(fmt).c_str(),
           used_desired_format ? 1 : 0);
  TryEnsureAudioClientSessionAudible(log, client.Get(), "render");

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

static size_t WaveFormatTotalSizeBytes(const WAVEFORMATEX* fmt) {
  if (!fmt) return 0;
  // WAVEFORMATEX::cbSize is the number of bytes after the base WAVEFORMATEX struct.
  const size_t extra = fmt->cbSize;
  // Guard against corrupted headers (e.g. uninitialized pointers) producing unreasonable sizes.
  if (extra > 4096) return 0;
  return sizeof(WAVEFORMATEX) + extra;
}

static std::vector<BYTE> CopyWaveFormatBytes(const WAVEFORMATEX* fmt) {
  std::vector<BYTE> out;
  const size_t size = WaveFormatTotalSizeBytes(fmt);
  if (size == 0) return out;
  out.resize(size);
  memcpy(out.data(), fmt, size);
  return out;
}

static bool HrLooksLikeAudclntError(HRESULT hr) {
  // AUDCLNT_E_* codes typically sit in the 0x88890000 range.
  const uint32_t u = static_cast<uint32_t>(hr);
  return (u & 0xFFFF0000u) == 0x88890000u;
}

static bool HrIsExpectedSndBufferLimitsFailure(HRESULT hr) {
  if (hr == E_INVALIDARG) return true;
  if (hr == HRESULT_FROM_WIN32(ERROR_NOT_SUPPORTED)) return true;
  if (hr == HRESULT_FROM_WIN32(ERROR_BAD_FORMAT)) return true;
  if (HrLooksLikeAudclntError(hr)) return true;
  return false;
}

struct VirtioSndBufferLimitsTestResult {
  bool ok = false;
  bool endpoint_found = false;
  bool timed_out = false;

  // Initialize outcome.
  bool init_succeeded = false;
  bool expected_failure = false;
  HRESULT init_hr = E_FAIL;
  HRESULT hr = E_FAIL;
  std::string fail_reason;

  // Diagnostics.
  std::string mode; // "exclusive" or "shared"
  std::string format;
  REFERENCE_TIME requested_buffer_hns = 0;
  REFERENCE_TIME requested_period_hns = 0;
  UINT32 buffer_frames = 0;
  UINT64 buffer_bytes = 0;
};

static bool BufferFramesToBytes(const WAVEFORMATEX* fmt, UINT32 frames, UINT64* out_bytes) {
  if (!out_bytes) return false;
  *out_bytes = 0;
  if (!fmt) return false;
  if (frames == 0) return false;
  if (fmt->nBlockAlign == 0) return false;
  *out_bytes = static_cast<UINT64>(frames) * static_cast<UINT64>(fmt->nBlockAlign);
  return true;
}

static VirtioSndBufferLimitsTestResult VirtioSndBufferLimitsAttempt(Logger& log, IMMDevice* endpoint,
                                                                    const char* mode_name,
                                                                    AUDCLNT_SHAREMODE sharemode,
                                                                    REFERENCE_TIME buffer_hns,
                                                                    REFERENCE_TIME period_hns,
                                                                    const std::vector<BYTE>& fmt_bytes) {
  VirtioSndBufferLimitsTestResult out{};
  out.mode = mode_name ? mode_name : "";
  out.requested_buffer_hns = buffer_hns;
  out.requested_period_hns = period_hns;

  if (!endpoint) {
    out.fail_reason = "endpoint_null";
    out.hr = E_POINTER;
    out.init_hr = out.hr;
    return out;
  }
  if (fmt_bytes.empty()) {
    out.fail_reason = "format_empty";
    out.hr = E_INVALIDARG;
    out.init_hr = out.hr;
    return out;
  }

  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  out.format = WaveFormatToString(fmt);

  ComPtr<IAudioClient> client;
  HRESULT hr = endpoint->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                                  reinterpret_cast<void**>(client.Put()));
  if (FAILED(hr) || !client) {
    out.fail_reason = "activate_audio_client_failed";
    out.hr = hr;
    out.init_hr = hr;
    return out;
  }

  hr = client->Initialize(sharemode, 0, buffer_hns, period_hns, fmt, nullptr);
  out.init_hr = hr;
  out.hr = hr;

  if (FAILED(hr)) {
    // The key property of this stress test is that Initialize returns (no hang/crash). A failure
    // HRESULT is acceptable as long as it is handled. Record whether it looks like an "expected"
    // WASAPI buffer/period/format failure for diagnostics.
    out.expected_failure = HrIsExpectedSndBufferLimitsFailure(hr);
    out.ok = true;
    return out;
  }

  out.init_succeeded = true;

  UINT32 frames = 0;
  const HRESULT size_hr = client->GetBufferSize(&frames);
  if (FAILED(size_hr) || frames == 0) {
    out.ok = false;
    out.fail_reason = "get_buffer_size_failed";
    out.hr = FAILED(size_hr) ? size_hr : E_FAIL;
    return out;
  }
  out.buffer_frames = frames;

  UINT64 bytes = 0;
  if (!BufferFramesToBytes(fmt, frames, &bytes)) {
    out.ok = false;
    out.fail_reason = "invalid_buffer_size";
    out.hr = E_FAIL;
    return out;
  }
  out.buffer_bytes = bytes;

  // If Initialize succeeded but returned a truly enormous buffer size, treat it as inconsistent.
  // (The stress test requests ~8MiB; anything wildly larger suggests an overflow or misreport.)
  constexpr UINT64 kMaxPlausibleBufferBytes = 256ull * 1024ull * 1024ull;
  if (bytes > kMaxPlausibleBufferBytes) {
    out.ok = false;
    out.fail_reason = "buffer_size_insane";
    out.hr = E_FAIL;
    return out;
  }

  out.ok = true;
  return out;
}

static VirtioSndBufferLimitsTestResult VirtioSndBufferLimitsTestInternal(Logger& log,
                                                                         const std::vector<std::wstring>& match_names,
                                                                         bool allow_transitional) {
  VirtioSndBufferLimitsTestResult out{};

  ScopedCoInitialize com(COINIT_MULTITHREADED);
  if (FAILED(com.hr())) {
    out.fail_reason = "com_init_failed";
    out.hr = com.hr();
    out.init_hr = out.hr;
    log.Logf("virtio-snd: buffer-limits CoInitializeEx failed hr=0x%08lx", static_cast<unsigned long>(out.hr));
    return out;
  }

  ComPtr<IMMDeviceEnumerator> enumerator;
  HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                                __uuidof(IMMDeviceEnumerator),
                                reinterpret_cast<void**>(enumerator.Put()));
  if (FAILED(hr) || !enumerator) {
    out.fail_reason = "create_device_enumerator_failed";
    out.hr = hr;
    out.init_hr = hr;
    log.Logf("virtio-snd: buffer-limits CoCreateInstance(MMDeviceEnumerator) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  const auto chosen_opt = FindVirtioSndRenderEndpoint(log, enumerator.Get(), match_names, allow_transitional, 20000);
  if (!chosen_opt.has_value()) {
    out.fail_reason = "no_matching_endpoint";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    out.init_hr = out.hr;
    log.LogLine("virtio-snd: buffer-limits no matching ACTIVE render endpoint found");
    return out;
  }
  const auto& chosen = *chosen_opt;
  out.endpoint_found = true;

  log.Logf("virtio-snd: buffer-limits selected endpoint name=%s id=%s instance_id=%s pci_hwid=%s score=%d",
           WideToUtf8(chosen.friendly).c_str(), WideToUtf8(chosen.id).c_str(),
           WideToUtf8(chosen.instance_id).c_str(), WideToUtf8(chosen.pci_hwid).c_str(), chosen.score);

  // Probe the endpoint for a stable mix format. Using the mix format ensures the Initialize call
  // exercises buffer sizing (not format negotiation).
  ComPtr<IAudioClient> probe;
  hr = chosen.device->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                               reinterpret_cast<void**>(probe.Put()));
  if (FAILED(hr) || !probe) {
    out.fail_reason = "activate_audio_client_failed";
    out.hr = hr;
    out.init_hr = hr;
    log.Logf("virtio-snd: buffer-limits Activate(IAudioClient) failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }

  WAVEFORMATEX* mix_raw = nullptr;
  hr = probe->GetMixFormat(&mix_raw);
  if (FAILED(hr) || !mix_raw) {
    out.fail_reason = "get_mix_format_failed";
    out.hr = FAILED(hr) ? hr : E_FAIL;
    out.init_hr = out.hr;
    log.Logf("virtio-snd: buffer-limits GetMixFormat failed hr=0x%08lx", static_cast<unsigned long>(out.hr));
    return out;
  }

  std::vector<BYTE> mix_bytes = CopyWaveFormatBytes(mix_raw);
  const std::string mix_str = WaveFormatToString(mix_raw);

  const uint32_t sample_rate = mix_raw->nSamplesPerSec;
  const uint32_t block_align = mix_raw->nBlockAlign;
  uint64_t bytes_per_sec = mix_raw->nAvgBytesPerSec;
  if (bytes_per_sec == 0 && sample_rate != 0 && block_align != 0) {
    bytes_per_sec = static_cast<uint64_t>(sample_rate) * static_cast<uint64_t>(block_align);
  }

  CoTaskMemFree(mix_raw);
  mix_raw = nullptr;

  if (mix_bytes.empty()) {
    out.fail_reason = "copy_mix_format_failed";
    out.hr = E_FAIL;
    out.init_hr = out.hr;
    log.LogLine("virtio-snd: buffer-limits unable to copy mix format");
    return out;
  }
  if (sample_rate == 0 || block_align == 0 || bytes_per_sec == 0) {
    out.fail_reason = "invalid_mix_format";
    out.hr = E_FAIL;
    out.init_hr = out.hr;
    log.Logf("virtio-snd: buffer-limits invalid mix format=%s", mix_str.c_str());
    return out;
  }

  // Target an ~8MiB audio buffer to stress virtio-snd buffer sizing constraints without allocating
  // excessive guest memory.
  constexpr uint64_t kTargetBytes = 8ull * 1024ull * 1024ull;
  const uint64_t duration_sec = std::max<uint64_t>(1, (kTargetBytes + bytes_per_sec - 1) / bytes_per_sec);
  const REFERENCE_TIME requested_buffer_hns =
      static_cast<REFERENCE_TIME>(duration_sec * 10000000ull); // seconds -> 100ns units

  log.Logf("virtio-snd: buffer-limits mix_format=%s bytes_per_sec=%llu target_bytes=%llu duration_sec=%llu",
           mix_str.c_str(), static_cast<unsigned long long>(bytes_per_sec),
           static_cast<unsigned long long>(kTargetBytes), static_cast<unsigned long long>(duration_sec));

  REFERENCE_TIME default_period = 0;
  REFERENCE_TIME min_period = 0;
  if (FAILED(probe->GetDevicePeriod(&default_period, &min_period))) {
    default_period = 0;
    min_period = 0;
  }

  // Attempt exclusive first (lets us specify both buffer duration + periodicity). If exclusive isn't
  // possible, fall back to shared.
  std::vector<BYTE> excl_bytes;
  REFERENCE_TIME excl_period = 0;
  if (min_period > 0) {
    excl_period = min_period;
  } else if (default_period > 0) {
    excl_period = default_period;
  }

  if (excl_period > 0) {
    WAVEFORMATEX* closest = nullptr;
    const auto* mix_fmt = reinterpret_cast<const WAVEFORMATEX*>(mix_bytes.data());
    const HRESULT fmt_hr = probe->IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, mix_fmt, &closest);
    if (fmt_hr == S_OK) {
      excl_bytes = mix_bytes;
    } else if (fmt_hr == S_FALSE && closest) {
      excl_bytes = CopyWaveFormatBytes(closest);
    }
    if (closest) CoTaskMemFree(closest);
  }

  if (!excl_bytes.empty() && excl_period > 0) {
    // Ensure the exclusive buffer duration is a multiple of periodicity.
    const REFERENCE_TIME aligned_buffer_hns =
        ((requested_buffer_hns + excl_period - 1) / excl_period) * excl_period;
    auto excl = VirtioSndBufferLimitsAttempt(log, chosen.device.Get(), "exclusive", AUDCLNT_SHAREMODE_EXCLUSIVE,
                                             aligned_buffer_hns, excl_period, excl_bytes);
    excl.endpoint_found = true;
    if (excl.ok && excl.init_succeeded) {
      return excl;
    }
    // If exclusive doesn't succeed, attempt shared mode as well (to avoid reporting an exclusive-only
    // configuration issue as a buffer sizing regression).
    auto shared = VirtioSndBufferLimitsAttempt(log, chosen.device.Get(), "shared", AUDCLNT_SHAREMODE_SHARED,
                                               requested_buffer_hns, 0, mix_bytes);
    shared.endpoint_found = true;
    if (shared.ok) return shared;
    return excl;
  }

  auto shared = VirtioSndBufferLimitsAttempt(log, chosen.device.Get(), "shared", AUDCLNT_SHAREMODE_SHARED,
                                             requested_buffer_hns, 0, mix_bytes);
  shared.endpoint_found = true;
  return shared;
}

struct VirtioSndBufferLimitsThreadContext {
  Logger* log = nullptr;
  std::vector<std::wstring> match_names;
  bool allow_transitional = false;
  HANDLE done_event = nullptr;
  VirtioSndBufferLimitsTestResult result{};
};

static DWORD WINAPI VirtioSndBufferLimitsThreadProc(void* param) {
  auto* ctx = reinterpret_cast<VirtioSndBufferLimitsThreadContext*>(param);
  if (!ctx) return 0;
  if (ctx->log) {
    ctx->result = VirtioSndBufferLimitsTestInternal(*ctx->log, ctx->match_names, ctx->allow_transitional);
  } else {
    ctx->result.ok = false;
    ctx->result.fail_reason = "logger_null";
    ctx->result.hr = E_POINTER;
    ctx->result.init_hr = ctx->result.hr;
  }
  if (ctx->done_event) SetEvent(ctx->done_event);
  return 0;
}

static VirtioSndBufferLimitsTestResult VirtioSndBufferLimitsTest(Logger& log,
                                                                 const std::vector<std::wstring>& match_names,
                                                                 bool allow_transitional) {
  VirtioSndBufferLimitsThreadContext ctx{};
  ctx.log = &log;
  ctx.match_names = match_names;
  ctx.allow_transitional = allow_transitional;

  ctx.done_event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (!ctx.done_event) {
    VirtioSndBufferLimitsTestResult out{};
    out.ok = false;
    out.fail_reason = "create_event_failed";
    out.hr = HRESULT_FROM_WIN32(GetLastError());
    out.init_hr = out.hr;
    return out;
  }

  DWORD thread_id = 0;
  HANDLE thread = CreateThread(nullptr, 0, VirtioSndBufferLimitsThreadProc, &ctx, 0, &thread_id);
  if (!thread) {
    CloseHandle(ctx.done_event);
    VirtioSndBufferLimitsTestResult out{};
    out.ok = false;
    out.fail_reason = "create_thread_failed";
    out.hr = HRESULT_FROM_WIN32(GetLastError());
    out.init_hr = out.hr;
    return out;
  }

  // Bound runtime so a buggy driver can't hang the entire selftest when asked for extreme buffer sizes.
  constexpr DWORD kTimeoutMs = 30000;
  const DWORD wait_rc = WaitForSingleObject(ctx.done_event, kTimeoutMs);
  if (wait_rc != WAIT_OBJECT_0) {
    log.Logf("virtio-snd: buffer-limits timed out wait_rc=%lu", static_cast<unsigned long>(wait_rc));
    TerminateThread(thread, 1);
    WaitForSingleObject(thread, 5000);
    CloseHandle(thread);
    CloseHandle(ctx.done_event);

    VirtioSndBufferLimitsTestResult out{};
    out.ok = false;
    out.timed_out = true;
    out.fail_reason = "timeout";
    out.hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
    out.init_hr = out.hr;
    return out;
  }

  WaitForSingleObject(thread, 5000);
  CloseHandle(thread);
  CloseHandle(ctx.done_event);
  return ctx.result;
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

static bool WaveOutToneTest(Logger& log, const std::vector<std::wstring>& match_names, bool allow_transitional) {
  const UINT num = waveOutGetNumDevs();
  log.Logf("virtio-snd: waveOut devices=%u", num);
  if (num == 0) return false;

  // Ensure the master volume isn't muted/at 0 before attempting the winmm fallback.
  // This is best-effort; failures do not cause the test to fail directly.
  TryEnsureDefaultRenderEndpointAudible(log);

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
      const auto inst_info = GetVirtioSndPciIdInfoFromString(*inst_id);
      if (inst_info.modern || (allow_transitional && inst_info.transitional)) {
        score += 500;
      }
      const auto hwids = GetHardwareIdsForInstanceId(*inst_id);
      if (IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional)) score += 1000;
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
    if (num == 1) {
      // Some audio stacks (or SDK header combinations) may not expose a usable device instance ID
      // via DRV_QUERYDEVICEINSTANCEID, and the device name may not mention "virtio". If there is
      // only a single waveOut device, assume it is the virtio-snd-backed endpoint.
      device_id = 0;
      log.LogLine("virtio-snd: waveOut no matching device; using only device_id=0");
    } else {
      log.LogLine("virtio-snd: waveOut no matching device found");
      return false;
    }
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

static std::wstring WinmmInErrorToWide(MMRESULT rc) {
  wchar_t buf[256]{};
  if (waveInGetErrorTextW(rc, buf, static_cast<UINT>(sizeof(buf) / sizeof(buf[0]))) == MMSYSERR_NOERROR) {
    return std::wstring(buf);
  }
  return L"";
}

static std::optional<std::wstring> WaveInDeviceInstanceId(UINT device_id) {
  wchar_t buf[512]{};
  const MMRESULT rc = waveInMessage(reinterpret_cast<HWAVEIN>(static_cast<UINT_PTR>(device_id)),
                                    DRV_QUERYDEVICEINSTANCEID, reinterpret_cast<DWORD_PTR>(buf), sizeof(buf));
  if (rc != MMSYSERR_NOERROR) return std::nullopt;
  buf[(sizeof(buf) / sizeof(buf[0])) - 1] = L'\0';
  if (buf[0] == L'\0') return std::nullopt;
  return std::wstring(buf);
}

static TestResult WaveInCaptureTest(Logger& log, const std::vector<std::wstring>& match_names, bool allow_transitional,
                                    bool require_non_silence) {
  TestResult out{};
  const UINT num = waveInGetNumDevs();
  log.Logf("virtio-snd: waveIn capture devices=%u", num);
  if (num == 0) {
    out.fail_reason = "no_wavein_devices";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    return out;
  }

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
    WAVEINCAPSW caps{};
    const MMRESULT rc = waveInGetDevCapsW(i, &caps, sizeof(caps));
    if (rc != MMSYSERR_NOERROR) continue;

    int score = 0;
    if (name_matches(caps.szPname)) score += 100;

    const auto inst_id = WaveInDeviceInstanceId(i);
    if (inst_id.has_value()) {
      log.Logf("virtio-snd: waveIn[%u]=%s instance_id=%s", i, WideToUtf8(caps.szPname).c_str(),
               WideToUtf8(*inst_id).c_str());
      const auto inst_info = GetVirtioSndPciIdInfoFromString(*inst_id);
      if (inst_info.modern || (allow_transitional && inst_info.transitional)) {
        score += 500;
      }
      const auto hwids = GetHardwareIdsForInstanceId(*inst_id);
      if (IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional)) score += 1000;
    } else {
      log.Logf("virtio-snd: waveIn[%u]=%s instance_id=<unavailable>", i, WideToUtf8(caps.szPname).c_str());
    }

    if (score > best_score) {
      best_score = score;
      device_id = i;
    }
  }

  if (device_id == UINT_MAX || best_score <= 0) {
    log.LogLine("virtio-snd: waveIn no matching device found");
    out.fail_reason = "no_matching_device";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    return out;
  } else {
    log.Logf("virtio-snd: waveIn using device_id=%u score=%d", device_id, best_score);
  }

  HANDLE done_event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (!done_event) {
    log.Logf("virtio-snd: waveIn CreateEvent failed err=%lu", GetLastError());
    out.fail_reason = "create_event_failed";
    out.hr = HRESULT_FROM_WIN32(GetLastError());
    return out;
  }

  auto try_open = [&](WORD channels, HWAVEIN* out_hwi, WAVEFORMATEX* out_fmt) -> MMRESULT {
    if (!out_hwi || !out_fmt) return MMSYSERR_INVALPARAM;
    *out_hwi = nullptr;
    *out_fmt = {};
    out_fmt->wFormatTag = WAVE_FORMAT_PCM;
    out_fmt->nChannels = channels;
    out_fmt->nSamplesPerSec = 48000;
    out_fmt->wBitsPerSample = 16;
    out_fmt->nBlockAlign = static_cast<WORD>((out_fmt->nChannels * out_fmt->wBitsPerSample) / 8);
    out_fmt->nAvgBytesPerSec = out_fmt->nSamplesPerSec * out_fmt->nBlockAlign;

    return waveInOpen(out_hwi, device_id, out_fmt, reinterpret_cast<DWORD_PTR>(done_event), 0, CALLBACK_EVENT);
  };

  HWAVEIN hwi = nullptr;
  WAVEFORMATEX fmt{};
  MMRESULT rc = try_open(1, &hwi, &fmt);
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveInOpen mono failed rc=%u text=%s; trying stereo", rc,
             WideToUtf8(WinmmInErrorToWide(rc)).c_str());
    rc = try_open(2, &hwi, &fmt);
  }
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveInOpen failed rc=%u text=%s", rc, WideToUtf8(WinmmInErrorToWide(rc)).c_str());
    CloseHandle(done_event);
    out.fail_reason = "wavein_open_failed";
    out.hr = E_FAIL;
    return out;
  }

  ResetEvent(done_event);

  const UINT32 frames = fmt.nSamplesPerSec / 4; // 250ms
  std::vector<BYTE> data(static_cast<size_t>(frames) * fmt.nBlockAlign);

  WAVEHDR hdr{};
  hdr.lpData = reinterpret_cast<LPSTR>(data.data());
  hdr.dwBufferLength = static_cast<DWORD>(data.size());

  rc = waveInPrepareHeader(hwi, &hdr, sizeof(hdr));
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveInPrepareHeader failed rc=%u text=%s", rc,
             WideToUtf8(WinmmInErrorToWide(rc)).c_str());
    waveInClose(hwi);
    CloseHandle(done_event);
    out.fail_reason = "wavein_prepare_header_failed";
    out.hr = E_FAIL;
    return out;
  }

  rc = waveInAddBuffer(hwi, &hdr, sizeof(hdr));
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveInAddBuffer failed rc=%u text=%s", rc, WideToUtf8(WinmmInErrorToWide(rc)).c_str());
    waveInUnprepareHeader(hwi, &hdr, sizeof(hdr));
    waveInClose(hwi);
    CloseHandle(done_event);
    out.fail_reason = "wavein_add_buffer_failed";
    out.hr = E_FAIL;
    return out;
  }

  rc = waveInStart(hwi);
  if (rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveInStart failed rc=%u text=%s", rc, WideToUtf8(WinmmInErrorToWide(rc)).c_str());
    waveInReset(hwi);
    waveInUnprepareHeader(hwi, &hdr, sizeof(hdr));
    waveInClose(hwi);
    CloseHandle(done_event);
    out.fail_reason = "wavein_start_failed";
    out.hr = E_FAIL;
    return out;
  }

  const DWORD wait_rc = WaitForSingleObject(done_event, 5000);
  if (wait_rc != WAIT_OBJECT_0) {
    log.Logf("virtio-snd: waveIn timed out wait_rc=%lu", wait_rc);
    waveInStop(hwi);
    waveInReset(hwi);
    waveInUnprepareHeader(hwi, &hdr, sizeof(hdr));
    waveInClose(hwi);
    CloseHandle(done_event);
    out.fail_reason = "capture_timeout";
    out.hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
    return out;
  }

  waveInStop(hwi);
  waveInReset(hwi);

  const bool got_bytes = hdr.dwBytesRecorded > 0;
  log.Logf("virtio-snd: waveIn captured bytes=%lu flags=0x%08lx", static_cast<unsigned long>(hdr.dwBytesRecorded),
           static_cast<unsigned long>(hdr.dwFlags));
  out.captured_frames = (fmt.nBlockAlign != 0) ? (static_cast<UINT64>(hdr.dwBytesRecorded) / fmt.nBlockAlign) : 0;
  const bool non_silence = got_bytes && BufferContainsNonSilence(&fmt, data.data(), hdr.dwBytesRecorded);
  out.captured_non_silence = non_silence;
  out.captured_silence_only = got_bytes && !non_silence;

  waveInUnprepareHeader(hwi, &hdr, sizeof(hdr));
  waveInClose(hwi);
  CloseHandle(done_event);

  if (!got_bytes) {
    log.LogLine("virtio-snd: waveIn capture did not return any bytes");
    out.fail_reason = "capture_no_bytes";
    out.hr = HRESULT_FROM_WIN32(ERROR_NO_DATA);
    return out;
  }

  if (require_non_silence && !non_silence) {
    log.LogLine("virtio-snd: waveIn capture returned only silence; failing (--require-non-silence)");
    out.fail_reason = "captured_silence";
    out.hr = E_FAIL;
    return out;
  }

  log.Logf("virtio-snd: waveIn capture ok (non_silence=%d)", non_silence ? 1 : 0);
  out.ok = true;
  out.hr = S_OK;
  out.fail_reason.clear();
  return out;
}

static TestResult VirtioSndCaptureTest(Logger& log, const std::vector<std::wstring>& match_names, bool smoke_test,
                                       DWORD endpoint_wait_ms, bool allow_transitional, bool require_non_silence) {
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
                                __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(enumerator.Put()));
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
  std::wstring chosen_instance_id;
  std::wstring chosen_pci_hwid;
  int best_score = -1;

  const DWORD deadline_ms = GetTickCount() + endpoint_wait_ms;
  int attempt = 0;

  for (;;) {
    attempt++;

    ComPtr<IMMDeviceCollection> collection;
    const DWORD state_mask =
        DEVICE_STATE_ACTIVE | DEVICE_STATE_DISABLED | DEVICE_STATE_NOTPRESENT | DEVICE_STATE_UNPLUGGED;
    hr = enumerator->EnumAudioEndpoints(eCapture, state_mask, collection.Put());
    if (FAILED(hr)) {
      log.Logf("virtio-snd: EnumAudioEndpoints(eCapture) failed hr=0x%08lx attempt=%d",
               static_cast<unsigned long>(hr), attempt);
      if (endpoint_wait_ms != 0 && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
        Sleep(1000);
        continue;
      }
      break;
    }

    UINT count = 0;
    hr = collection->GetCount(&count);
    if (FAILED(hr)) {
      log.Logf("virtio-snd: IMMDeviceCollection::GetCount failed hr=0x%08lx", static_cast<unsigned long>(hr));
      if (endpoint_wait_ms != 0 && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
        Sleep(1000);
        continue;
      }
      break;
    }

    log.Logf("virtio-snd: capture endpoints count=%u attempt=%d", count, attempt);

    best_score = -1;
    chosen.Reset();
    chosen_friendly.clear();
    chosen_id.clear();
    chosen_instance_id.clear();
    chosen_pci_hwid.clear();

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
      VirtioSndPciIdInfo hwid_info{};
      const bool hwid_allowed = IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional, &hwid_info);
      const auto inst_info = GetVirtioSndPciIdInfoFromString(instance_id);
      const bool inst_allowed = IsAllowedVirtioSndPciId(inst_info, allow_transitional);

      log.Logf("virtio-snd: capture endpoint idx=%u state=%s name=%s id=%s instance_id=%s", static_cast<unsigned>(i),
               MmDeviceStateToString(state), WideToUtf8(friendly).c_str(), WideToUtf8(dev_id).c_str(),
               WideToUtf8(instance_id).c_str());
      std::wstring pci_hwid;
      for (const auto& hwid : hwids) {
        if (ContainsInsensitive(hwid, L"PCI\\")) {
          pci_hwid = hwid;
          break;
        }
      }
      if (!pci_hwid.empty()) {
        log.Logf("virtio-snd: capture endpoint idx=%u pci_hwid=%s", static_cast<unsigned>(i),
                 WideToUtf8(pci_hwid).c_str());
      } else if (!hwids.empty()) {
        log.Logf("virtio-snd: capture endpoint idx=%u hwid0=%s", static_cast<unsigned>(i),
                 WideToUtf8(hwids[0]).c_str());
      }
      log.Logf(
          "virtio-snd: capture endpoint idx=%u virtio_snd_match inst(modern=%d rev01=%d transitional=%d allowed=%d) "
          "hw(modern=%d rev01=%d transitional=%d allowed=%d)",
          static_cast<unsigned>(i), inst_info.modern ? 1 : 0, inst_info.modern_rev01 ? 1 : 0,
          inst_info.transitional ? 1 : 0, inst_allowed ? 1 : 0, hwid_info.modern ? 1 : 0,
          hwid_info.modern_rev01 ? 1 : 0, hwid_info.transitional ? 1 : 0, hwid_allowed ? 1 : 0);

      if (state != DEVICE_STATE_ACTIVE) continue;

      int score = 0;
      if (ContainsInsensitive(friendly, L"virtio")) score += 100;
      if (ContainsInsensitive(friendly, L"aero")) score += 50;
      for (const auto& m : match_names) {
        if (!m.empty() && ContainsInsensitive(friendly, m)) score += 200;
      }
      if (hwid_info.modern) score += 1000;
      if (hwid_info.modern_rev01) score += 50;
      if (allow_transitional && hwid_info.transitional) score += 900;
      if (inst_info.modern) score += 800;
      if (inst_info.modern_rev01) score += 50;
      if (allow_transitional && inst_info.transitional) score += 700;

      if (score <= 0) continue;

      if (score > best_score && LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names, allow_transitional)) {
        best_score = score;
        chosen = std::move(dev);
        chosen_friendly = friendly;
        chosen_id = dev_id;
        chosen_instance_id = instance_id;
        chosen_pci_hwid = pci_hwid;
      }
    }

    if (chosen) break;
    if (endpoint_wait_ms == 0 || static_cast<int32_t>(GetTickCount() - deadline_ms) >= 0) break;
    Sleep(1000);
  }

  if (!chosen) {
    log.LogLine("virtio-snd: no matching ACTIVE capture endpoint found; checking default endpoint");
    hr = enumerator->GetDefaultAudioEndpoint(eCapture, eConsole, chosen.Put());
    if (FAILED(hr) || !chosen) {
      out.fail_reason = "no_matching_endpoint";
      out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
      log.LogLine("virtio-snd: no default capture endpoint available");
      return out;
    }

    ComPtr<IPropertyStore> props;
    hr = chosen->OpenPropertyStore(STGM_READ, props.Put());
    std::wstring friendly;
    std::wstring instance_id;
    if (SUCCEEDED(hr)) {
      friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
      if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
      instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
    }
    const auto hwids = GetHardwareIdsForInstanceId(instance_id);
    if (!LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names, allow_transitional)) {
      out.fail_reason = "no_matching_endpoint";
      out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
      log.Logf("virtio-snd: default capture endpoint does not look like virtio-snd (name=%s instance_id=%s)",
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
    chosen_instance_id = instance_id;
    for (const auto& hwid : hwids) {
      if (ContainsInsensitive(hwid, L"PCI\\")) {
        chosen_pci_hwid = hwid;
        break;
      }
    }
  }

  out.endpoint_found = true;
  log.Logf("virtio-snd: selected capture endpoint name=%s id=%s instance_id=%s pci_hwid=%s score=%d",
           WideToUtf8(chosen_friendly).c_str(), WideToUtf8(chosen_id).c_str(),
           WideToUtf8(chosen_instance_id).c_str(), WideToUtf8(chosen_pci_hwid).c_str(), best_score);

  const bool do_smoke_test = smoke_test || require_non_silence;
  if (!do_smoke_test) {
    out.ok = true;
    out.hr = S_OK;
    out.fail_reason.clear();
    return out;
  }

  ComPtr<IAudioClient> client;
  hr = chosen->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                        reinterpret_cast<void**>(client.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "activate_audio_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: capture IMMDevice::Activate(IAudioClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  std::vector<BYTE> fmt_bytes;
  fmt_bytes.resize(sizeof(WAVEFORMATEX));
  auto* desired = reinterpret_cast<WAVEFORMATEX*>(fmt_bytes.data());
  *desired = {};
  desired->wFormatTag = WAVE_FORMAT_PCM;
  desired->nChannels = 1;
  desired->nSamplesPerSec = 48000;
  desired->wBitsPerSample = 16;
  desired->nBlockAlign = static_cast<WORD>((desired->nChannels * desired->wBitsPerSample) / 8);
  desired->nAvgBytesPerSec = desired->nSamplesPerSec * desired->nBlockAlign;
  desired->cbSize = 0;

  log.Logf("virtio-snd: capture desired format=%s", WaveFormatToString(desired).c_str());

  WAVEFORMATEX* mix = nullptr;
  hr = client->GetMixFormat(&mix);
  if (SUCCEEDED(hr) && mix) {
    log.Logf("virtio-snd: capture mix format=%s", WaveFormatToString(mix).c_str());
    CoTaskMemFree(mix);
  } else {
    log.Logf("virtio-snd: capture GetMixFormat failed hr=0x%08lx (continuing)", static_cast<unsigned long>(hr));
  }
  constexpr REFERENCE_TIME kBufferDuration100ms = 1000000; // 100ms in 100ns units
  hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
  if (FAILED(hr)) {
    log.Logf("virtio-snd: capture Initialize(shared desired 48kHz S16 mono) failed hr=0x%08lx; trying WAVE_FORMAT_EXTENSIBLE",
             static_cast<unsigned long>(hr));

    fmt_bytes.resize(sizeof(WAVEFORMATEXTENSIBLE));
    auto* ext = reinterpret_cast<WAVEFORMATEXTENSIBLE*>(fmt_bytes.data());
    *ext = {};
    ext->Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    ext->Format.nChannels = 1;
    ext->Format.nSamplesPerSec = 48000;
    ext->Format.wBitsPerSample = 16;
    ext->Format.nBlockAlign = static_cast<WORD>((ext->Format.nChannels * ext->Format.wBitsPerSample) / 8);
    ext->Format.nAvgBytesPerSec = ext->Format.nSamplesPerSec * ext->Format.nBlockAlign;
    ext->Format.cbSize = static_cast<WORD>(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
    ext->Samples.wValidBitsPerSample = 16;
    ext->dwChannelMask = SPEAKER_FRONT_CENTER;
    ext->SubFormat = kWaveSubFormatPcm;
    desired = &ext->Format;

    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_fixed_failed";
      out.hr = hr;
      log.Logf("virtio-snd: capture Initialize(shared desired extensible) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
  }
  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  const DWORD sample_rate_hz = fmt->nSamplesPerSec;

  UINT32 buffer_frames = 0;
  hr = client->GetBufferSize(&buffer_frames);
  if (FAILED(hr) || buffer_frames == 0) {
    out.fail_reason = "get_buffer_size_failed";
    out.hr = FAILED(hr) ? hr : E_FAIL;
    log.Logf("virtio-snd: capture GetBufferSize failed hr=0x%08lx buffer_frames=%u",
             static_cast<unsigned long>(out.hr), buffer_frames);
    return out;
  }

  ComPtr<IAudioCaptureClient> capture;
  hr = client->GetService(__uuidof(IAudioCaptureClient), reinterpret_cast<void**>(capture.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "get_capture_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: capture GetService(IAudioCaptureClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  hr = client->Start();
  if (FAILED(hr)) {
    out.fail_reason = "start_failed";
    out.hr = hr;
    log.Logf("virtio-snd: capture IAudioClient::Start failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }

  const UINT64 min_frames =
      (sample_rate_hz != 0) ? std::max<UINT64>(1, static_cast<UINT64>(sample_rate_hz) / 10) : 1;
  UINT64 total_frames = 0;
  UINT64 silent_frames = 0;
  UINT64 non_silent_frames = 0;
  DWORD captured_flags = 0;
  const DWORD capture_deadline = GetTickCount() + 2500;
  while (static_cast<int32_t>(GetTickCount() - capture_deadline) < 0) {
    UINT32 packet_frames = 0;
    hr = capture->GetNextPacketSize(&packet_frames);
    if (FAILED(hr)) {
      out.fail_reason = "get_next_packet_size_failed";
      out.hr = hr;
      log.Logf("virtio-snd: capture GetNextPacketSize failed hr=0x%08lx", static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }
    if (packet_frames == 0) {
      Sleep(5);
      continue;
    }

    BYTE* data = nullptr;
    UINT32 frames = 0;
    DWORD flags = 0;
    hr = capture->GetBuffer(&data, &frames, &flags, nullptr, nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "get_buffer_failed";
      out.hr = hr;
      log.Logf("virtio-snd: capture GetBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }

    if (frames > 0) {
      total_frames += frames;
      captured_flags = flags;
      if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {
        silent_frames += frames;
      } else if (fmt->nBlockAlign != 0) {
        const size_t bytes = static_cast<size_t>(frames) * fmt->nBlockAlign;
        if (data && BufferContainsNonSilence(fmt, data, bytes)) {
          non_silent_frames += frames;
        } else {
          silent_frames += frames;
        }
      }
    }

    hr = capture->ReleaseBuffer(frames);
    if (FAILED(hr)) {
      out.fail_reason = "release_buffer_failed";
      out.hr = hr;
      log.Logf("virtio-snd: capture ReleaseBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
      client->Stop();
      return out;
    }

    if (total_frames >= min_frames) break;
  }

  hr = client->Stop();
  if (FAILED(hr)) {
    out.fail_reason = "stop_failed";
    out.hr = hr;
    log.Logf("virtio-snd: capture IAudioClient::Stop failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }
  client->Reset();

  if (total_frames == 0) {
    out.fail_reason = "capture_timeout";
    out.hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
    log.LogLine("virtio-snd: capture timed out waiting for frames");
    return out;
  }

  out.captured_frames = total_frames;
  out.captured_non_silence = non_silent_frames > 0;
  out.captured_silence_only = non_silent_frames == 0;

  if (require_non_silence && !out.captured_non_silence) {
    log.LogLine("virtio-snd: capture returned only silence; failing (--require-non-silence)");
    out.ok = false;
    out.hr = E_FAIL;
    out.fail_reason = "captured_silence";
    return out;
  }

  if (out.captured_silence_only) {
    log.LogLine("virtio-snd: capture returned only silence (PASS by default; use --require-non-silence to fail)");
  }

  out.ok = true;
  out.hr = S_OK;
  out.fail_reason.clear();
  log.Logf(
      "virtio-snd: capture smoke ok (frames=%llu min_frames=%llu silent_frames=%llu non_silent_frames=%llu "
      "flags=0x%08lx)",
      total_frames, min_frames, silent_frames, non_silent_frames, static_cast<unsigned long>(captured_flags));
  return out;
}

static TestResult VirtioSndDuplexTest(Logger& log, const std::vector<std::wstring>& match_names, bool allow_transitional) {
  TestResult out;

  ScopedCoInitialize com(COINIT_MULTITHREADED);
  if (FAILED(com.hr())) {
    out.fail_reason = "com_init_failed";
    out.hr = com.hr();
    log.Logf("virtio-snd: duplex CoInitializeEx failed hr=0x%08lx", static_cast<unsigned long>(out.hr));
    return out;
  }

  ComPtr<IMMDeviceEnumerator> enumerator;
  HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                                __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(enumerator.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "create_device_enumerator_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex CoCreateInstance(MMDeviceEnumerator) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  struct SelectedEndpoint {
    ComPtr<IMMDevice> dev;
    std::wstring friendly;
    std::wstring id;
    std::wstring instance_id;
    int score = -1;
  };

  auto select_endpoint = [&](EDataFlow flow, DWORD wait_ms) -> std::optional<SelectedEndpoint> {
    const char* flow_name = (flow == eRender) ? "render" : (flow == eCapture) ? "capture" : "unknown";
    const DWORD deadline_ms = GetTickCount() + wait_ms;
    int attempt = 0;

    for (;;) {
      attempt++;

      ComPtr<IMMDeviceCollection> collection;
      const DWORD state_mask =
          DEVICE_STATE_ACTIVE | DEVICE_STATE_DISABLED | DEVICE_STATE_NOTPRESENT | DEVICE_STATE_UNPLUGGED;
      HRESULT hr_enum = enumerator->EnumAudioEndpoints(flow, state_mask, collection.Put());
      if (FAILED(hr_enum)) {
        log.Logf("virtio-snd: duplex EnumAudioEndpoints(%s) failed hr=0x%08lx attempt=%d", flow_name,
                 static_cast<unsigned long>(hr_enum), attempt);
        if (wait_ms != 0 && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
          Sleep(1000);
          continue;
        }
        break;
      }

      UINT count = 0;
      hr_enum = collection->GetCount(&count);
      if (FAILED(hr_enum)) {
        log.Logf("virtio-snd: duplex IMMDeviceCollection::GetCount(%s) failed hr=0x%08lx", flow_name,
                 static_cast<unsigned long>(hr_enum));
        if (wait_ms != 0 && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
          Sleep(1000);
          continue;
        }
        break;
      }

      log.Logf("virtio-snd: duplex %s endpoints count=%u attempt=%d", flow_name, count, attempt);

      SelectedEndpoint best{};
      best.score = -1;

      for (UINT i = 0; i < count; i++) {
        ComPtr<IMMDevice> dev;
        HRESULT hr_item = collection->Item(i, dev.Put());
        if (FAILED(hr_item)) continue;

        DWORD state = 0;
        hr_item = dev->GetState(&state);
        if (FAILED(hr_item)) state = 0;

        LPWSTR dev_id_raw = nullptr;
        std::wstring dev_id;
        hr_item = dev->GetId(&dev_id_raw);
        if (SUCCEEDED(hr_item) && dev_id_raw) {
          dev_id = dev_id_raw;
          CoTaskMemFree(dev_id_raw);
        }

        ComPtr<IPropertyStore> props;
        hr_item = dev->OpenPropertyStore(STGM_READ, props.Put());

        std::wstring friendly;
        std::wstring instance_id;
        if (SUCCEEDED(hr_item)) {
          friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
          if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
          instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
        }

        const auto hwids = GetHardwareIdsForInstanceId(instance_id);
        VirtioSndPciIdInfo hwid_info{};
        const bool hwid_allowed = IsAllowedVirtioSndPciHardwareId(hwids, allow_transitional, &hwid_info);
        const auto inst_info = GetVirtioSndPciIdInfoFromString(instance_id);
        const bool inst_allowed = IsAllowedVirtioSndPciId(inst_info, allow_transitional);

        log.Logf("virtio-snd: duplex %s endpoint idx=%u state=%s name=%s id=%s instance_id=%s",
                 flow_name, static_cast<unsigned>(i), MmDeviceStateToString(state),
                 WideToUtf8(friendly).c_str(), WideToUtf8(dev_id).c_str(), WideToUtf8(instance_id).c_str());
        log.Logf(
            "virtio-snd: duplex %s endpoint idx=%u virtio_snd_match inst(modern=%d rev01=%d transitional=%d "
            "allowed=%d) hw(modern=%d rev01=%d transitional=%d allowed=%d)",
            flow_name, static_cast<unsigned>(i), inst_info.modern ? 1 : 0, inst_info.modern_rev01 ? 1 : 0,
            inst_info.transitional ? 1 : 0, inst_allowed ? 1 : 0, hwid_info.modern ? 1 : 0,
            hwid_info.modern_rev01 ? 1 : 0, hwid_info.transitional ? 1 : 0, hwid_allowed ? 1 : 0);

        if (state != DEVICE_STATE_ACTIVE) continue;

        int score = 0;
        if (ContainsInsensitive(friendly, L"virtio")) score += 100;
        if (ContainsInsensitive(friendly, L"aero")) score += 50;
        for (const auto& m : match_names) {
          if (!m.empty() && ContainsInsensitive(friendly, m)) score += 200;
        }
        if (hwid_info.modern) score += 1000;
        if (hwid_info.modern_rev01) score += 50;
        if (allow_transitional && hwid_info.transitional) score += 900;
        if (inst_info.modern) score += 800;
        if (inst_info.modern_rev01) score += 50;
        if (allow_transitional && inst_info.transitional) score += 700;

        if (score <= 0) continue;
        if (!LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names, allow_transitional)) continue;

        if (score > best.score) {
          best.score = score;
          best.dev = std::move(dev);
          best.friendly = friendly;
          best.id = dev_id;
          best.instance_id = instance_id;
        }
      }

      if (best.dev) return best;
      if (wait_ms == 0 || static_cast<int32_t>(GetTickCount() - deadline_ms) >= 0) break;
      Sleep(1000);
    }

    log.Logf("virtio-snd: duplex no matching ACTIVE %s endpoint found; checking default endpoint", flow_name);
    SelectedEndpoint def_best{};
    hr = enumerator->GetDefaultAudioEndpoint(flow, eConsole, def_best.dev.Put());
    if (FAILED(hr) || !def_best.dev) {
      log.Logf("virtio-snd: duplex no default %s endpoint available", flow_name);
      return std::nullopt;
    }

    ComPtr<IPropertyStore> props;
    hr = def_best.dev->OpenPropertyStore(STGM_READ, props.Put());
    std::wstring friendly;
    std::wstring instance_id;
    if (SUCCEEDED(hr)) {
      friendly = GetPropertyString(props.Get(), PKEY_Device_FriendlyName);
      if (friendly.empty()) friendly = GetPropertyString(props.Get(), PKEY_Device_DeviceDesc);
      instance_id = GetPropertyString(props.Get(), PKEY_Device_InstanceId);
    }
    const auto hwids = GetHardwareIdsForInstanceId(instance_id);
    if (!LooksLikeVirtioSndEndpoint(friendly, instance_id, hwids, match_names, allow_transitional)) {
      log.Logf("virtio-snd: duplex default %s endpoint does not look like virtio-snd (name=%s instance_id=%s)",
               flow_name, WideToUtf8(friendly).c_str(), WideToUtf8(instance_id).c_str());
      return std::nullopt;
    }

    def_best.friendly = friendly;
    def_best.instance_id = instance_id;
    def_best.score = 0;
    LPWSTR dev_id_raw = nullptr;
    hr = def_best.dev->GetId(&dev_id_raw);
    if (SUCCEEDED(hr) && dev_id_raw) {
      def_best.id = dev_id_raw;
      CoTaskMemFree(dev_id_raw);
    }
    return def_best;
  };

  const DWORD kEndpointWaitMs = 20000;

  auto render_ep = select_endpoint(eRender, kEndpointWaitMs);
  if (!render_ep.has_value()) {
    out.fail_reason = "no_matching_endpoint";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    log.LogLine("virtio-snd: duplex missing render endpoint");
    return out;
  }

  auto capture_ep = select_endpoint(eCapture, kEndpointWaitMs);
  if (!capture_ep.has_value()) {
    out.fail_reason = "no_matching_endpoint";
    out.hr = HRESULT_FROM_WIN32(ERROR_NOT_FOUND);
    log.LogLine("virtio-snd: duplex missing capture endpoint");
    return out;
  }

  out.endpoint_found = true;
  log.Logf("virtio-snd: duplex selected render endpoint name=%s id=%s score=%d",
           WideToUtf8(render_ep->friendly).c_str(), WideToUtf8(render_ep->id).c_str(), render_ep->score);
  log.Logf("virtio-snd: duplex selected capture endpoint name=%s id=%s score=%d",
           WideToUtf8(capture_ep->friendly).c_str(), WideToUtf8(capture_ep->id).c_str(), capture_ep->score);

  ComPtr<IAudioClient> render_client;
  hr = render_ep->dev->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                                reinterpret_cast<void**>(render_client.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "activate_render_audio_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex render IMMDevice::Activate(IAudioClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  ComPtr<IAudioClient> capture_client;
  hr = capture_ep->dev->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                                 reinterpret_cast<void**>(capture_client.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "activate_capture_audio_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex capture IMMDevice::Activate(IAudioClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  constexpr REFERENCE_TIME kBufferDuration100ms = 1000000; // 100ms in 100ns units

  // Render: 48kHz / 16-bit / stereo PCM (contract v1).
  std::vector<BYTE> render_fmt_bytes;
  render_fmt_bytes.resize(sizeof(WAVEFORMATEX));
  auto* render_desired = reinterpret_cast<WAVEFORMATEX*>(render_fmt_bytes.data());
  *render_desired = {};
  render_desired->wFormatTag = WAVE_FORMAT_PCM;
  render_desired->nChannels = 2;
  render_desired->nSamplesPerSec = 48000;
  render_desired->wBitsPerSample = 16;
  render_desired->nBlockAlign = static_cast<WORD>((render_desired->nChannels * render_desired->wBitsPerSample) / 8);
  render_desired->nAvgBytesPerSec = render_desired->nSamplesPerSec * render_desired->nBlockAlign;
  render_desired->cbSize = 0;

  hr = render_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, render_desired, nullptr);
  if (FAILED(hr)) {
    log.Logf(
        "virtio-snd: duplex render Initialize(shared desired 48kHz S16 stereo) failed hr=0x%08lx; trying WAVE_FORMAT_EXTENSIBLE",
        static_cast<unsigned long>(hr));

    render_fmt_bytes.resize(sizeof(WAVEFORMATEXTENSIBLE));
    auto* ext = reinterpret_cast<WAVEFORMATEXTENSIBLE*>(render_fmt_bytes.data());
    *ext = {};
    ext->Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    ext->Format.nChannels = 2;
    ext->Format.nSamplesPerSec = 48000;
    ext->Format.wBitsPerSample = 16;
    ext->Format.nBlockAlign = static_cast<WORD>((ext->Format.nChannels * ext->Format.wBitsPerSample) / 8);
    ext->Format.nAvgBytesPerSec = ext->Format.nSamplesPerSec * ext->Format.nBlockAlign;
    ext->Format.cbSize = static_cast<WORD>(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
    ext->Samples.wValidBitsPerSample = 16;
    ext->dwChannelMask = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT;
    ext->SubFormat = kWaveSubFormatPcm;
    render_desired = &ext->Format;

    hr = render_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, render_desired, nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_render_shared_failed";
      out.hr = hr;
      log.Logf("virtio-snd: duplex render Initialize(shared desired extensible) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
  }

  // Capture: 48kHz / 16-bit / mono PCM (contract v1).
  std::vector<BYTE> capture_fmt_bytes;
  capture_fmt_bytes.resize(sizeof(WAVEFORMATEX));
  auto* capture_desired = reinterpret_cast<WAVEFORMATEX*>(capture_fmt_bytes.data());
  *capture_desired = {};
  capture_desired->wFormatTag = WAVE_FORMAT_PCM;
  capture_desired->nChannels = 1;
  capture_desired->nSamplesPerSec = 48000;
  capture_desired->wBitsPerSample = 16;
  capture_desired->nBlockAlign =
      static_cast<WORD>((capture_desired->nChannels * capture_desired->wBitsPerSample) / 8);
  capture_desired->nAvgBytesPerSec = capture_desired->nSamplesPerSec * capture_desired->nBlockAlign;
  capture_desired->cbSize = 0;

  hr = capture_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, capture_desired, nullptr);
  if (FAILED(hr)) {
    log.Logf(
        "virtio-snd: duplex capture Initialize(shared desired 48kHz S16 mono) failed hr=0x%08lx; trying WAVE_FORMAT_EXTENSIBLE",
        static_cast<unsigned long>(hr));

    capture_fmt_bytes.resize(sizeof(WAVEFORMATEXTENSIBLE));
    auto* ext = reinterpret_cast<WAVEFORMATEXTENSIBLE*>(capture_fmt_bytes.data());
    *ext = {};
    ext->Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    ext->Format.nChannels = 1;
    ext->Format.nSamplesPerSec = 48000;
    ext->Format.wBitsPerSample = 16;
    ext->Format.nBlockAlign = static_cast<WORD>((ext->Format.nChannels * ext->Format.wBitsPerSample) / 8);
    ext->Format.nAvgBytesPerSec = ext->Format.nSamplesPerSec * ext->Format.nBlockAlign;
    ext->Format.cbSize = static_cast<WORD>(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
    ext->Samples.wValidBitsPerSample = 16;
    ext->dwChannelMask = SPEAKER_FRONT_CENTER;
    ext->SubFormat = kWaveSubFormatPcm;
    capture_desired = &ext->Format;

    hr = capture_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, capture_desired, nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_capture_shared_failed";
      out.hr = hr;
      log.Logf("virtio-snd: duplex capture Initialize(shared desired extensible) failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
      return out;
    }
  }

  const auto* render_fmt = reinterpret_cast<const WAVEFORMATEX*>(render_fmt_bytes.data());
  const auto* capture_fmt = reinterpret_cast<const WAVEFORMATEX*>(capture_fmt_bytes.data());
  log.Logf("virtio-snd: duplex render stream format=%s", WaveFormatToString(render_fmt).c_str());
  log.Logf("virtio-snd: duplex capture stream format=%s", WaveFormatToString(capture_fmt).c_str());

  UINT32 render_buffer_frames = 0;
  hr = render_client->GetBufferSize(&render_buffer_frames);
  if (FAILED(hr) || render_buffer_frames == 0) {
    out.fail_reason = "get_render_buffer_size_failed";
    out.hr = FAILED(hr) ? hr : E_FAIL;
    log.Logf("virtio-snd: duplex render GetBufferSize failed hr=0x%08lx buffer_frames=%u",
             static_cast<unsigned long>(out.hr), render_buffer_frames);
    return out;
  }

  UINT32 capture_buffer_frames = 0;
  hr = capture_client->GetBufferSize(&capture_buffer_frames);
  if (FAILED(hr) || capture_buffer_frames == 0) {
    out.fail_reason = "get_capture_buffer_size_failed";
    out.hr = FAILED(hr) ? hr : E_FAIL;
    log.Logf("virtio-snd: duplex capture GetBufferSize failed hr=0x%08lx buffer_frames=%u",
             static_cast<unsigned long>(out.hr), capture_buffer_frames);
    return out;
  }

  ComPtr<IAudioRenderClient> render;
  hr = render_client->GetService(__uuidof(IAudioRenderClient), reinterpret_cast<void**>(render.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "get_render_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex render GetService(IAudioRenderClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  ComPtr<IAudioCaptureClient> capture;
  hr = capture_client->GetService(__uuidof(IAudioCaptureClient), reinterpret_cast<void**>(capture.Put()));
  if (FAILED(hr)) {
    out.fail_reason = "get_capture_client_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex capture GetService(IAudioCaptureClient) failed hr=0x%08lx",
             static_cast<unsigned long>(hr));
    return out;
  }

  // Prefill the render buffer with tone so we immediately have audio queued when both streams start.
  double phase = 0.0;
  if (render_buffer_frames > 0) {
    BYTE* data = nullptr;
    hr = render->GetBuffer(render_buffer_frames, &data);
    if (FAILED(hr)) {
      out.fail_reason = "render_get_buffer_prefill_failed";
      out.hr = hr;
      log.Logf("virtio-snd: duplex render GetBuffer(prefill) failed hr=0x%08lx", static_cast<unsigned long>(hr));
      return out;
    }
    if (!FillToneInterleaved(data, render_buffer_frames, render_fmt, 440.0, &phase)) {
      render->ReleaseBuffer(render_buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT);
      out.fail_reason = "unsupported_stream_format";
      out.hr = E_FAIL;
      log.Logf("virtio-snd: duplex unsupported render stream format for tone generation: %s",
               WaveFormatToString(render_fmt).c_str());
      return out;
    }
    hr = render->ReleaseBuffer(render_buffer_frames, 0);
    if (FAILED(hr)) {
      out.fail_reason = "render_release_buffer_prefill_failed";
      out.hr = hr;
      log.Logf("virtio-snd: duplex render ReleaseBuffer(prefill) failed hr=0x%08lx", static_cast<unsigned long>(hr));
      return out;
    }
  }

  bool render_started = false;
  bool capture_started = false;

  hr = capture_client->Start();
  if (FAILED(hr)) {
    out.fail_reason = "capture_start_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex capture Start failed hr=0x%08lx", static_cast<unsigned long>(hr));
    return out;
  }
  capture_started = true;

  hr = render_client->Start();
  if (FAILED(hr)) {
    out.fail_reason = "render_start_failed";
    out.hr = hr;
    log.Logf("virtio-snd: duplex render Start failed hr=0x%08lx", static_cast<unsigned long>(hr));
    capture_client->Stop();
    capture_client->Reset();
    return out;
  }
  render_started = true;

  UINT64 total_capture_frames = 0;
  bool any_non_silence = false;

  const DWORD run_deadline = GetTickCount() + 3000; // keep short; this runs at every boot in CI images.
  while (static_cast<int32_t>(GetTickCount() - run_deadline) < 0) {
    bool did_work = false;

    // Render: keep the buffer fed with tone.
    UINT32 padding = 0;
    hr = render_client->GetCurrentPadding(&padding);
    if (FAILED(hr)) {
      out.fail_reason = "render_get_current_padding_failed";
      out.hr = hr;
      log.Logf("virtio-snd: duplex render GetCurrentPadding failed hr=0x%08lx", static_cast<unsigned long>(hr));
      break;
    }

    const UINT32 available = (padding < render_buffer_frames) ? (render_buffer_frames - padding) : 0;
    if (available > 0) {
      const UINT32 to_write = std::min<UINT32>(available, std::max<UINT32>(1, render_buffer_frames / 4));
      BYTE* data = nullptr;
      hr = render->GetBuffer(to_write, &data);
      if (FAILED(hr)) {
        out.fail_reason = "render_get_buffer_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex render GetBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
        break;
      }
      if (!FillToneInterleaved(data, to_write, render_fmt, 440.0, &phase)) {
        render->ReleaseBuffer(to_write, AUDCLNT_BUFFERFLAGS_SILENT);
        out.fail_reason = "unsupported_stream_format";
        out.hr = E_FAIL;
        log.Logf("virtio-snd: duplex unsupported render stream format for tone generation: %s",
                 WaveFormatToString(render_fmt).c_str());
        break;
      }
      hr = render->ReleaseBuffer(to_write, 0);
      if (FAILED(hr)) {
        out.fail_reason = "render_release_buffer_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex render ReleaseBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
        break;
      }
      did_work = true;
    }

    // Capture: drain all available packets.
    for (;;) {
      UINT32 packet_frames = 0;
      hr = capture->GetNextPacketSize(&packet_frames);
      if (FAILED(hr)) {
        out.fail_reason = "capture_get_next_packet_size_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex capture GetNextPacketSize failed hr=0x%08lx", static_cast<unsigned long>(hr));
        break;
      }
      if (packet_frames == 0) break;

      BYTE* data = nullptr;
      UINT32 frames = 0;
      DWORD flags = 0;
      hr = capture->GetBuffer(&data, &frames, &flags, nullptr, nullptr);
      if (FAILED(hr)) {
        out.fail_reason = "capture_get_buffer_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex capture GetBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
        break;
      }

      if (frames > 0) {
        total_capture_frames += frames;
        if (!(flags & AUDCLNT_BUFFERFLAGS_SILENT) && capture_fmt->nBlockAlign != 0) {
          const size_t bytes = static_cast<size_t>(frames) * capture_fmt->nBlockAlign;
          if (data && BufferContainsNonSilence(capture_fmt, data, bytes)) any_non_silence = true;
        }
      }

      hr = capture->ReleaseBuffer(frames);
      if (FAILED(hr)) {
        out.fail_reason = "capture_release_buffer_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex capture ReleaseBuffer failed hr=0x%08lx", static_cast<unsigned long>(hr));
        break;
      }

      did_work = true;
    }

    if (!out.fail_reason.empty()) break;

    if (!did_work) Sleep(5);
  }

  if (capture_started) {
    const HRESULT stop_hr = capture_client->Stop();
    if (FAILED(stop_hr) && SUCCEEDED(out.hr)) {
      out.fail_reason = "capture_stop_failed";
      out.hr = stop_hr;
      log.Logf("virtio-snd: duplex capture Stop failed hr=0x%08lx", static_cast<unsigned long>(stop_hr));
    }
    capture_client->Reset();
  }

  if (render_started) {
    const HRESULT stop_hr = render_client->Stop();
    if (FAILED(stop_hr) && SUCCEEDED(out.hr)) {
      out.fail_reason = "render_stop_failed";
      out.hr = stop_hr;
      log.Logf("virtio-snd: duplex render Stop failed hr=0x%08lx", static_cast<unsigned long>(stop_hr));
    }
    render_client->Reset();
  }

  if (!out.fail_reason.empty()) {
    if (out.hr == S_OK) out.hr = E_FAIL;
    return out;
  }

  if (total_capture_frames == 0) {
    out.fail_reason = "capture_no_frames";
    out.hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
    log.LogLine("virtio-snd: duplex capture returned 0 frames");
    return out;
  }

  out.ok = true;
  out.hr = S_OK;
  out.fail_reason.clear();
  out.captured_frames = total_capture_frames;
  out.captured_non_silence = any_non_silence;
  out.captured_silence_only = !any_non_silence;
  log.Logf("virtio-snd: duplex ok (capture_frames=%llu non_silence=%d)", total_capture_frames,
           any_non_silence ? 1 : 0);
  return out;
}

static void PrintUsage() {
  printf(
      "aero-virtio-selftest.exe [options]\n"
      "\n"
      "Options:\n"
      "  --blk-root <path>         Directory to use for virtio-blk file I/O test\n"
      "  --http-url <url>          HTTP URL for TCP connectivity test (also expects <url>-large)\n"
      "  --dns-host <hostname>     Hostname for DNS resolution test\n"
      "  --log-file <path>         Log file path (default C:\\\\aero-virtio-selftest.log)\n"
      "  --disable-snd             Skip virtio-snd test (emit SKIP)\n"
      "  --disable-snd-capture     Skip virtio-snd capture test (emit SKIP)\n"
      "  --require-snd             Fail if virtio-snd is missing (default: SKIP)\n"
      "  --test-snd                Alias for --require-snd\n"
      "  --require-snd-capture     Fail if virtio-snd capture is missing (default: SKIP)\n"
      "  --test-snd-capture        Run virtio-snd capture smoke test if available (default: auto when virtio-snd is present)\n"
      "  --test-snd-buffer-limits  Run virtio-snd large WASAPI buffer/period stress test (optional)\n"
      "  --require-non-silence     Fail capture smoke test if only silence is captured\n"
      "  --allow-virtio-snd-transitional  Also accept legacy PCI\\VEN_1AF4&DEV_1018\n"
      "  --net-timeout-sec <sec>   Wait time for DHCP/link\n"
      "  --io-size-mib <mib>       virtio-blk test file size\n"
      "  --io-chunk-kib <kib>      virtio-blk chunk size\n"
      "  --help                    Show this help\n");
}

static bool EnvVarTruthy(const wchar_t* name) {
  if (!name || !*name) return false;
  wchar_t buf[64]{};
  const DWORD n = GetEnvironmentVariableW(name, buf, static_cast<DWORD>(sizeof(buf) / sizeof(buf[0])));
  if (n == 0 || n >= (sizeof(buf) / sizeof(buf[0]))) return false;
  std::wstring v(buf, buf + n);
  v = ToLower(std::move(v));
  return v == L"1" || v == L"true" || v == L"yes" || v == L"on";
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
    } else if (arg == L"--disable-snd-capture") {
      opt.disable_snd_capture = true;
    } else if (arg == L"--require-snd" || arg == L"--test-snd") {
      opt.require_snd = true;
    } else if (arg == L"--require-snd-capture") {
      opt.require_snd_capture = true;
    } else if (arg == L"--test-snd-capture") {
      opt.test_snd_capture = true;
    } else if (arg == L"--test-snd-buffer-limits") {
      opt.test_snd_buffer_limits = true;
    } else if (arg == L"--require-non-silence") {
      opt.require_non_silence = true;
    } else if (arg == L"--allow-virtio-snd-transitional") {
      opt.allow_virtio_snd_transitional = true;
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

  if (!opt.disable_snd && !opt.disable_snd_capture && !opt.test_snd_capture &&
      EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE")) {
    opt.test_snd_capture = true;
  }

  if (opt.disable_snd &&
      (opt.require_snd || opt.require_snd_capture || opt.test_snd_capture || opt.test_snd_buffer_limits ||
       opt.require_non_silence)) {
    fprintf(stderr,
            "--disable-snd cannot be combined with --test-snd/--require-snd, --require-snd-capture, "
            "--test-snd-capture, --test-snd-buffer-limits, or --require-non-silence\n");
    PrintUsage();
    return 2;
  }
  if (opt.disable_snd_capture && (opt.require_snd_capture || opt.test_snd_capture || opt.require_non_silence)) {
    fprintf(stderr,
            "--disable-snd-capture cannot be combined with --require-snd-capture, --test-snd-capture, or "
            "--require-non-silence\n");
    PrintUsage();
    return 2;
  }
  Logger log(opt.log_file);

  log.LogLine("AERO_VIRTIO_SELFTEST|START|version=1");
  log.Logf("AERO_VIRTIO_SELFTEST|CONFIG|http_url=%s|http_url_large=%s|dns_host=%s|blk_root=%s",
           WideToUtf8(opt.http_url).c_str(),
           WideToUtf8(UrlAppendSuffix(opt.http_url, L"-large")).c_str(),
           WideToUtf8(opt.dns_host).c_str(), WideToUtf8(opt.blk_root).c_str());

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

  // virtio-input end-to-end event delivery:
  //
  // This test reads HID input reports directly from the virtio-input HID minidriver and matches
  // deterministic host-injected input events (keyboard + mouse). The host harness enables injection
  // via QMP `input-send-event` and can optionally enforce this marker.
  //
  // The marker is emitted unconditionally, but does NOT affect the overall selftest RESULT marker by
  // default (so existing harness runs that do not perform QMP injection remain unchanged).
  const auto input_events = VirtioInputEventsTest(log);
  if (input_events.ok) {
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|kbd_reports=%d|mouse_reports=%d|kbd_a_down=%d|kbd_a_up=%d|mouse_move=%d|mouse_left_down=%d|mouse_left_up=%d",
        input_events.keyboard_reports, input_events.mouse_reports, input_events.saw_key_a_down ? 1 : 0,
        input_events.saw_key_a_up ? 1 : 0, input_events.saw_mouse_move ? 1 : 0,
        input_events.saw_mouse_left_down ? 1 : 0, input_events.saw_mouse_left_up ? 1 : 0);
  } else {
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|reason=%s|err=%lu|kbd_reports=%d|mouse_reports=%d|kbd_a_down=%d|kbd_a_up=%d|mouse_move=%d|mouse_left_down=%d|mouse_left_up=%d",
        input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
        static_cast<unsigned long>(input_events.win32_error), input_events.keyboard_reports,
        input_events.mouse_reports, input_events.saw_key_a_down ? 1 : 0, input_events.saw_key_a_up ? 1 : 0,
        input_events.saw_mouse_move ? 1 : 0, input_events.saw_mouse_left_down ? 1 : 0,
        input_events.saw_mouse_left_up ? 1 : 0);
  }
  // virtio-snd:
  //
  // The host harness can optionally attach a virtio-snd PCI function. When the device is present,
  // exercise the playback + capture + duplex paths automatically so audio regressions are caught
  // even if the image runs the selftest without extra flags. Use `--disable-snd` to skip all
  // virtio-snd testing, or `--test-snd/--require-snd` to fail if the device is missing.
  auto snd_pci = opt.disable_snd ? std::vector<VirtioSndPciDevice>{}
                                : DetectVirtioSndPciDevices(log, opt.allow_virtio_snd_transitional);
  if (!opt.disable_snd && snd_pci.empty()) {
    // The scheduled task that runs the selftest can sometimes start very early during boot,
    // before PnP fully enumerates the virtio-snd PCI function. Give the bus a short grace
    // period so we don't emit spurious SKIP markers (which causes the host harness to fail
    // when virtio-snd is attached).
    const DWORD deadline_ms = GetTickCount() + 10000;
    int attempt = 0;
    while (snd_pci.empty() && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
      attempt++;
      Sleep(250);
      snd_pci = DetectVirtioSndPciDevices(log, opt.allow_virtio_snd_transitional, false);
    }
    if (!snd_pci.empty()) {
      log.Logf("virtio-snd: pci device detected after wait (attempt=%d)", attempt);
    }
  }

  const bool want_snd_playback = opt.require_snd || !snd_pci.empty();
  const bool capture_smoke_test = opt.test_snd_capture || opt.require_non_silence || want_snd_playback;
  const bool want_snd_capture =
      !opt.disable_snd_capture &&
      (opt.require_snd_capture || opt.test_snd_capture || opt.require_non_silence || want_snd_playback);

  if (opt.disable_snd) {
    log.LogLine("virtio-snd: disabled by --disable-snd");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
  } else if (!want_snd_playback && !opt.require_snd_capture && !opt.test_snd_capture &&
             !opt.require_non_silence) {
    log.LogLine("virtio-snd: skipped (enable with --test-snd)");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
    log.LogLine(opt.disable_snd_capture ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled"
                                        : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set");
    log.LogLine(opt.disable_snd_capture ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled"
                                        : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
  } else {
    if (!want_snd_playback) {
      log.LogLine("virtio-snd: skipped (enable with --test-snd)");
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
    }

    if (snd_pci.empty()) {
      if (opt.allow_virtio_snd_transitional) {
        log.LogLine(
            "virtio-snd: PCI\\VEN_1AF4&DEV_1059 (or legacy PCI\\VEN_1AF4&DEV_1018) device not detected");
      } else {
        log.LogLine("virtio-snd: PCI\\VEN_1AF4&DEV_1059 device not detected (contract v1 modern-only)");
      }

      if (want_snd_playback) {
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL");
        all_ok = false;
      }

      if (opt.disable_snd_capture) {
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
      } else if (opt.require_snd_capture) {
        log.LogLine("virtio-snd: --require-snd-capture set; failing (device missing)");
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|device_missing");
        all_ok = false;
      } else {
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|device_missing");
      }

      log.LogLine(opt.disable_snd_capture ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled"
                  : !capture_smoke_test   ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set"
                                          : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|device_missing");
    } else {
      auto binding = CheckVirtioSndPciBinding(log, snd_pci);

      // The scheduled task that runs the selftest can sometimes start very early during boot,
      // before the device is fully bound to its driver service. When virtio-snd is present and
      // expected, give PnP a short grace period to bind the driver so we don't report spurious
      // failures (or capture endpoint missing) due to transient "driver_not_bound" states.
      if (!binding.ok && !binding.any_wrong_service) {
        const DWORD deadline_ms = GetTickCount() + 10000;
        int attempt = 0;
        while (!binding.ok && !binding.any_wrong_service &&
               static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
          attempt++;
          Sleep(250);
          snd_pci = DetectVirtioSndPciDevices(log, opt.allow_virtio_snd_transitional, false);
          binding = SummarizeVirtioSndPciBinding(snd_pci);
          if (binding.ok) {
            log.Logf("virtio-snd: pci binding became healthy after wait (attempt=%d)", attempt);
            break;
          }
        }

        if (!binding.ok) {
          // Re-run the binding check with logging enabled to capture actionable diagnostics.
          binding = CheckVirtioSndPciBinding(log, snd_pci);
        }
      }

      if (!binding.ok) {
        const char* reason = binding.any_wrong_service   ? "wrong_service"
                             : binding.any_missing_service ? "driver_not_bound"
                             : binding.any_problem         ? "device_error"
                                                           : "driver_not_bound";

        if (want_snd_playback) {
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|%s", reason);
          all_ok = false;
        }

        if (opt.disable_snd_capture) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
        } else if (opt.require_snd_capture) {
          log.LogLine("virtio-snd: --require-snd-capture set; failing (driver binding not healthy)");
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|%s", reason);
          all_ok = false;
        } else {
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|%s", reason);
        }

        if (opt.disable_snd_capture) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
        } else if (!capture_smoke_test) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
        } else {
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|%s", reason);
        }
      } else if (!VirtioSndHasTopologyInterface(log, snd_pci)) {
        log.LogLine("virtio-snd: no KSCATEGORY_TOPOLOGY interface found for detected virtio-snd device");

        if (want_snd_playback) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL");
          all_ok = false;
        }

        if (opt.disable_snd_capture) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
        } else if (opt.require_snd_capture) {
          log.LogLine("virtio-snd: --require-snd-capture set; failing (topology interface missing)");
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|topology_interface_missing");
          all_ok = false;
        } else {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|topology_interface_missing");
        }

        if (opt.disable_snd_capture) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
        } else if (!capture_smoke_test) {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
        } else {
          log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|topology_interface_missing");
        }
      } else {
        std::vector<std::wstring> match_names;
        for (const auto& d : snd_pci) {
          if (!d.description.empty()) match_names.push_back(d.description);
        }

        bool force_null_backend = false;
        for (const auto& dev : snd_pci) {
          if (dev.force_null_backend.has_value() && *dev.force_null_backend != 0) {
            force_null_backend = true;
            break;
          }
        }

        if (force_null_backend) {
          log.LogLine(
              "virtio-snd: ForceNullBackend=1 set; virtio transport disabled (host wav capture will be silent)");

          if (want_snd_playback) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend");
            all_ok = false;
          } else {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP");
          }

          if (opt.disable_snd_capture) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
          } else if (want_snd_capture) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|force_null_backend");
            all_ok = false;

            if (want_snd_playback && capture_smoke_test) {
              log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|force_null_backend");
              all_ok = false;
            } else {
              log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
            }
          } else {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set");
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
          }
        } else {
          // The scheduled task that runs the selftest can start before the Windows audio services are
          // fully initialized. Wait briefly for AudioSrv/AudioEndpointBuilder so endpoint enumeration
          // doesn't fail spuriously (which would make host-side virtio-snd wav verification flaky).
          if (want_snd_playback || want_snd_capture) {
            WaitForWindowsAudioServices(log, 30000);
          }

          if (opt.test_snd_buffer_limits && want_snd_playback) {
            const auto stress =
                VirtioSndBufferLimitsTest(log, match_names, opt.allow_virtio_snd_transitional);
            if (stress.ok) {
              log.Logf(
                  "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS|mode=%s|init_hr=0x%08lx|expected_failure=%d|buffer_bytes=%llu",
                  stress.mode.empty() ? "-" : stress.mode.c_str(),
                  static_cast<unsigned long>(stress.init_hr), stress.expected_failure ? 1 : 0,
                  static_cast<unsigned long long>(stress.buffer_bytes));
            } else {
              log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|reason=%s|hr=0x%08lx",
                       stress.fail_reason.empty() ? "unknown" : stress.fail_reason.c_str(),
                       static_cast<unsigned long>(stress.hr));
              all_ok = false;
            }
          }

          if (want_snd_playback) {
            bool snd_ok = false;
            const auto snd = VirtioSndTest(log, match_names, opt.allow_virtio_snd_transitional);
            if (snd.ok) {
              snd_ok = true;
            } else {
              log.Logf("virtio-snd: WASAPI failed reason=%s hr=0x%08lx",
                       snd.fail_reason.empty() ? "unknown" : snd.fail_reason.c_str(),
                       static_cast<unsigned long>(snd.hr));
              log.LogLine("virtio-snd: trying waveOut fallback");
              snd_ok = WaveOutToneTest(log, match_names, opt.allow_virtio_snd_transitional);
            }

            log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|%s", snd_ok ? "PASS" : "FAIL");
            all_ok = all_ok && snd_ok;
          }

          if (opt.disable_snd_capture) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
          } else if (want_snd_capture) {
            const DWORD capture_wait_ms =
                (opt.require_snd_capture || capture_smoke_test || want_snd_playback) ? 20000 : 0;
            bool capture_ok = false;
            const char* capture_method = "wasapi";
            bool capture_silence_only = false;
            bool capture_non_silence = false;
            UINT64 capture_frames = 0;

            auto capture = VirtioSndCaptureTest(log, match_names, capture_smoke_test, capture_wait_ms,
                                                opt.allow_virtio_snd_transitional, opt.require_non_silence);
            if (capture.ok) {
              capture_ok = true;
              capture_silence_only = capture.captured_silence_only;
              capture_non_silence = capture.captured_non_silence;
              capture_frames = capture.captured_frames;
            } else if (capture_smoke_test) {
              log.Logf("virtio-snd: capture WASAPI failed reason=%s hr=0x%08lx",
                       capture.fail_reason.empty() ? "unknown" : capture.fail_reason.c_str(),
                       static_cast<unsigned long>(capture.hr));
              log.LogLine("virtio-snd: trying waveIn fallback");

              const auto wavein = WaveInCaptureTest(log, match_names, opt.allow_virtio_snd_transitional,
                                                    opt.require_non_silence);
              if (wavein.ok) {
                capture_ok = true;
                capture_method = "waveIn";
                capture_silence_only = wavein.captured_silence_only;
                capture_non_silence = wavein.captured_non_silence;
                capture_frames = wavein.captured_frames;
              }
            }

            if (capture_ok) {
              if (capture_smoke_test) {
                log.Logf(
                    "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|method=%s|frames=%llu|non_silence=%d|silence_only=%d",
                    capture_method, capture_frames, capture_non_silence ? 1 : 0,
                    capture_silence_only ? 1 : 0);
              } else {
                log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS|endpoint_present");
              }
            } else if (capture.fail_reason == "no_matching_endpoint") {
              if (opt.require_snd_capture) {
                log.LogLine("virtio-snd: --require-snd-capture set; failing");
                log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|endpoint_missing");
                all_ok = false;
              } else {
                log.LogLine(
                    "virtio-snd: no capture endpoint; skipping (use --require-snd-capture to require)");
                log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing");
              }
            } else if (capture.fail_reason == "captured_silence") {
              log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|silence");
              all_ok = false;
            } else {
              log.Logf("virtio-snd: capture failed reason=%s hr=0x%08lx",
                       capture.fail_reason.empty() ? "unknown" : capture.fail_reason.c_str(),
                       static_cast<unsigned long>(capture.hr));
              if (opt.require_snd_capture || capture_smoke_test) {
                log.LogLine(
                    capture.endpoint_found
                        ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|stream_init_failed"
                        : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|error");
                all_ok = false;
              } else {
                log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|error");
              }
            }
          } else {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set");
          }

          if (opt.disable_snd_capture) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
          } else if (!(want_snd_playback && capture_smoke_test)) {
            log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
          } else {
            const auto duplex = VirtioSndDuplexTest(log, match_names, opt.allow_virtio_snd_transitional);
            if (duplex.ok) {
              log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS|frames=%llu|non_silence=%d",
                       duplex.captured_frames, duplex.captured_non_silence ? 1 : 0);
            } else if (duplex.fail_reason == "no_matching_endpoint") {
              log.LogLine(
                  "virtio-snd: duplex endpoint missing; skipping (use --require-snd-capture to require)");
              log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|endpoint_missing");
            } else {
              log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|reason=%s|hr=0x%08lx",
                       duplex.fail_reason.empty() ? "unknown" : duplex.fail_reason.c_str(),
                       static_cast<unsigned long>(duplex.hr));
              all_ok = false;
            }
          }
        }
      }
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
