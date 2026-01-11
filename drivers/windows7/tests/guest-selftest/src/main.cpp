// SPDX-License-Identifier: MIT OR Apache-2.0
//
// aero-virtio-selftest: Windows 7 user-mode functional tests for Aero virtio drivers.
// Primary targets: virtio-blk + virtio-net. Output is written to stdout, a log file, and COM1.

#include <windows.h>

#include <cfgmgr32.h>
#include <mmsystem.h>
#include <mmddk.h>
#include <setupapi.h>

#include <devguid.h>
#include <initguid.h>
#include <iphlpapi.h>
#include <ntddstor.h>
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
  // If set, the virtio-snd test will FAIL (instead of SKIP) when no virtio-snd device is present.
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

static std::optional<std::wstring> GetEnvVarW(const wchar_t* name) {
  if (!name || !*name) return std::nullopt;

  // First call with nSize=0 to get required size (including NUL).
  SetLastError(ERROR_SUCCESS);
  const DWORD required = GetEnvironmentVariableW(name, nullptr, 0);
  if (required == 0) {
    if (GetLastError() == ERROR_ENVVAR_NOT_FOUND) return std::nullopt;
    // Present but empty.
    return std::wstring();
  }

  std::wstring buf(required, L'\0');
  SetLastError(ERROR_SUCCESS);
  const DWORD written = GetEnvironmentVariableW(name, buf.data(), required);
  if (written == 0) {
    if (GetLastError() == ERROR_ENVVAR_NOT_FOUND) return std::nullopt;
    // Present but empty.
    return std::wstring();
  }
  buf.resize(written);
  return buf;
}

static bool EnvVarTruthy(const wchar_t* name) {
  const auto v = GetEnvVarW(name);
  if (!v.has_value()) return false;

  std::wstring s = ToLower(*v);
  s.erase(std::remove_if(s.begin(), s.end(), [](wchar_t c) { return iswspace(c) != 0; }), s.end());
  if (s.empty()) return true;
  if (s == L"0" || s == L"false" || s == L"no" || s == L"off") return false;
  return true;
}

static bool ContainsInsensitive(const std::wstring& haystack, const std::wstring& needle) {
  return ToLower(haystack).find(ToLower(needle)) != std::wstring::npos;
}

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

enum class TestVerdict {
  kPass,
  kFail,
  kSkip,
};

static const char* VerdictString(TestVerdict v) {
  switch (v) {
    case TestVerdict::kPass:
      return "PASS";
    case TestVerdict::kFail:
      return "FAIL";
    case TestVerdict::kSkip:
      return "SKIP";
  }
  return "FAIL";
}

static std::string WaveOutErrorText(MMRESULT mm) {
  wchar_t buf[256]{};
  if (waveOutGetErrorTextW(mm, buf, static_cast<UINT>(sizeof(buf) / sizeof(buf[0]))) ==
      MMSYSERR_NOERROR) {
    return WideToUtf8(std::wstring(buf));
  }
  char fallback[64];
  snprintf(fallback, sizeof(fallback), "MMRESULT=%u", static_cast<unsigned>(mm));
  return fallback;
}

static std::optional<std::wstring> CmGetDeviceIdStringW(DEVINST inst) {
  ULONG len = 0;
  CONFIGRET cr = CM_Get_Device_ID_SizeW(&len, inst, 0);
  if (cr != CR_SUCCESS) return std::nullopt;
  std::wstring buf(static_cast<size_t>(len) + 1, L'\0');
  cr = CM_Get_Device_IDW(inst, buf.data(), static_cast<ULONG>(buf.size()), 0);
  if (cr != CR_SUCCESS) return std::nullopt;
  buf.resize(wcslen(buf.c_str()));
  return buf;
}

