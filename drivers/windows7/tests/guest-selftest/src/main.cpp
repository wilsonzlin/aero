#include <windows.h>

#include <setupapi.h>

#include <devguid.h>
#include <initguid.h>
#include <iphlpapi.h>
#include <ntddstor.h>
#include <winhttp.h>
#include <ws2tcpip.h>

#include <algorithm>
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

    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
    if (!IsVirtioHardwareId(hwids)) continue;

    HANDLE h = CreateFileW(detail->DevicePath, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                           OPEN_EXISTING, 0, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
      log.Logf("virtio-blk: CreateFile(%s) failed: %lu", WideToUtf8(detail->DevicePath).c_str(),
               GetLastError());
      continue;
    }

    STORAGE_DEVICE_NUMBER devnum{};
    DWORD bytes = 0;
    if (DeviceIoControl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0, &devnum, sizeof(devnum),
                        &bytes, nullptr)) {
      disks.insert(devnum.DeviceNumber);
      log.Logf("virtio-blk: detected disk device_number=%lu path=%s", devnum.DeviceNumber,
               WideToUtf8(detail->DevicePath).c_str());
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

  if (disks.count(*base_disk) == 0) {
    log.Logf("virtio-blk: test dir is on disk %lu (not a detected virtio disk)", *base_disk);
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
      WinHttpOpen(L"AeroVirtioSelftest/1.0", WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
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

static void PrintUsage() {
  printf(
      "aero-virtio-selftest.exe [options]\n"
      "\n"
      "Options:\n"
      "  --blk-root <path>         Directory to use for virtio-blk file I/O test\n"
      "  --http-url <url>          HTTP URL for TCP connectivity test\n"
      "  --dns-host <hostname>     Hostname for DNS resolution test\n"
      "  --log-file <path>         Log file path (default C:\\\\aero-virtio-selftest.log)\n"
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
  log.Logf("AERO_VIRTIO_SELFTEST|CONFIG|http_url=%s|dns_host=%s|blk_root=%s",
           WideToUtf8(opt.http_url).c_str(), WideToUtf8(opt.dns_host).c_str(),
           WideToUtf8(opt.blk_root).c_str());

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

  log.Logf("AERO_VIRTIO_SELFTEST|RESULT|%s", all_ok ? "PASS" : "FAIL");
  return all_ok ? 0 : 1;
}