static std::optional<bool> WaveOutDevnodeTreeContainsVirtioSnd(Logger& log, HWAVEOUT hwo,
                                                               bool log_failure) {
  // Some WDM audio drivers support mapping waveOut devices back to a PnP devnode. This allows us to
  // verify that we're actually opening the virtio-snd device when using WAVE_MAPPER fallback.
  DEVINST inst = 0;
  const MMRESULT mm =
      waveOutMessage(hwo, DRV_QUERYDEVNODE, reinterpret_cast<DWORD_PTR>(&inst), 0);
  if (mm != MMSYSERR_NOERROR) {
    if (log_failure) {
      log.Logf("virtio-snd: waveOutMessage(DRV_QUERYDEVNODE) failed: %s",
               WaveOutErrorText(mm).c_str());
    }
    return std::nullopt;
  }

  DEVINST cur = inst;
  for (int depth = 0; depth < 8; depth++) {
    const auto id = CmGetDeviceIdStringW(cur);
    if (id.has_value()) {
      if (ContainsInsensitive(*id, L"VEN_1AF4&DEV_1059")) return true;
    }

    DEVINST parent = 0;
    const CONFIGRET cr = CM_Get_Parent(&parent, cur, 0);
    if (cr != CR_SUCCESS) break;
    cur = parent;
  }

  return false;
}

enum class DevnodeVerifyMode {
  kNone,
  // Attempt verification if supported. If the devnode query is unavailable, proceed but log.
  kBestEffort,
  // Verification must succeed and the devnode must match virtio-snd.
  kRequired,
};

static bool IsVirtioSndHardwareId(const std::vector<std::wstring>& hwids) {
  for (const auto& id : hwids) {
    if (ContainsInsensitive(id, L"PCI\\VEN_1AF4&DEV_1059")) return true;
    if (ContainsInsensitive(id, L"VEN_1AF4&DEV_1059")) return true;
  }
  return false;
}

struct VirtioSndDevice {
  std::wstring instance_id;   // e.g. "PCI\\VEN_1AF4&DEV_1059&..."
  std::wstring friendly_name; // optional
};

static std::vector<VirtioSndDevice> DetectVirtioSndDevices(Logger& log) {
  std::vector<VirtioSndDevice> out;

  HDEVINFO devinfo = SetupDiGetClassDevsW(&GUID_DEVCLASS_MEDIA, nullptr, nullptr, DIGCF_PRESENT);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-snd: SetupDiGetClassDevs(GUID_DEVCLASS_MEDIA) failed: %lu", GetLastError());
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
    if (!IsVirtioSndHardwareId(hwids)) continue;

    VirtioSndDevice snd{};

    wchar_t instance_buf[512]{};
    if (SetupDiGetDeviceInstanceIdW(devinfo, &dev, instance_buf,
                                    static_cast<DWORD>(sizeof(instance_buf) / sizeof(instance_buf[0])),
                                    nullptr)) {
      snd.instance_id = instance_buf;
    }

    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      snd.friendly_name = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      snd.friendly_name = *desc;
    }

    log.Logf("virtio-snd: detected device instance_id=%s name=%s", WideToUtf8(snd.instance_id).c_str(),
             WideToUtf8(snd.friendly_name).c_str());
    out.push_back(std::move(snd));
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return out;
}

static std::optional<UINT> FindWaveOutDeviceIdByNameHints(Logger& log,
                                                          const std::vector<std::wstring>& hints,
                                                          std::wstring* pname_out,
                                                          bool log_available_devices) {
  const UINT count = waveOutGetNumDevs();
  if (count == 0) return std::nullopt;

  for (UINT i = 0; i < count; i++) {
    WAVEOUTCAPSW caps{};
    const MMRESULT mm = waveOutGetDevCapsW(i, &caps, sizeof(caps));
    if (mm != MMSYSERR_NOERROR) {
      log.Logf("virtio-snd: waveOutGetDevCaps id=%u failed: %s", i, WaveOutErrorText(mm).c_str());
      continue;
    }

    const std::wstring pname = caps.szPname;
    for (const auto& hint : hints) {
      if (hint.empty()) continue;
      if (ContainsInsensitive(pname, hint)) {
        if (pname_out) *pname_out = pname;
        return i;
      }
    }
  }

  if (log_available_devices) {
    // No match - log the available devices to help diagnose name-matching issues.
    for (UINT i = 0; i < count; i++) {
      WAVEOUTCAPSW caps{};
      const MMRESULT mm = waveOutGetDevCapsW(i, &caps, sizeof(caps));
      if (mm != MMSYSERR_NOERROR) continue;
      log.Logf("virtio-snd: available waveOut device id=%u name=%s", i,
               WideToUtf8(caps.szPname).c_str());
    }
  }

  return std::nullopt;
}

static std::optional<UINT> FindWaveOutDeviceIdByVirtioDevnode(Logger& log, const WAVEFORMATEX& fmt,
                                                              std::wstring* pname_out) {
  const UINT count = waveOutGetNumDevs();
  if (count == 0) return std::nullopt;

  for (UINT i = 0; i < count; i++) {
    HWAVEOUT hwo = nullptr;
    const MMRESULT open_rc = waveOutOpen(&hwo, i, &fmt, 0, 0, CALLBACK_NULL);
    if (open_rc != MMSYSERR_NOERROR || !hwo) continue;

    const auto is_virtio = WaveOutDevnodeTreeContainsVirtioSnd(log, hwo, false);
    (void)waveOutClose(hwo);

    if (is_virtio.has_value() && *is_virtio) {
      if (pname_out) {
        WAVEOUTCAPSW caps{};
        if (waveOutGetDevCapsW(i, &caps, sizeof(caps)) == MMSYSERR_NOERROR) {
          *pname_out = caps.szPname;
        } else {
          pname_out->clear();
        }
      }
      return i;
    }
  }

  return std::nullopt;
}

static bool WaveOutPlaybackSmokeTest(Logger& log, UINT device_id, const std::wstring& device_name_hint,
                                     MMRESULT* open_rc_out, DevnodeVerifyMode verify_mode) {
  if (open_rc_out) *open_rc_out = MMSYSERR_NOERROR;

  HANDLE done_event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
  if (!done_event) {
    log.Logf("virtio-snd: CreateEvent failed: %lu", GetLastError());
    return false;
  }

  WAVEFORMATEX fmt{};
  fmt.wFormatTag = WAVE_FORMAT_PCM;
  fmt.nChannels = 2;
  fmt.nSamplesPerSec = 48000;
  fmt.wBitsPerSample = 16;
  fmt.nBlockAlign = static_cast<WORD>((fmt.nChannels * fmt.wBitsPerSample) / 8);
  fmt.nAvgBytesPerSec = fmt.nBlockAlign * fmt.nSamplesPerSec;

  HWAVEOUT hwo = nullptr;
  log.Logf("virtio-snd: waveOutOpen device_id=%u hint=%s", device_id,
           WideToUtf8(device_name_hint).c_str());
  const MMRESULT open_rc = waveOutOpen(&hwo, device_id, &fmt,
                                      reinterpret_cast<DWORD_PTR>(done_event), 0, CALLBACK_EVENT);
  if (open_rc_out) *open_rc_out = open_rc;
  if (open_rc != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutOpen failed: %s", WaveOutErrorText(open_rc).c_str());
    CloseHandle(done_event);
    return false;
  }

  UINT actual_id = device_id;
  if (waveOutGetID(hwo, &actual_id) == MMSYSERR_NOERROR) {
    WAVEOUTCAPSW caps{};
    if (waveOutGetDevCapsW(actual_id, &caps, sizeof(caps)) == MMSYSERR_NOERROR) {
      log.Logf("virtio-snd: opened waveOut device id=%u name=%s", actual_id,
               WideToUtf8(caps.szPname).c_str());
    } else {
      log.Logf("virtio-snd: opened waveOut device id=%u", actual_id);
    }
  }

  if (verify_mode != DevnodeVerifyMode::kNone) {
    const auto is_virtio =
        WaveOutDevnodeTreeContainsVirtioSnd(log, hwo, verify_mode == DevnodeVerifyMode::kRequired);
    if (!is_virtio.has_value()) {
      if (verify_mode == DevnodeVerifyMode::kRequired) {
        log.LogLine("virtio-snd: unable to verify waveOut devnode maps to virtio-snd");
        waveOutClose(hwo);
        CloseHandle(done_event);
        return false;
      }
      log.LogLine("virtio-snd: devnode verification unavailable; continuing without verification");
    }
    if (is_virtio.has_value() && !*is_virtio) {
      log.LogLine("virtio-snd: waveOut device devnode does not appear to be virtio-snd");
      waveOutClose(hwo);
      CloseHandle(done_event);
      return false;
    }
  }

  const uint32_t duration_ms = 500;
  const uint32_t frames = (fmt.nSamplesPerSec * duration_ms) / 1000;
  std::vector<int16_t> samples(frames * fmt.nChannels);

  const double pi = 3.14159265358979323846;
  const double freq_hz = 440.0;
  const double amp = 8000.0;

  for (uint32_t i = 0; i < frames; i++) {
    const double t = static_cast<double>(i) / static_cast<double>(fmt.nSamplesPerSec);
    const int16_t s = static_cast<int16_t>(std::sin(2.0 * pi * freq_hz * t) * amp);
    samples[i * 2 + 0] = s;
    samples[i * 2 + 1] = s;
  }

  WAVEHDR hdr{};
  hdr.lpData = reinterpret_cast<LPSTR>(samples.data());
  hdr.dwBufferLength = static_cast<DWORD>(samples.size() * sizeof(int16_t));

  MMRESULT mm = waveOutPrepareHeader(hwo, &hdr, sizeof(hdr));
  if (mm != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutPrepareHeader failed: %s", WaveOutErrorText(mm).c_str());
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  mm = waveOutWrite(hwo, &hdr, sizeof(hdr));
  if (mm != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutWrite failed: %s", WaveOutErrorText(mm).c_str());
    waveOutReset(hwo);
    (void)waveOutUnprepareHeader(hwo, &hdr, sizeof(hdr));
    waveOutClose(hwo);
    CloseHandle(done_event);
    return false;
  }

  const DWORD timeout_ms = 10000;
  const DWORD deadline = GetTickCount() + timeout_ms;
  PerfTimer t;

  while ((hdr.dwFlags & WHDR_DONE) == 0 && static_cast<int32_t>(GetTickCount() - deadline) < 0) {
    WaitForSingleObject(done_event, 200);
  }

  bool ok = (hdr.dwFlags & WHDR_DONE) != 0;
  if (!ok) {
    log.Logf("virtio-snd: playback timeout after %lu ms flags=0x%08lx", timeout_ms, hdr.dwFlags);
    waveOutReset(hwo);
  } else {
    log.Logf("virtio-snd: playback done sec=%.3f", t.SecondsSinceStart());
  }

  for (int attempt = 0; attempt < 10; attempt++) {
    mm = waveOutUnprepareHeader(hwo, &hdr, sizeof(hdr));
    if (mm == MMSYSERR_NOERROR) break;
    if (mm != WAVERR_STILLPLAYING) break;
    Sleep(50);
  }
  if (mm != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutUnprepareHeader failed: %s", WaveOutErrorText(mm).c_str());
    ok = false;
  }

  mm = waveOutClose(hwo);
  if (mm != MMSYSERR_NOERROR) {
    log.Logf("virtio-snd: waveOutClose failed: %s", WaveOutErrorText(mm).c_str());
    ok = false;
  }

  CloseHandle(done_event);
  return ok;
}

static TestVerdict VirtioSndTest(Logger& log, const Options& opt) {
  log.LogLine("virtio-snd: starting WaveOut smoke test");

  const auto devs = DetectVirtioSndDevices(log);
  if (devs.empty()) {
    log.LogLine("virtio-snd: no PCI\\VEN_1AF4&DEV_1059 device detected");
    return opt.require_snd ? TestVerdict::kFail : TestVerdict::kSkip;
  }

  std::vector<std::wstring> hints;
  for (const auto& d : devs) {
    if (!d.friendly_name.empty()) hints.push_back(d.friendly_name);
  }
  hints.push_back(L"virtio");

  WAVEFORMATEX fmt{};
  fmt.wFormatTag = WAVE_FORMAT_PCM;
  fmt.nChannels = 2;
  fmt.nSamplesPerSec = 48000;
  fmt.wBitsPerSample = 16;
  fmt.nBlockAlign = static_cast<WORD>((fmt.nChannels * fmt.wBitsPerSample) / 8);
  fmt.nAvgBytesPerSec = fmt.nBlockAlign * fmt.nSamplesPerSec;

  // At boot, the audio stack can be slow to come up (especially when running as SYSTEM).
  // Retry transient waveOutOpen failures for a short, bounded time to avoid false negatives.
  const DWORD open_timeout_ms = 15000;
  const DWORD deadline = GetTickCount() + open_timeout_ms;
  bool logged_name_match_devices = false;
  UINT last_open_id = UINT_MAX;
  DevnodeVerifyMode last_verify_mode = DevnodeVerifyMode::kNone;
  std::wstring last_name;

  while (static_cast<int32_t>(GetTickCount() - deadline) < 0) {
    std::wstring chosen_name;
    std::optional<UINT> chosen_id;
    DevnodeVerifyMode verify_mode = DevnodeVerifyMode::kRequired;

    // Prefer a deterministic mapping to the virtio-snd PCI device via DRV_QUERYDEVNODE.
    chosen_id = FindWaveOutDeviceIdByVirtioDevnode(log, fmt, &chosen_name);
    if (chosen_id.has_value()) {
      verify_mode = DevnodeVerifyMode::kNone;
      if (chosen_name.empty()) chosen_name = L"virtio-snd";
    } else {
      // Fall back to matching by name, which is less strict but works even when devnode query is unsupported.
      chosen_id = FindWaveOutDeviceIdByNameHints(log, hints, &chosen_name, !logged_name_match_devices);
      if (chosen_id.has_value()) {
        verify_mode = DevnodeVerifyMode::kBestEffort;
      } else {
        const UINT count = waveOutGetNumDevs();
        if (!logged_name_match_devices && count != 0) {
          logged_name_match_devices = true;
        }

        if (count == 1) {
          // If there is only one waveOut device, opening device 0 is effectively deterministic and
          // avoids depending on product-name matching.
          WAVEOUTCAPSW caps{};
          if (waveOutGetDevCapsW(0, &caps, sizeof(caps)) == MMSYSERR_NOERROR) {
            chosen_name = caps.szPname;
          } else {
            chosen_name = L"waveOut[0]";
          }
          chosen_id = 0;
          verify_mode = DevnodeVerifyMode::kBestEffort;
        } else {
          chosen_name = L"WAVE_MAPPER";
          verify_mode = DevnodeVerifyMode::kRequired;
        }
      }
    }

    const UINT open_id = chosen_id.value_or(WAVE_MAPPER);
    if (open_id != last_open_id || verify_mode != last_verify_mode || chosen_name != last_name) {
      log.Logf("virtio-snd: selected waveOut device_id=%u name=%s verify_mode=%d", open_id,
               WideToUtf8(chosen_name).c_str(), static_cast<int>(verify_mode));
      last_open_id = open_id;
      last_verify_mode = verify_mode;
      last_name = chosen_name;
    }

    MMRESULT open_rc = MMSYSERR_NOERROR;
    if (WaveOutPlaybackSmokeTest(log, open_id, chosen_name, &open_rc, verify_mode)) {
      return TestVerdict::kPass;
    }
    Sleep(1000);
  }

  return TestVerdict::kFail;
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
      "  --require-snd             Fail if virtio-snd is missing (default: SKIP)\n"
      "                           (or set env AERO_VIRTIO_SELFTEST_REQUIRE_SND=1)\n"
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
  opt.require_snd = EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_REQUIRE_SND");

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

  Logger log(opt.log_file);

  log.LogLine("AERO_VIRTIO_SELFTEST|START|version=1");
  log.Logf("AERO_VIRTIO_SELFTEST|CONFIG|http_url=%s|dns_host=%s|blk_root=%s|require_snd=%d",
           WideToUtf8(opt.http_url).c_str(), WideToUtf8(opt.dns_host).c_str(),
           WideToUtf8(opt.blk_root).c_str(), opt.require_snd ? 1 : 0);

  bool all_ok = true;

  const bool blk_ok = VirtioBlkTest(log, opt);
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk|%s", blk_ok ? "PASS" : "FAIL");
  all_ok = all_ok && blk_ok;

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

  const TestVerdict snd_v = VirtioSndTest(log, opt);
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|%s", VerdictString(snd_v));
  if (snd_v == TestVerdict::kFail) all_ok = false;

  log.Logf("AERO_VIRTIO_SELFTEST|RESULT|%s", all_ok ? "PASS" : "FAIL");
  return all_ok ? 0 : 1;
}
