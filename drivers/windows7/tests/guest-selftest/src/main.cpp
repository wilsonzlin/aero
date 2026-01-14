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

#include <aero_virtio_net_diag.h>

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

#include <aero_virtio_snd_diag.h>
#include <aero_virtio_blk_ioctl.h>

#ifndef STATUS_NOT_SUPPORTED
// Some Windows 7 SDK environments don't expose NTSTATUS codes in the default
// include set. Define the minimal constant we need for miniport IOCTL probing.
#define STATUS_NOT_SUPPORTED static_cast<ULONG>(0xC00000BBu)
#endif

#ifndef RegDisposition_OpenExisting
// Some SDK environments omit the REGDISPOSITION enum constants used by
// ConfigManager registry helpers. `RegDisposition_OpenExisting` is the standard
// "open only" value.
#define RegDisposition_OpenExisting static_cast<REGDISPOSITION>(1)
#endif

namespace {

struct Options {
  std::wstring http_url = L"http://10.0.2.2:18080/aero-virtio-selftest";
  // UDP echo server port for the virtio-net UDP smoke test (guest reaches host loopback as 10.0.2.2 via slirp).
  USHORT udp_port = 18081;
  // Prefer a hostname that (on many QEMU versions) resolves without relying on external internet.
  // If unavailable, the selftest will fall back to "example.com".
  std::wstring dns_host = L"host.lan";
  std::wstring log_file = L"C:\\aero-virtio-selftest.log";
  // Optional: override where the virtio-blk file I/O test writes its temporary file.
  // This must be a directory on a virtio-backed volume (e.g. "D:\\aero-test\\").
  // If empty, the selftest will attempt to auto-detect a mounted virtio volume.
  std::wstring blk_root;
  // Optional: run an end-to-end virtio-blk runtime resize test.
  // This requires host-side intervention during the run (QMP block resize).
  bool test_blk_resize = false;
  // If set, run a stability test that forces a virtio-blk miniport reset via the private
  // `AEROVBLK_IOCTL_FORCE_RESET` IOCTL and then verifies post-reset I/O still works.
  bool test_blk_reset = false;
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

  // If set, run an end-to-end virtio-input event delivery test that reads actual HID input reports.
  // This is intended to be paired with host-side QMP `input-send-event` injection.
  bool test_input_events = false;
  // If set, run an end-to-end virtio-input tablet (absolute pointer) event delivery test.
  // This is intended to be paired with host-side QMP `input-send-event` injection of `abs` events.
  bool test_input_tablet_events = false;
  // Optional: expand the virtio-input end-to-end report test to cover additional HID usages:
  // - keyboard modifiers + function keys
  // - mouse side buttons
  // - mouse wheel
  // These are intentionally separate so the default `--test-input-events` path remains stable.
  bool test_input_events_modifiers = false;
  bool test_input_events_buttons = false;
  bool test_input_events_wheel = false;

  // If set, run an end-to-end virtio-input media key test that reads Consumer Control HID reports.
  // This is intended to be paired with host-side QMP `input-send-event` injection of multimedia keys.
  bool test_input_media_keys = false;
  // If set, run a virtio-input statusq LED smoke test (HID keyboard output reports -> virtio statusq).
  // This is optional by default and is intended to be gated by the host harness when validating
  // virtio-input statusq consumption/completions.
  bool test_input_led = false;

  // If set, require the virtio-input driver to be using MSI-X (message-signaled interrupts).
  // Without this flag the selftest still emits an informational virtio-input-msix marker.
  bool require_input_msix = false;

  // If set, run a virtio-net link flap regression test coordinated by the host harness via QMP `set_link`.
  bool test_net_link_flap = false;

  DWORD net_timeout_sec = 120;
  DWORD io_file_size_mib = 32;
  DWORD io_chunk_kib = 1024;

  // Soft assertion: if set, fail the virtio-blk test when the miniport reports it is
  // still operating in INTx mode (expected MSI/MSI-X).
  bool expect_blk_msi = false;
  // If set, fail the overall run unless the virtio-net driver reports MSI-X mode.
  bool require_net_msix = false;
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

static bool LessInsensitive(const std::wstring& a, const std::wstring& b) {
  const size_t n = std::min(a.size(), b.size());
  for (size_t i = 0; i < n; i++) {
    const wchar_t ca = static_cast<wchar_t>(towlower(a[i]));
    const wchar_t cb = static_cast<wchar_t>(towlower(b[i]));
    if (ca < cb) return true;
    if (ca > cb) return false;
  }
  return a.size() < b.size();
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

// Userspace mirror of `drivers/windows7/virtio-input/src/log.h` diagnostics IOCTLs / structs.
//
// The guest selftest intentionally duplicates these definitions so it can be built with a plain Win7-compatible
// SDK toolchain (without WDK-only headers). Keep them in sync with the driver ABI:
//   scripts/ci/check-win7-virtio-input-diagnostics-abi-sync.py
//
// These are used by the selftest to observe interrupt configuration (INTx vs MSI-X) and to validate virtio-input
// statusq consumption (keyboard LED output -> statusq counters).
static constexpr DWORD IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO =
    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_READ_ACCESS);

static constexpr DWORD IOCTL_VIOINPUT_QUERY_COUNTERS = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS);
static constexpr DWORD IOCTL_VIOINPUT_RESET_COUNTERS = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS);

static constexpr USHORT VIOINPUT_INTERRUPT_VECTOR_NONE = 0xFFFFu;

enum VIOINPUT_INTERRUPT_MODE : ULONG {
  VioInputInterruptModeUnknown = 0,
  VioInputInterruptModeIntx = 1,
  VioInputInterruptModeMsix = 2,
};

enum VIOINPUT_INTERRUPT_MAPPING : ULONG {
  VioInputInterruptMappingUnknown = 0,
  VioInputInterruptMappingAllOnVector0 = 1,
  VioInputInterruptMappingPerQueue = 2,
};

struct VIOINPUT_INTERRUPT_INFO {
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
};

// Userspace mirror of `drivers/windows7/virtio-input/src/log.h` VIOINPUT_COUNTERS.
// This struct is queried via IOCTL_VIOINPUT_QUERY_COUNTERS and must match the kernel layout.
struct VIOINPUT_COUNTERS {
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
};

static_assert(sizeof(VIOINPUT_COUNTERS) == 176, "VIOINPUT_COUNTERS layout");

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

static uint32_t ReadBe32(const uint8_t* p) {
  return (static_cast<uint32_t>(p[0]) << 24) | (static_cast<uint32_t>(p[1]) << 16) |
         (static_cast<uint32_t>(p[2]) << 8) | static_cast<uint32_t>(p[3]);
}

static uint16_t ReadBe16(const uint8_t* p) {
  return static_cast<uint16_t>((static_cast<uint16_t>(p[0]) << 8) | static_cast<uint16_t>(p[1]));
}

static std::string HexDump(const uint8_t* p, size_t len) {
  std::string out;
  out.reserve(len * 3);
  for (size_t i = 0; i < len; i++) {
    char b[4];
    snprintf(b, sizeof(b), "%02x", static_cast<unsigned int>(p[i]));
    out.append(b);
    if (i + 1 != len) out.push_back(' ');
  }
  return out;
}

static std::string MacToString(const uint8_t mac[6]) {
  if (!mac) return {};
  char buf[32];
  snprintf(buf, sizeof(buf), "%02x:%02x:%02x:%02x:%02x:%02x",
           static_cast<unsigned>(mac[0]),
           static_cast<unsigned>(mac[1]),
           static_cast<unsigned>(mac[2]),
           static_cast<unsigned>(mac[3]),
           static_cast<unsigned>(mac[4]),
           static_cast<unsigned>(mac[5]));
  return std::string(buf);
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

static void LogScsiSenseSummary(Logger& log, const char* prefix, const uint8_t* sense, size_t len) {
  if (!sense || len == 0) {
    log.Logf("%s <no sense data>", prefix ? prefix : "scsi_sense");
    return;
  }

  // Parse a minimal SPC sense summary (sense key / ASC / ASCQ) from either:
  // - fixed format (0x70/0x71): key @ [2], ASC/ASCQ @ [12]/[13]
  // - descriptor format (0x72/0x73): key @ [1], ASC/ASCQ @ [2]/[3]
  uint8_t sk = 0;
  uint8_t asc = 0;
  uint8_t ascq = 0;

  const uint8_t resp_code = sense[0] & 0x7F;
  if ((resp_code == 0x70 || resp_code == 0x71) && len >= 14) {
    sk = static_cast<uint8_t>(sense[2] & 0x0F);
    asc = sense[12];
    ascq = sense[13];
  } else if ((resp_code == 0x72 || resp_code == 0x73) && len >= 4) {
    sk = static_cast<uint8_t>(sense[1] & 0x0F);
    asc = sense[2];
    ascq = sense[3];
  } else if (len >= 14) {
    // Heuristic fallback: treat it like fixed format.
    sk = static_cast<uint8_t>(sense[2] & 0x0F);
    asc = sense[12];
    ascq = sense[13];
  }

  log.Logf("%s sense_key=0x%02x asc=0x%02x ascq=0x%02x", prefix ? prefix : "scsi_sense",
           static_cast<unsigned>(sk), static_cast<unsigned>(asc), static_cast<unsigned>(ascq));
}

struct TestResult {
  bool ok = false;
  std::string fail_reason;
  HRESULT hr = S_OK;
  // For virtio-snd tests, record the endpoint mix format (shared-mode) that Windows selected.
  // This surfaces the driver's negotiated format/rate to the host harness.
  std::string mix_format;
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

static constexpr USHORT kVirtioPciMsiNoVector = 0xFFFFu;

static_assert(offsetof(AEROVBLK_QUERY_INFO, NegotiatedFeatures) == 0x00, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, QueueSize) == 0x08, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, NumFree) == 0x0A, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, AvailIdx) == 0x0C, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, UsedIdx) == 0x0E, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, InterruptMode) == 0x10, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, MsixConfigVector) == 0x14, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, MsixQueue0Vector) == 0x16, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, MessageCount) == 0x18, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, Reserved0) == 0x1C, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, AbortSrbCount) == 0x20, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, ResetDeviceSrbCount) == 0x24, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, ResetBusSrbCount) == 0x28, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, PnpSrbCount) == 0x2C, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, IoctlResetCount) == 0x30, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, CapacityChangeEvents) == 0x34, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, ResetDetectedCount) == 0x38, "AEROVBLK_QUERY_INFO layout");
static_assert(offsetof(AEROVBLK_QUERY_INFO, HwResetBusCount) == 0x3C, "AEROVBLK_QUERY_INFO layout");
static_assert(sizeof(AEROVBLK_QUERY_INFO) == 0x40, "AEROVBLK_QUERY_INFO size mismatch");

struct AerovblkQueryInfoResult {
  AEROVBLK_QUERY_INFO info{};
  size_t returned_len = 0; // Bytes of `info` returned by the driver (variable-length contract).
};

static_assert(sizeof(AEROVNET_DIAG_INFO) <= 256, "AEROVNET_DIAG_INFO size");
static_assert(AEROVNET_DIAG_IOCTL_QUERY == CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800u, METHOD_BUFFERED, FILE_READ_ACCESS),
              "AEROVNET_DIAG_IOCTL_QUERY value");

// Userspace mirror of `drivers/windows7/virtio-net/include/aero_virtio_net.h` checksum offload stats IOCTL contract.
// Served by the same control device as the virtio-net diagnostics interface.
static constexpr const wchar_t* kAerovnetOffloadDevicePath = kAerovnetDiagDevicePath;
static constexpr ULONG kAerovnetIoctlQueryOffloadStats =
    CTL_CODE(FILE_DEVICE_NETWORK, 0xA80, METHOD_BUFFERED, FILE_READ_ACCESS);

struct AEROVNET_OFFLOAD_STATS {
  DWORD Version;
  DWORD Size;
  uint8_t Mac[6];
  uint8_t Reserved0[2];
  uint64_t HostFeatures;
  uint64_t GuestFeatures;
  uint64_t TxCsumOffloadTcp4;
  uint64_t TxCsumOffloadTcp6;
  uint64_t TxCsumOffloadUdp4;
  uint64_t TxCsumOffloadUdp6;
  uint64_t RxCsumValidatedTcp4;
  uint64_t RxCsumValidatedTcp6;
  uint64_t RxCsumValidatedUdp4;
  uint64_t RxCsumValidatedUdp6;
  uint64_t TxCsumFallback;
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

static HANDLE TryOpenPhysicalDriveForIoctl(DWORD disk_number, DWORD* out_err) {
  if (out_err) *out_err = ERROR_SUCCESS;
  wchar_t path[64];
  swprintf_s(path, L"\\\\.\\PhysicalDrive%lu", static_cast<unsigned long>(disk_number));

  const DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  const DWORD flags = FILE_ATTRIBUTE_NORMAL;
  const DWORD desired_accesses[] = {GENERIC_READ | GENERIC_WRITE, GENERIC_READ, 0};
  DWORD last_err = ERROR_SUCCESS;
  for (const DWORD access : desired_accesses) {
    HANDLE h = CreateFileW(path, access, share, nullptr, OPEN_EXISTING, flags, nullptr);
    if (h != INVALID_HANDLE_VALUE) {
      if (out_err) *out_err = ERROR_SUCCESS;
      return h;
    }
    last_err = GetLastError();
  }
  if (out_err) *out_err = last_err;
  return INVALID_HANDLE_VALUE;
}

static HANDLE OpenPhysicalDriveForIoctl(Logger& log, DWORD disk_number) {
  DWORD err = ERROR_SUCCESS;
  HANDLE h = TryOpenPhysicalDriveForIoctl(disk_number, &err);
  if (h != INVALID_HANDLE_VALUE) return h;

  log.Logf("virtio-blk: CreateFile(PhysicalDrive%lu) failed err=%lu", static_cast<unsigned long>(disk_number),
           static_cast<unsigned long>(err));
  return INVALID_HANDLE_VALUE;
}

static std::optional<AerovblkQueryInfoResult> QueryAerovblkMiniportInfo(Logger& log, HANDLE hPhysicalDrive) {
  if (hPhysicalDrive == INVALID_HANDLE_VALUE) return std::nullopt;

  std::vector<BYTE> buf(sizeof(SRB_IO_CONTROL) + sizeof(AEROVBLK_QUERY_INFO));
  auto* ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  ctrl->HeaderLength = sizeof(SRB_IO_CONTROL);
  memcpy(ctrl->Signature, AEROVBLK_SRBIO_SIG, sizeof(ctrl->Signature));
  ctrl->Timeout = 10;
  ctrl->ControlCode = AEROVBLK_IOCTL_QUERY;
  ctrl->ReturnCode = 0;
  ctrl->Length = sizeof(AEROVBLK_QUERY_INFO);

  DWORD bytes = 0;
  if (!DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_MINIPORT, buf.data(), static_cast<DWORD>(buf.size()),
                       buf.data(), static_cast<DWORD>(buf.size()), &bytes, nullptr)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT(AEROVBLK_IOCTL_QUERY) failed err=%lu", GetLastError());
    return std::nullopt;
  }
  constexpr size_t kQueryInfoV1Len = offsetof(AEROVBLK_QUERY_INFO, InterruptMode);
  if (bytes < sizeof(SRB_IO_CONTROL) + kQueryInfoV1Len) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned too few bytes=%lu (expected >=%zu)", bytes,
             sizeof(SRB_IO_CONTROL) + kQueryInfoV1Len);
    return std::nullopt;
  }

  ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  if (ctrl->ReturnCode != 0) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned ReturnCode=0x%08lx", ctrl->ReturnCode);
    return std::nullopt;
  }
  const size_t payload_bytes = (bytes > sizeof(SRB_IO_CONTROL)) ? (bytes - sizeof(SRB_IO_CONTROL)) : 0;
  size_t returned_len = std::min<size_t>(payload_bytes, ctrl->Length);
  if (returned_len < kQueryInfoV1Len) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT returned Length=%lu (expected >=%zu)", ctrl->Length, kQueryInfoV1Len);
    return std::nullopt;
  }

  AerovblkQueryInfoResult out{};
  out.returned_len = returned_len;

  // Variable-length contract: copy only the bytes the driver reports as valid.
  memset(&out.info, 0, sizeof(out.info));
  memcpy(&out.info, buf.data() + sizeof(SRB_IO_CONTROL), std::min(returned_len, sizeof(out.info)));

  // Defensive: if the driver returns a short/odd-length payload that cuts through the middle of a field,
  // ensure we don't treat partially-copied bytes as meaningful values. In particular, 0 is a valid MSI-X
  // vector index but is a bad "unknown" default. Use the virtio sentinel (0xFFFF) when the full field
  // was not returned.
  constexpr size_t kMsixCfgEnd = offsetof(AEROVBLK_QUERY_INFO, MsixConfigVector) + sizeof(USHORT);
  constexpr size_t kMsixQ0End = offsetof(AEROVBLK_QUERY_INFO, MsixQueue0Vector) + sizeof(USHORT);
  if (returned_len < kMsixCfgEnd) out.info.MsixConfigVector = kVirtioPciMsiNoVector;
  if (returned_len < kMsixQ0End) out.info.MsixQueue0Vector = kVirtioPciMsiNoVector;
  return out;
}

static bool ForceAerovblkMiniportReset(Logger& log, HANDLE hPhysicalDrive, bool* out_performed) {
  if (out_performed) *out_performed = false;
  if (hPhysicalDrive == INVALID_HANDLE_VALUE) return false;

  std::vector<BYTE> buf(sizeof(SRB_IO_CONTROL));
  auto* ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  ctrl->HeaderLength = sizeof(SRB_IO_CONTROL);
  memcpy(ctrl->Signature, AEROVBLK_SRBIO_SIG, sizeof(ctrl->Signature));
  ctrl->Timeout = 30;
  ctrl->ControlCode = AEROVBLK_IOCTL_FORCE_RESET;
  ctrl->ReturnCode = 0;
  ctrl->Length = 0;

  DWORD bytes = 0;
  if (!DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_MINIPORT, buf.data(), static_cast<DWORD>(buf.size()),
                       buf.data(), static_cast<DWORD>(buf.size()), &bytes, nullptr)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT(AEROVBLK_IOCTL_FORCE_RESET) failed err=%lu", GetLastError());
    return false;
  }
  if (bytes < sizeof(SRB_IO_CONTROL)) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT(force reset) returned too few bytes=%lu", bytes);
    return false;
  }

  ctrl = reinterpret_cast<SRB_IO_CONTROL*>(buf.data());
  if (ctrl->ReturnCode == static_cast<ULONG>(STATUS_NOT_SUPPORTED)) {
    log.LogLine("virtio-blk: miniport force reset SKIP (STATUS_NOT_SUPPORTED)");
    return true;
  }
  if (ctrl->ReturnCode != 0) {
    log.Logf("virtio-blk: IOCTL_SCSI_MINIPORT(force reset) ReturnCode=0x%08lx", ctrl->ReturnCode);
    return false;
  }

  log.LogLine("virtio-blk: miniport force reset ok");
  if (out_performed) *out_performed = true;
  return true;
}

static bool ValidateAerovblkMiniportInfo(Logger& log, const AerovblkQueryInfoResult& res) {
  const auto& info = res.info;
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

  constexpr size_t kCountersEnd = offsetof(AEROVBLK_QUERY_INFO, IoctlResetCount) + sizeof(ULONG);
  const bool have_counters = res.returned_len >= kCountersEnd;
  const unsigned long abort_count = have_counters ? static_cast<unsigned long>(info.AbortSrbCount) : 0;
  const unsigned long reset_dev_count = have_counters ? static_cast<unsigned long>(info.ResetDeviceSrbCount) : 0;
  const unsigned long reset_bus_count = have_counters ? static_cast<unsigned long>(info.ResetBusSrbCount) : 0;
  const unsigned long pnp_count = have_counters ? static_cast<unsigned long>(info.PnpSrbCount) : 0;
  const unsigned long ioctl_reset_count = have_counters ? static_cast<unsigned long>(info.IoctlResetCount) : 0;

  log.Logf("virtio-blk: miniport query PASS queue_size=%u num_free=%u avail_idx=%u used_idx=%u features=%s abort=%lu reset_dev=%lu reset_bus=%lu pnp=%lu ioctl_reset=%lu",
           info.QueueSize, info.NumFree, info.AvailIdx, info.UsedIdx,
           VirtioFeaturesToString(info.NegotiatedFeatures).c_str(),
           abort_count, reset_dev_count, reset_bus_count, pnp_count, ioctl_reset_count);

  // Optional flags diagnostics (variable-length contract).
  constexpr size_t kFlagsEnd = offsetof(AEROVBLK_QUERY_INFO, Reserved0) + sizeof(ULONG);
  if (res.returned_len >= kFlagsEnd) {
    const ULONG flags = info.Reserved0;
    log.Logf("virtio-blk-miniport-flags|INFO|raw=0x%08lx|removed=%d|surprise_removed=%d|reset_in_progress=%d|reset_pending=%d",
             static_cast<unsigned long>(flags),
             (flags & AEROVBLK_QUERY_FLAG_REMOVED) ? 1 : 0,
             (flags & AEROVBLK_QUERY_FLAG_SURPRISE_REMOVED) ? 1 : 0,
             (flags & AEROVBLK_QUERY_FLAG_RESET_IN_PROGRESS) ? 1 : 0,
             (flags & AEROVBLK_QUERY_FLAG_RESET_PENDING) ? 1 : 0);
  } else {
    log.Logf("virtio-blk-miniport-flags|WARN|reason=missing_flags|returned_len=%zu|expected_min=%zu",
             res.returned_len, kFlagsEnd);
  }

  // Optional reset/recovery counters (variable-length contract).
  constexpr size_t kResetRecoveryEnd = offsetof(AEROVBLK_QUERY_INFO, HwResetBusCount) + sizeof(ULONG);
  if (res.returned_len >= kResetRecoveryEnd) {
    log.Logf("virtio-blk-miniport-reset-recovery|INFO|reset_detected=%lu|hw_reset_bus=%lu",
             static_cast<unsigned long>(info.ResetDetectedCount),
             static_cast<unsigned long>(info.HwResetBusCount));
  } else {
    log.Logf("virtio-blk-miniport-reset-recovery|WARN|reason=missing_counters|returned_len=%zu|expected_min=%zu",
             res.returned_len, kResetRecoveryEnd);
  }

  if (!have_counters) {
    log.Logf("virtio-blk: miniport query WARN (counters not reported; returned_len=%zu expected_min=%zu)",
             res.returned_len, kCountersEnd);
  }

  // Optional interrupt mode diagnostics (variable-length contract).
  constexpr size_t kIrqModeEnd = offsetof(AEROVBLK_QUERY_INFO, InterruptMode) + sizeof(ULONG);
  constexpr size_t kMsixCfgEnd = offsetof(AEROVBLK_QUERY_INFO, MsixConfigVector) + sizeof(USHORT);
  constexpr size_t kMsixQ0End = offsetof(AEROVBLK_QUERY_INFO, MsixQueue0Vector) + sizeof(USHORT);
  constexpr size_t kMsgCountEnd = offsetof(AEROVBLK_QUERY_INFO, MessageCount) + sizeof(ULONG);

  if (res.returned_len < kIrqModeEnd) {
    log.Logf("virtio-blk-miniport-irq|WARN|reason=missing_interrupt_mode|returned_len=%zu|expected_min=%zu",
             res.returned_len, kIrqModeEnd);
    return true;
  }

  if (res.returned_len < sizeof(AEROVBLK_QUERY_INFO)) {
    log.Logf("virtio-blk-miniport-irq|WARN|reason=query_truncated|returned_len=%zu|expected=%zu",
             res.returned_len, sizeof(AEROVBLK_QUERY_INFO));
  }

  const char* mode = "unknown";
  if (info.InterruptMode == AEROVBLK_INTERRUPT_MODE_INTX) {
    mode = "intx";
  } else if (info.InterruptMode == AEROVBLK_INTERRUPT_MODE_MSI) {
    mode = "msi";
  }

  const USHORT msix_cfg = (res.returned_len >= kMsixCfgEnd) ? info.MsixConfigVector : kVirtioPciMsiNoVector;
  const USHORT msix_q0 = (res.returned_len >= kMsixQ0End) ? info.MsixQueue0Vector : kVirtioPciMsiNoVector;

  std::string msg_count = "unknown";
  if (res.returned_len >= kMsgCountEnd) {
    msg_count = std::to_string(static_cast<unsigned long>(info.MessageCount));
  } else {
    log.Logf("virtio-blk-miniport-irq|WARN|reason=missing_message_count|returned_len=%zu|expected_min=%zu",
             res.returned_len, kMsgCountEnd);
  }

  // Keep `message_count` for backward compatibility with older log parsers, but prefer the
  // `messages` key used by other virtio IRQ diagnostics.
  log.Logf(
      "virtio-blk-miniport-irq|INFO|mode=%s|messages=%s|message_count=%s|msix_config_vector=0x%04x|msix_queue0_vector=0x%04x",
      mode, msg_count.c_str(), msg_count.c_str(), static_cast<unsigned>(msix_cfg), static_cast<unsigned>(msix_q0));
  return true;
}

static const char* AerovblkIrqModeForMarker(const AEROVBLK_QUERY_INFO& info) {
  if (info.InterruptMode == AEROVBLK_INTERRUPT_MODE_INTX) return "intx";
  if (info.InterruptMode == AEROVBLK_INTERRUPT_MODE_MSI) {
    // `InterruptMode` intentionally conflates MSI + MSI-X. If any virtio MSI-X vectors are assigned,
    // treat this as MSI-X for marker diagnostics.
    if (info.MsixConfigVector != kVirtioPciMsiNoVector || info.MsixQueue0Vector != kVirtioPciMsiNoVector) {
      return "msix";
    }
    return "msi";
  }
  return "unknown";
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

// Some Windows 7 SDK environments are missing newer interrupt flag definitions (e.g. message-signaled interrupts).
// Define the subset we need for resource list parsing so the guest selftest can be built with a plain Win7 SDK.
#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
// In WDK headers this is defined as 0x0004 (alongside CM_RESOURCE_INTERRUPT_LATCHED=0x0001).
// This flag indicates the interrupt resource is message-signaled (MSI/MSI-X).
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004
#endif

struct IrqModeInfo {
  // True when Windows assigned MSI/MSI-X (message signaled) interrupts.
  bool is_msi = false;
  // Number of messages allocated when `is_msi` is true.
  uint32_t messages = 0;
};

struct IrqQueryResult {
  bool ok = false;
  IrqModeInfo info{};
  std::string reason;
  CONFIGRET cr = CR_SUCCESS;
};

struct DevNodeMatch {
  DEVINST devinst = 0;
  std::wstring instance_id;
};

static std::vector<DevNodeMatch> FindPresentDevNodesByHwidSubstrings(const wchar_t* enumerator,
                                                                     const std::vector<std::wstring>& needles) {
  std::vector<DevNodeMatch> out;
  if (needles.empty()) return out;

  const DWORD flags = DIGCF_PRESENT | DIGCF_ALLCLASSES;
  HDEVINFO devinfo = SetupDiGetClassDevsW(nullptr, enumerator, nullptr, flags);
  if (devinfo == INVALID_HANDLE_VALUE) return out;

  for (DWORD idx = 0;; idx++) {
    SP_DEVINFO_DATA dev{};
    dev.cbSize = sizeof(dev);
    if (!SetupDiEnumDeviceInfo(devinfo, idx, &dev)) {
      if (GetLastError() == ERROR_NO_MORE_ITEMS) break;
      continue;
    }

    const auto hwids = GetDevicePropertyMultiSz(devinfo, &dev, SPDRP_HARDWAREID);
    bool match = false;
    for (const auto& id : hwids) {
      for (const auto& needle : needles) {
        if (ContainsInsensitive(id, needle)) {
          match = true;
          break;
        }
      }
      if (match) break;
    }
    if (!match) continue;

    DevNodeMatch m{};
    m.devinst = dev.DevInst;
    if (auto inst = GetDeviceInstanceIdString(devinfo, &dev)) {
      m.instance_id = *inst;
    }
    out.push_back(std::move(m));
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  std::sort(out.begin(), out.end(),
            [](const DevNodeMatch& a, const DevNodeMatch& b) { return a.instance_id < b.instance_id; });
  return out;
}

static uint32_t ChooseMsiMessageCount(uint32_t message_descriptor_count, const std::set<uint32_t>& message_count_values) {
  if (message_descriptor_count == 0) return 0;
  if (message_count_values.empty()) return message_descriptor_count;

  // If the resource descriptor includes a single non-trivial MessageCount (common when the interrupt is represented
  // as a single range), prefer it. Otherwise, fall back to counting descriptors.
  if (message_count_values.size() == 1) {
    const uint32_t v = *message_count_values.begin();
    if (v > message_descriptor_count) return v;
    if (message_descriptor_count == 1 && v > 0) return v;
  }
  return message_descriptor_count;
}

static IrqQueryResult QueryDevInstIrqModeOnce(DEVINST devinst) {
  IrqQueryResult out{};

  if (devinst == 0) {
    out.reason = "invalid_devinst";
    out.cr = CR_FAILURE;
    return out;
  }

  LOG_CONF log_conf = 0;
  CONFIGRET cr = CM_Get_First_Log_Conf(&log_conf, devinst, ALLOC_LOG_CONF);
  if (cr != CR_SUCCESS) {
    out.reason = "cm_get_first_log_conf_failed";
    out.cr = cr;
    return out;
  }

  // Enumerate the translated (allocated) resource descriptors and look for interrupt descriptors.
  RES_DES res_des = 0;
  RESOURCEID res_id = 0;
  cr = CM_Get_First_Res_Des(&res_des, log_conf, ResType_All, &res_id, 0);
  if (cr != CR_SUCCESS) {
    CM_Free_Log_Conf_Handle(log_conf);
    out.reason = "cm_get_first_res_des_failed";
    out.cr = cr;
    return out;
  }

  bool saw_any_interrupt = false;
  bool saw_message_interrupt = false;
  uint32_t message_desc_count = 0;
  std::set<uint32_t> message_count_values;

  // `CM_PARTIAL_RESOURCE_DESCRIPTOR::Type` values. We only need Interrupt here.
  static constexpr uint8_t kCmResourceTypeInterrupt = 2;

  while (true) {
    ULONG data_size = 0;
    cr = CM_Get_Res_Des_Data_Size(&data_size, res_des, 0);
    if (cr != CR_SUCCESS) {
      out.reason = "cm_get_res_des_data_size_failed";
      out.cr = cr;
      break;
    }

    std::vector<uint8_t> data;
    if (data_size > 0) {
      data.resize(data_size);
      cr = CM_Get_Res_Des_Data(res_des, data.data(), data_size, 0);
      if (cr != CR_SUCCESS) {
        out.reason = "cm_get_res_des_data_failed";
        out.cr = cr;
        break;
      }
    }

    // Best-effort parse of CM_PARTIAL_RESOURCE_DESCRIPTOR header (Type/ShareDisposition/Flags).
    if (data_size >= 4) {
      const uint8_t type = data[0];
      uint16_t flags = 0;
      memcpy(&flags, data.data() + 2, sizeof(flags));

      if (type == kCmResourceTypeInterrupt) {
        saw_any_interrupt = true;
        if ((flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0) {
          saw_message_interrupt = true;
          message_desc_count++;

          // When present, MessageCount sits immediately after Level + Vector + Affinity. Depending on packing,
          // the union may be 4 or 8-byte aligned on x64; probe both offsets (best-effort).
          const size_t msg_count_off1 = 12u + sizeof(ULONG_PTR);
          const size_t msg_count_off2 = msg_count_off1 + 4u;
          const size_t msg_count_offs[] = {msg_count_off1, msg_count_off2};
          for (const size_t msg_count_off : msg_count_offs) {
            if (data_size < msg_count_off + sizeof(uint32_t)) continue;
            uint32_t msg_count = 0;
            memcpy(&msg_count, data.data() + msg_count_off, sizeof(msg_count));
            // Ignore obviously corrupt values. Virtio devices should only allocate a small number of vectors.
            if (msg_count == 0 || msg_count > 4096) continue;
            message_count_values.insert(msg_count);
            break;
          }
        }
      }
    }

    RES_DES next = 0;
    RESOURCEID next_id = 0;
    const CONFIGRET next_cr = CM_Get_Next_Res_Des(&next, res_des, ResType_All, &next_id, 0);
    CM_Free_Res_Des_Handle(res_des);
    res_des = 0;

    if (next_cr != CR_SUCCESS) {
      // Expected end-of-list is CR_NO_MORE_RES_DES.
      cr = next_cr;
      if (cr == CR_NO_MORE_RES_DES) {
        break;
      }
      out.reason = "cm_get_next_res_des_failed";
      out.cr = cr;
      break;
    }

    res_des = next;
    res_id = next_id;
  }

  if (res_des) {
    CM_Free_Res_Des_Handle(res_des);
  }
  CM_Free_Log_Conf_Handle(log_conf);

  if (!out.reason.empty()) return out;

  if (!saw_any_interrupt) {
    out.reason = "no_interrupt_resources";
    out.cr = CR_SUCCESS;
    return out;
  }

  out.ok = true;
  out.info.is_msi = saw_message_interrupt;
  if (saw_message_interrupt) {
    out.info.messages = ChooseMsiMessageCount(message_desc_count, message_count_values);
    if (out.info.messages == 0) out.info.messages = 1;
  }
  return out;
}

static IrqQueryResult QueryDevInstIrqModeWithParentFallback(DEVINST devinst) {
  // Some stacks expose virtio functionality as a child devnode (e.g. a HID interface) while the underlying PCI
  // function holds the interrupt resources. Walk up the devnode tree until we find an interrupt resource descriptor.
  DEVINST dn = devinst;
  IrqQueryResult last{};
  for (int depth = 0; depth < 16 && dn != 0; depth++) {
    last = QueryDevInstIrqModeOnce(dn);
    if (last.ok) return last;
    if (last.reason != "no_interrupt_resources" && last.reason != "cm_get_first_log_conf_failed" &&
        last.reason != "cm_get_first_res_des_failed" && last.reason != "cm_get_next_res_des_failed") {
      // For other failures (e.g. invalid handles), don't keep walking indefinitely.
      break;
    }
    DEVINST parent = 0;
    const CONFIGRET cr = CM_Get_Parent(&parent, dn, 0);
    if (cr != CR_SUCCESS) {
      last.reason = "cm_get_parent_failed";
      last.cr = cr;
      break;
    }
    dn = parent;
  }
  return last;
}

static void EmitVirtioIrqMarkerForDevInst(Logger& log, const char* dev_name, DEVINST devinst) {
  if (!dev_name) return;
  if (devinst == 0) {
    log.Logf("%s-irq|WARN|reason=device_missing", dev_name);
    return;
  }

  const auto irq = QueryDevInstIrqModeWithParentFallback(devinst);
  if (!irq.ok) {
    log.Logf("%s-irq|WARN|reason=%s", dev_name,
             irq.reason.empty() ? "resource_query_failed" : irq.reason.c_str());
    return;
  }

  if (!irq.info.is_msi) {
    log.Logf("%s-irq|INFO|mode=intx", dev_name);
    return;
  }

  log.Logf("%s-irq|INFO|mode=msi|messages=%lu", dev_name, static_cast<unsigned long>(irq.info.messages));
}

static void EmitVirtioIrqMarker(Logger& log, const char* dev_name, const std::vector<std::wstring>& hwid_needles,
                                const std::vector<std::wstring>& fallback_needles = {}) {
  if (!dev_name) return;

  // Fast path: restrict to PCI enumerated devices.
  auto matches = FindPresentDevNodesByHwidSubstrings(L"PCI", hwid_needles);
  if (matches.empty() && !fallback_needles.empty()) {
    matches = FindPresentDevNodesByHwidSubstrings(nullptr, fallback_needles);
  }

  if (matches.empty()) {
    EmitVirtioIrqMarkerForDevInst(log, dev_name, 0);
    return;
  }

  EmitVirtioIrqMarkerForDevInst(log, dev_name, matches.front().devinst);
}

static void EmitVirtioNetDiagMarker(Logger& log) {
  HANDLE h = CreateFileW(AEROVNET_DIAG_DEVICE_PATH, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                         OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    const DWORD err = GetLastError();
    if (err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND) {
      log.LogLine("virtio-net-diag|WARN|reason=not_supported");
    } else {
      log.Logf("virtio-net-diag|WARN|reason=open_failed|err=%lu", static_cast<unsigned long>(err));
    }
    return;
  }

  AEROVNET_DIAG_INFO info{};
  DWORD bytes = 0;
  const BOOL ok = DeviceIoControl(h, AEROVNET_DIAG_IOCTL_QUERY, nullptr, 0, &info, sizeof(info), &bytes, nullptr);
  const DWORD err = ok ? 0 : GetLastError();
  CloseHandle(h);

  if (!ok) {
    log.Logf("virtio-net-diag|WARN|reason=ioctl_failed|err=%lu", static_cast<unsigned long>(err));
    return;
  }
  if (bytes < sizeof(ULONG) * 2) {
    log.Logf("virtio-net-diag|WARN|reason=short_read|bytes=%lu", static_cast<unsigned long>(bytes));
    return;
  }

  const char* mode = "unknown";
  if (info.InterruptMode == AEROVNET_INTERRUPT_MODE_INTX) {
    mode = "intx";
  } else if (info.InterruptMode == AEROVNET_INTERRUPT_MODE_MSI) {
    if (info.MsixConfigVector != kVirtioPciMsiNoVector || info.MsixRxVector != kVirtioPciMsiNoVector ||
        info.MsixTxVector != kVirtioPciMsiNoVector) {
      mode = "msix";
    } else {
      mode = "msi";
    }
  }

  const char* rx_err_flags_s = "unknown";
  const char* tx_err_flags_s = "unknown";
  char rx_err_buf[16];
  char tx_err_buf[16];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, RxVqErrorFlags) + sizeof(ULONG)) {
    snprintf(rx_err_buf, sizeof(rx_err_buf), "0x%08lx", static_cast<unsigned long>(info.RxVqErrorFlags));
    rx_err_flags_s = rx_err_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, TxVqErrorFlags) + sizeof(ULONG)) {
    snprintf(tx_err_buf, sizeof(tx_err_buf), "0x%08lx", static_cast<unsigned long>(info.TxVqErrorFlags));
    tx_err_flags_s = tx_err_buf;
  }

  const char* udp4_s = "unknown";
  const char* udp6_s = "unknown";
  char udp4_buf[8];
  char udp6_buf[8];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, TxUdpChecksumV4Enabled) + sizeof(UCHAR)) {
    snprintf(udp4_buf, sizeof(udp4_buf), "%u", static_cast<unsigned>(info.TxUdpChecksumV4Enabled));
    udp4_s = udp4_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, TxUdpChecksumV6Enabled) + sizeof(UCHAR)) {
    snprintf(udp6_buf, sizeof(udp6_buf), "%u", static_cast<unsigned>(info.TxUdpChecksumV6Enabled));
    udp6_s = udp6_buf;
  }

  const char* tx_tcp_offload_s = "unknown";
  const char* tx_tcp_fallback_s = "unknown";
  const char* tx_udp_offload_s = "unknown";
  const char* tx_udp_fallback_s = "unknown";
  char tx_tcp_offload_buf[32];
  char tx_tcp_fallback_buf[32];
  char tx_udp_offload_buf[32];
  char tx_udp_fallback_buf[32];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, StatTxTcpCsumOffload) + sizeof(ULONGLONG)) {
    snprintf(tx_tcp_offload_buf, sizeof(tx_tcp_offload_buf), "%llu",
             static_cast<unsigned long long>(info.StatTxTcpCsumOffload));
    tx_tcp_offload_s = tx_tcp_offload_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, StatTxTcpCsumFallback) + sizeof(ULONGLONG)) {
    snprintf(tx_tcp_fallback_buf, sizeof(tx_tcp_fallback_buf), "%llu",
             static_cast<unsigned long long>(info.StatTxTcpCsumFallback));
    tx_tcp_fallback_s = tx_tcp_fallback_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, StatTxUdpCsumOffload) + sizeof(ULONGLONG)) {
    snprintf(tx_udp_offload_buf, sizeof(tx_udp_offload_buf), "%llu",
             static_cast<unsigned long long>(info.StatTxUdpCsumOffload));
    tx_udp_offload_s = tx_udp_offload_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, StatTxUdpCsumFallback) + sizeof(ULONGLONG)) {
    snprintf(tx_udp_fallback_buf, sizeof(tx_udp_fallback_buf), "%llu",
             static_cast<unsigned long long>(info.StatTxUdpCsumFallback));
    tx_udp_fallback_s = tx_udp_fallback_buf;
  }

  const char* tso_max_s = "unknown";
  char tso_max_buf[32];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, TxTsoMaxOffloadSize) + sizeof(ULONG)) {
    snprintf(tso_max_buf, sizeof(tso_max_buf), "%lu", static_cast<unsigned long>(info.TxTsoMaxOffloadSize));
    tso_max_s = tso_max_buf;
  }

  const char* ctrl_vq_s = "unknown";
  const char* ctrl_rx_s = "unknown";
  const char* ctrl_vlan_s = "unknown";
  const char* ctrl_mac_s = "unknown";
  char ctrl_vq_buf[8];
  char ctrl_rx_buf[8];
  char ctrl_vlan_buf[8];
  char ctrl_mac_buf[8];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlVqNegotiated) + sizeof(UCHAR)) {
    snprintf(ctrl_vq_buf, sizeof(ctrl_vq_buf), "%u", static_cast<unsigned>(info.CtrlVqNegotiated));
    ctrl_vq_s = ctrl_vq_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlRxNegotiated) + sizeof(UCHAR)) {
    snprintf(ctrl_rx_buf, sizeof(ctrl_rx_buf), "%u", static_cast<unsigned>(info.CtrlRxNegotiated));
    ctrl_rx_s = ctrl_rx_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlVlanNegotiated) + sizeof(UCHAR)) {
    snprintf(ctrl_vlan_buf, sizeof(ctrl_vlan_buf), "%u", static_cast<unsigned>(info.CtrlVlanNegotiated));
    ctrl_vlan_s = ctrl_vlan_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlMacAddrNegotiated) + sizeof(UCHAR)) {
    snprintf(ctrl_mac_buf, sizeof(ctrl_mac_buf), "%u", static_cast<unsigned>(info.CtrlMacAddrNegotiated));
    ctrl_mac_s = ctrl_mac_buf;
  }

  const char* ctrl_q_index_s = "unknown";
  const char* ctrl_q_size_s = "unknown";
  char ctrl_q_index_buf[16];
  char ctrl_q_size_buf[16];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlVqQueueIndex) + sizeof(USHORT)) {
    snprintf(ctrl_q_index_buf, sizeof(ctrl_q_index_buf), "%u", static_cast<unsigned>(info.CtrlVqQueueIndex));
    ctrl_q_index_s = ctrl_q_index_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlVqQueueSize) + sizeof(USHORT)) {
    snprintf(ctrl_q_size_buf, sizeof(ctrl_q_size_buf), "%u", static_cast<unsigned>(info.CtrlVqQueueSize));
    ctrl_q_size_s = ctrl_q_size_buf;
  }

  const char* ctrl_err_flags_s = "unknown";
  char ctrl_err_buf[16];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlVqErrorFlags) + sizeof(ULONG)) {
    snprintf(ctrl_err_buf, sizeof(ctrl_err_buf), "0x%08lx", static_cast<unsigned long>(info.CtrlVqErrorFlags));
    ctrl_err_flags_s = ctrl_err_buf;
  }

  const char* ctrl_cmd_sent_s = "unknown";
  const char* ctrl_cmd_ok_s = "unknown";
  const char* ctrl_cmd_err_s = "unknown";
  const char* ctrl_cmd_timeout_s = "unknown";
  char ctrl_cmd_sent_buf[32];
  char ctrl_cmd_ok_buf[32];
  char ctrl_cmd_err_buf[32];
  char ctrl_cmd_timeout_buf[32];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlCmdSent) + sizeof(ULONGLONG)) {
    snprintf(ctrl_cmd_sent_buf, sizeof(ctrl_cmd_sent_buf), "%llu", static_cast<unsigned long long>(info.CtrlCmdSent));
    ctrl_cmd_sent_s = ctrl_cmd_sent_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlCmdOk) + sizeof(ULONGLONG)) {
    snprintf(ctrl_cmd_ok_buf, sizeof(ctrl_cmd_ok_buf), "%llu", static_cast<unsigned long long>(info.CtrlCmdOk));
    ctrl_cmd_ok_s = ctrl_cmd_ok_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlCmdErr) + sizeof(ULONGLONG)) {
    snprintf(ctrl_cmd_err_buf, sizeof(ctrl_cmd_err_buf), "%llu", static_cast<unsigned long long>(info.CtrlCmdErr));
    ctrl_cmd_err_s = ctrl_cmd_err_buf;
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CtrlCmdTimeout) + sizeof(ULONGLONG)) {
    snprintf(ctrl_cmd_timeout_buf, sizeof(ctrl_cmd_timeout_buf), "%llu",
             static_cast<unsigned long long>(info.CtrlCmdTimeout));
    ctrl_cmd_timeout_s = ctrl_cmd_timeout_buf;
  }

  const char* perm_mac_s = "unknown";
  const char* cur_mac_s = "unknown";
  std::string perm_mac_str;
  std::string cur_mac_str;
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, PermanentMac) + 6) {
    perm_mac_str = MacToString(info.PermanentMac);
    perm_mac_s = perm_mac_str.c_str();
  }
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, CurrentMac) + 6) {
    cur_mac_str = MacToString(info.CurrentMac);
    cur_mac_s = cur_mac_str.c_str();
  }

  const char* link_up_s = "unknown";
  char link_up_buf[8];
  if (bytes >= offsetof(AEROVNET_DIAG_INFO, LinkUp) + sizeof(UCHAR)) {
    snprintf(link_up_buf, sizeof(link_up_buf), "%u", static_cast<unsigned>(info.LinkUp));
    link_up_s = link_up_buf;
  }

  log.Logf(
      "virtio-net-diag|INFO|host_features=%s|guest_features=%s|irq_mode=%s|irq_message_count=%lu|"
      "msix_config_vector=0x%04x|msix_rx_vector=0x%04x|msix_tx_vector=0x%04x|"
      "rx_queue_size=%u|tx_queue_size=%u|"
      "rx_avail_idx=%u|rx_used_idx=%u|tx_avail_idx=%u|tx_used_idx=%u|"
      "rx_vq_error_flags=%s|tx_vq_error_flags=%s|"
      "tx_csum_v4=%u|tx_csum_v6=%u|tx_udp_csum_v4=%s|tx_udp_csum_v6=%s|"
      "tx_tcp_csum_offload_pkts=%s|tx_tcp_csum_fallback_pkts=%s|"
      "tx_udp_csum_offload_pkts=%s|tx_udp_csum_fallback_pkts=%s|"
      "tx_tso_v4=%u|tx_tso_v6=%u|tx_tso_max_size=%s|"
      "ctrl_vq=%s|ctrl_rx=%s|ctrl_vlan=%s|ctrl_mac_addr=%s|ctrl_queue_index=%s|ctrl_queue_size=%s|"
      "ctrl_error_flags=%s|ctrl_cmd_sent=%s|ctrl_cmd_ok=%s|ctrl_cmd_err=%s|ctrl_cmd_timeout=%s|"
      "perm_mac=%s|cur_mac=%s|link_up=%s|"
      "stat_tx_err=%llu|stat_rx_err=%llu|stat_rx_no_buf=%llu",
      VirtioFeaturesToString(info.HostFeatures).c_str(), VirtioFeaturesToString(info.GuestFeatures).c_str(), mode,
      static_cast<unsigned long>(info.MessageCount), static_cast<unsigned>(info.MsixConfigVector),
      static_cast<unsigned>(info.MsixRxVector), static_cast<unsigned>(info.MsixTxVector), info.RxQueueSize,
      info.TxQueueSize, info.RxAvailIdx, info.RxUsedIdx, info.TxAvailIdx, info.TxUsedIdx, rx_err_flags_s,
      tx_err_flags_s, info.TxChecksumV4Enabled, info.TxChecksumV6Enabled, udp4_s, udp6_s, tx_tcp_offload_s,
      tx_tcp_fallback_s, tx_udp_offload_s, tx_udp_fallback_s, info.TxTsoV4Enabled, info.TxTsoV6Enabled, tso_max_s,
      ctrl_vq_s, ctrl_rx_s, ctrl_vlan_s, ctrl_mac_s, ctrl_q_index_s, ctrl_q_size_s, ctrl_err_flags_s, ctrl_cmd_sent_s,
      ctrl_cmd_ok_s, ctrl_cmd_err_s, ctrl_cmd_timeout_s,
      perm_mac_s, cur_mac_s, link_up_s,
      static_cast<unsigned long long>(info.StatTxErrors),
      static_cast<unsigned long long>(info.StatRxErrors), static_cast<unsigned long long>(info.StatRxNoBuffers));
}

static bool EmitVirtioNetMsixMarker(Logger& log, bool require_net_msix) {
  HANDLE h = CreateFileW(AEROVNET_DIAG_DEVICE_PATH, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                         OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    const DWORD err = GetLastError();
    if (err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND) {
      log.LogLine(require_net_msix ? "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|FAIL|reason=diag_unavailable"
                                   : "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|SKIP|reason=diag_unavailable");
    } else {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|%s|reason=open_failed|err=%lu",
               require_net_msix ? "FAIL" : "SKIP", static_cast<unsigned long>(err));
    }
    return !require_net_msix;
  }

  AEROVNET_DIAG_INFO info{};
  DWORD bytes = 0;
  const BOOL ok = DeviceIoControl(h, AEROVNET_DIAG_IOCTL_QUERY, nullptr, 0, &info, sizeof(info), &bytes, nullptr);
  const DWORD err = ok ? 0 : GetLastError();
  CloseHandle(h);

  constexpr size_t kRequiredEnd = offsetof(AEROVNET_DIAG_INFO, MsixTxVector) + sizeof(uint16_t);
  constexpr size_t kCountersEnd = offsetof(AEROVNET_DIAG_INFO, TxBuffersDrained) + sizeof(uint32_t);
  if (!ok) {
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|%s|reason=ioctl_failed|err=%lu",
             require_net_msix ? "FAIL" : "SKIP", static_cast<unsigned long>(err));
    return !require_net_msix;
  }
  if (bytes < kRequiredEnd) {
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|%s|reason=diag_truncated|bytes=%lu",
             require_net_msix ? "FAIL" : "SKIP", static_cast<unsigned long>(bytes));
    return !require_net_msix;
  }

  const char* mode = "unknown";
  if (info.InterruptMode == AEROVNET_INTERRUPT_MODE_INTX) {
    mode = "intx";
  } else if (info.InterruptMode == AEROVNET_INTERRUPT_MODE_MSI) {
    // For virtio-net modern, message-signaled interrupts imply MSI-X routing.
    // Infer `msix` if any virtio MSI-X vector is assigned.
    if (info.MsixConfigVector != kVirtioPciMsiNoVector || info.MsixRxVector != kVirtioPciMsiNoVector ||
        info.MsixTxVector != kVirtioPciMsiNoVector) {
      mode = "msix";
    } else {
      mode = "msi";
    }
  }

  auto vec_to_string = [](USHORT v) -> std::string {
    if (v == kVirtioPciMsiNoVector) return "none";
    return std::to_string(static_cast<unsigned int>(v));
  };

  const bool require_ok = !require_net_msix || strcmp(mode, "msix") == 0;
  if (bytes >= kCountersEnd) {
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|%s|mode=%s|messages=%lu|config_vector=%s|rx_vector=%s|tx_vector=%s|"
        "flags=0x%08lx|intr0=%lu|intr1=%lu|intr2=%lu|dpc0=%lu|dpc1=%lu|dpc2=%lu|rx_drained=%lu|tx_drained=%lu",
        require_ok ? "PASS" : "FAIL", mode, static_cast<unsigned long>(info.MessageCount),
        vec_to_string(info.MsixConfigVector).c_str(), vec_to_string(info.MsixRxVector).c_str(),
        vec_to_string(info.MsixTxVector).c_str(), static_cast<unsigned long>(info.Flags),
        static_cast<unsigned long>(info.InterruptCountVector0), static_cast<unsigned long>(info.InterruptCountVector1),
        static_cast<unsigned long>(info.InterruptCountVector2), static_cast<unsigned long>(info.DpcCountVector0),
        static_cast<unsigned long>(info.DpcCountVector1), static_cast<unsigned long>(info.DpcCountVector2),
        static_cast<unsigned long>(info.RxBuffersDrained), static_cast<unsigned long>(info.TxBuffersDrained));
  } else {
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|%s|mode=%s|messages=%lu|config_vector=%s|rx_vector=%s|tx_vector=%s",
        require_ok ? "PASS" : "FAIL", mode, static_cast<unsigned long>(info.MessageCount),
        vec_to_string(info.MsixConfigVector).c_str(), vec_to_string(info.MsixRxVector).c_str(),
        vec_to_string(info.MsixTxVector).c_str());
  }
  return require_ok;
}

static std::optional<AERO_VIRTIO_SND_DIAG_INFO> QueryVirtioSndDiag(Logger& log,
                                                                   DWORD* out_err = nullptr) {
  // Best-effort: the virtio-snd diag device is optional and may not exist on older drivers/images.
  if (out_err) *out_err = ERROR_SUCCESS;
  HANDLE h = CreateFileW(L"\\\\.\\aero_virtio_snd_diag", GENERIC_READ,
                         FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, OPEN_EXISTING, 0, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    if (out_err) *out_err = GetLastError();
    return std::nullopt;
  }

  AERO_VIRTIO_SND_DIAG_INFO info{};
  DWORD bytes = 0;
  const BOOL ok =
      DeviceIoControl(h, IOCTL_AERO_VIRTIO_SND_DIAG_QUERY, nullptr, 0, &info, sizeof(info), &bytes, nullptr);
  const DWORD err = ok ? 0 : GetLastError();
  CloseHandle(h);

  if (!ok) {
    log.Logf("virtio-snd: diag query failed err=%lu", static_cast<unsigned long>(err));
    if (out_err) *out_err = err;
    return std::nullopt;
  }
  if (bytes < sizeof(info)) {
    log.Logf("virtio-snd: diag query returned too few bytes=%lu expected=%zu", static_cast<unsigned long>(bytes),
             sizeof(info));
    if (out_err) *out_err = ERROR_INVALID_DATA;
    return std::nullopt;
  }
  if (info.Version != AERO_VIRTIO_SND_DIAG_VERSION) {
    log.Logf("virtio-snd: diag version mismatch got=%lu expected=%u", static_cast<unsigned long>(info.Version),
             static_cast<unsigned>(AERO_VIRTIO_SND_DIAG_VERSION));
    if (out_err) *out_err = ERROR_INVALID_DATA;
    return std::nullopt;
  }
  return info;
}

static void EmitVirtioSndIrqMarker(Logger& log, DEVINST devinst) {
  if (devinst == 0) {
    EmitVirtioIrqMarkerForDevInst(log, "virtio-snd", 0);
    return;
  }

  const auto info_opt = QueryVirtioSndDiag(log);
  if (!info_opt.has_value()) {
    // Fall back to ConfigManager resource inspection (reports whether Windows assigned message interrupts).
    EmitVirtioIrqMarkerForDevInst(log, "virtio-snd", devinst);
    return;
  }

  const auto& info = *info_opt;
  const char* mode = "unknown";
  if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_INTX) {
    mode = "intx";
  } else if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_MSIX) {
    mode = "msix";
  } else if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_NONE) {
    mode = "none";
  }

  log.Logf(
      "virtio-snd-irq|INFO|mode=%s|messages=%lu|msix_config_vector=0x%04x|msix_queue0_vector=0x%04x|"
      "msix_queue1_vector=0x%04x|msix_queue2_vector=0x%04x|msix_queue3_vector=0x%04x|interrupt_count=%lu|dpc_count=%lu|"
      "drain0=%lu|drain1=%lu|drain2=%lu|drain3=%lu",
      mode, static_cast<unsigned long>(info.MessageCount), static_cast<unsigned>(info.MsixConfigVector),
      static_cast<unsigned>(info.QueueMsixVector[0]), static_cast<unsigned>(info.QueueMsixVector[1]),
      static_cast<unsigned>(info.QueueMsixVector[2]), static_cast<unsigned>(info.QueueMsixVector[3]),
      static_cast<unsigned long>(info.InterruptCount), static_cast<unsigned long>(info.DpcCount),
       static_cast<unsigned long>(info.QueueDrainCount[0]), static_cast<unsigned long>(info.QueueDrainCount[1]),
       static_cast<unsigned long>(info.QueueDrainCount[2]), static_cast<unsigned long>(info.QueueDrainCount[3]));
}

static std::string IrqFieldsForTestMarkerFromDevInst(DEVINST devinst) {
  const auto irq = QueryDevInstIrqModeWithParentFallback(devinst);
  if (!irq.ok) {
    std::string out = "|irq_mode=none|irq_message_count=0";
    if (!irq.reason.empty()) {
      out += "|irq_reason=";
      out += irq.reason;
    }
    return out;
  }

  if (!irq.info.is_msi) {
    return "|irq_mode=intx|irq_message_count=0";
  }

  return std::string("|irq_mode=msi|irq_message_count=") + std::to_string(irq.info.messages);
}

static std::string IrqFieldsForTestMarker(const std::vector<std::wstring>& hwid_needles,
                                          const std::vector<std::wstring>& fallback_needles = {}) {
  // Fast path: restrict to PCI enumerated devices.
  auto matches = FindPresentDevNodesByHwidSubstrings(L"PCI", hwid_needles);
  if (matches.empty() && !fallback_needles.empty()) {
    matches = FindPresentDevNodesByHwidSubstrings(nullptr, fallback_needles);
  }
  if (matches.empty()) {
    return "|irq_mode=none|irq_message_count=0|irq_reason=device_missing";
  }
  return IrqFieldsForTestMarkerFromDevInst(matches.front().devinst);
}

static void EmitVirtioSndMsixMarker(Logger& log, DEVINST devinst) {
  if (devinst == 0) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|SKIP|reason=device_missing");
    return;
  }

  DWORD err = ERROR_SUCCESS;
  const auto info_opt = QueryVirtioSndDiag(log, &err);
  if (!info_opt.has_value()) {
    const unsigned long e = static_cast<unsigned long>(err);
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|SKIP|reason=diag_unavailable|err=%lu", e);
    return;
  }

  const auto& info = *info_opt;
  const char* mode = "unknown";
  if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_INTX) {
    mode = "intx";
  } else if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_MSIX) {
    mode = "msix";
  } else if (info.IrqMode == AERO_VIRTIO_SND_DIAG_IRQ_MODE_NONE) {
    mode = "none";
  }

  auto vec_to_string = [](USHORT v) -> std::string {
    if (v == kVirtioPciMsiNoVector) return "none";
    return std::to_string(static_cast<unsigned int>(v));
  };

  log.Logf(
      "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=%s|messages=%lu|config_vector=%s|queue0_vector=%s|"
      "queue1_vector=%s|queue2_vector=%s|queue3_vector=%s|interrupts=%lu|dpcs=%lu|drain0=%lu|drain1=%lu|drain2=%lu|"
      "drain3=%lu",
      mode, static_cast<unsigned long>(info.MessageCount), vec_to_string(info.MsixConfigVector).c_str(),
      vec_to_string(info.QueueMsixVector[0]).c_str(), vec_to_string(info.QueueMsixVector[1]).c_str(),
      vec_to_string(info.QueueMsixVector[2]).c_str(), vec_to_string(info.QueueMsixVector[3]).c_str(),
      static_cast<unsigned long>(info.InterruptCount), static_cast<unsigned long>(info.DpcCount),
      static_cast<unsigned long>(info.QueueDrainCount[0]), static_cast<unsigned long>(info.QueueDrainCount[1]),
      static_cast<unsigned long>(info.QueueDrainCount[2]), static_cast<unsigned long>(info.QueueDrainCount[3]));
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

static std::optional<DWORD> QueryRegDword(HKEY key, const wchar_t* value_name) {
  if (!key || key == INVALID_HANDLE_VALUE || !value_name) return std::nullopt;

  DWORD type = 0;
  DWORD data = 0;
  DWORD bytes = sizeof(data);
  const LONG rc = RegQueryValueExW(key, value_name, nullptr, &type, reinterpret_cast<LPBYTE>(&data), &bytes);
  if (rc != ERROR_SUCCESS || type != REG_DWORD || bytes < sizeof(DWORD)) return std::nullopt;
  return data;
}

enum class VirtioSndToggleRegSource : DWORD {
  DeviceParametersSubkey = 0,
  DeviceKeyRoot = 1,
  DriverParametersSubkey = 2,
  DriverKeyRoot = 3,
};

static const char* VirtioSndToggleRegSourceToString(VirtioSndToggleRegSource source) {
  switch (source) {
    case VirtioSndToggleRegSource::DeviceParametersSubkey:
      return "device_parameters";
    case VirtioSndToggleRegSource::DeviceKeyRoot:
      return "device_root";
    case VirtioSndToggleRegSource::DriverParametersSubkey:
      return "driver_parameters";
    case VirtioSndToggleRegSource::DriverKeyRoot:
      return "driver_root";
    default:
      return "unknown";
  }
}

struct RegDwordLookup {
  DWORD value = 0;
  bool from_parameters_subkey = false;
};

static std::optional<RegDwordLookup> QueryDeviceDevRegDword(HDEVINFO devinfo, SP_DEVINFO_DATA* dev,
                                                            const wchar_t* value_name) {
  if (!devinfo || devinfo == INVALID_HANDLE_VALUE || !dev || !value_name) return std::nullopt;

  HKEY root = SetupDiOpenDevRegKey(devinfo, dev, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_QUERY_VALUE);
  if (root == INVALID_HANDLE_VALUE) return std::nullopt;

  // Bring-up toggles are typically placed under a Parameters subkey, but some
  // environments may store values directly under the device key as a fallback.
  HKEY params = INVALID_HANDLE_VALUE;
  const LONG rc = RegOpenKeyExW(root, L"Parameters", 0, KEY_QUERY_VALUE, &params);
  if (rc == ERROR_SUCCESS && params != INVALID_HANDLE_VALUE) {
    auto value = QueryRegDword(params, value_name);
    RegCloseKey(params);
    if (value.has_value()) {
      RegCloseKey(root);
      return RegDwordLookup{*value, true};
    }
  }

  if (auto value = QueryRegDword(root, value_name)) {
    RegCloseKey(root);
    return RegDwordLookup{*value, false};
  }

  RegCloseKey(root);
  return std::nullopt;
}

static std::optional<RegDwordLookup> QueryDeviceDriverRegDword(HDEVINFO devinfo, SP_DEVINFO_DATA* dev,
                                                               const wchar_t* value_name) {
  if (!devinfo || devinfo == INVALID_HANDLE_VALUE || !dev || !value_name) return std::nullopt;

  HKEY root = SetupDiOpenDevRegKey(devinfo, dev, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_QUERY_VALUE);
  if (root == INVALID_HANDLE_VALUE) return std::nullopt;

  // Bring-up toggles are typically placed under a Parameters subkey, but some
  // environments may store values directly under the driver key as a fallback.
  HKEY params = INVALID_HANDLE_VALUE;
  const LONG rc = RegOpenKeyExW(root, L"Parameters", 0, KEY_QUERY_VALUE, &params);
  if (rc == ERROR_SUCCESS && params != INVALID_HANDLE_VALUE) {
    auto value = QueryRegDword(params, value_name);
    RegCloseKey(params);
    if (value.has_value()) {
      RegCloseKey(root);
      return RegDwordLookup{*value, true};
    }
  }

  if (auto value = QueryRegDword(root, value_name)) {
    RegCloseKey(root);
    return RegDwordLookup{*value, false};
  }

  RegCloseKey(root);
  return std::nullopt;
}

struct VirtioSndPciDevice {
  DEVINST devinst = 0;
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
  std::optional<VirtioSndToggleRegSource> force_null_backend_source;
  std::optional<DWORD> allow_polling_only;
  std::optional<VirtioSndToggleRegSource> allow_polling_only_source;
};

// KSCATEGORY_TOPOLOGY {DDA54A40-1E4C-11D1-A050-405705C10000}
static const GUID kKsCategoryTopology = {0xdda54a40,
                                           0x1e4c,
                                           0x11d1,
                                           {0xa0, 0x50, 0x40, 0x57, 0x05, 0xc1, 0x00, 0x00}};

// Custom virtio-snd property set (driver diagnostics).
// Must match `drivers/windows7/virtio-snd/src/topology.c`.
static const GUID kKsPropSetAeroVirtioSnd = {
    0x3c0f8a06, 0x4f4b, 0x4c49, {0x9d, 0x1a, 0x8f, 0xbe, 0x0b, 0x9e, 0x4b, 0x7a}};
static constexpr ULONG kKsPropAeroVirtioSndEventqStats = 0x00000001u;

// Minimal KS property IOCTL definitions (some Win7 SDK-only toolchains omit ks.h).
#ifndef FILE_DEVICE_KS
#define FILE_DEVICE_KS 0x0000002F
#endif
#ifndef IOCTL_KS_PROPERTY
#define IOCTL_KS_PROPERTY CTL_CODE(FILE_DEVICE_KS, 0x000, METHOD_NEITHER, FILE_ANY_ACCESS)
#endif
#ifndef KSPROPERTY_TYPE_GET
#define KSPROPERTY_TYPE_GET 0x00000001u
#endif

// Local KSPROPERTY-compatible header (avoid relying on ks.h being present).
// Note: we intentionally do NOT use the type name `KSPROPERTY` to avoid conflicts
// when building with SDKs that do provide `KSPROPERTY`.
struct KsPropertyHeader {
  GUID Set;
  ULONG Id;
  ULONG Flags;
};

#pragma pack(push, 1)
struct AEROVIRTIO_SND_EVENTQ_STATS {
  ULONG Size;
  LONG Completions;
  LONG Parsed;
  LONG ShortBuffers;
  LONG UnknownType;
  LONG JackConnected;
  LONG JackDisconnected;
  LONG PcmPeriodElapsed;
  LONG PcmXrun;
  LONG CtlNotify;
};
#pragma pack(pop)

static_assert(sizeof(AEROVIRTIO_SND_EVENTQ_STATS) == 40, "AEROVIRTIO_SND_EVENTQ_STATS layout");

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
    snd.devinst = dev.DevInst;
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
      snd.force_null_backend = force->value;
      snd.force_null_backend_source = force->from_parameters_subkey
                                          ? VirtioSndToggleRegSource::DeviceParametersSubkey
                                          : VirtioSndToggleRegSource::DeviceKeyRoot;
    } else if (auto force = QueryDeviceDriverRegDword(devinfo, &dev, L"ForceNullBackend")) {
      snd.force_null_backend = force->value;
      snd.force_null_backend_source = force->from_parameters_subkey
                                          ? VirtioSndToggleRegSource::DriverParametersSubkey
                                          : VirtioSndToggleRegSource::DriverKeyRoot;
    }
    if (auto allow = QueryDeviceDevRegDword(devinfo, &dev, L"AllowPollingOnly")) {
      snd.allow_polling_only = allow->value;
      snd.allow_polling_only_source = allow->from_parameters_subkey
                                          ? VirtioSndToggleRegSource::DeviceParametersSubkey
                                          : VirtioSndToggleRegSource::DeviceKeyRoot;
    } else if (auto allow = QueryDeviceDriverRegDword(devinfo, &dev, L"AllowPollingOnly")) {
      snd.allow_polling_only = allow->value;
      snd.allow_polling_only_source = allow->from_parameters_subkey
                                          ? VirtioSndToggleRegSource::DriverParametersSubkey
                                          : VirtioSndToggleRegSource::DriverKeyRoot;
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
        log.Logf("virtio-snd: detected PCI device ForceNullBackend=%lu source=%s",
                 static_cast<unsigned long>(*snd.force_null_backend),
                 snd.force_null_backend_source.has_value()
                     ? VirtioSndToggleRegSourceToString(*snd.force_null_backend_source)
                     : "unknown");
      }
      if (snd.allow_polling_only.has_value()) {
        log.Logf("virtio-snd: detected PCI device AllowPollingOnly=%lu source=%s",
                 static_cast<unsigned long>(*snd.allow_polling_only),
                 snd.allow_polling_only_source.has_value()
                     ? VirtioSndToggleRegSourceToString(*snd.allow_polling_only_source)
                     : "unknown");
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

static std::optional<std::wstring> GetDeviceInterfacePathForInstance(
    Logger& log, const GUID& iface_guid, const std::wstring& target_instance_id,
    const char* iface_name_for_log) {
  HDEVINFO devinfo =
      SetupDiGetClassDevsW(&iface_guid, nullptr, nullptr, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-snd: SetupDiGetClassDevs(%s) failed: %lu", iface_name_for_log, GetLastError());
    return std::nullopt;
  }

  std::optional<std::wstring> found;
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

    found = std::wstring(detail->DevicePath);
    break;
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  if (found) {
    log.Logf("virtio-snd: using %s interface path=%s", iface_name_for_log,
             WideToUtf8(*found).c_str());
  }
  return found;
}

static std::optional<AEROVIRTIO_SND_EVENTQ_STATS> QueryVirtioSndEventqStats(Logger& log,
                                                                            const std::wstring& topology_path) {
  if (topology_path.empty()) return std::nullopt;

  // IOCTL_KS_PROPERTY uses FILE_ANY_ACCESS; open with 0 desired access to avoid requiring
  // extra permissions on some setups.
  HANDLE h =
      CreateFileW(topology_path.c_str(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-snd-eventq: CreateFile(topology) failed: %lu", GetLastError());
    return std::nullopt;
  }

  KsPropertyHeader prop{};
  prop.Set = kKsPropSetAeroVirtioSnd;
  prop.Id = kKsPropAeroVirtioSndEventqStats;
  prop.Flags = KSPROPERTY_TYPE_GET;

  AEROVIRTIO_SND_EVENTQ_STATS out{};
  DWORD bytes = 0;
  const BOOL ok =
      DeviceIoControl(h, IOCTL_KS_PROPERTY, &prop, sizeof(prop), &out, sizeof(out), &bytes, nullptr);
  const DWORD err = ok ? 0 : GetLastError();
  CloseHandle(h);

  if (!ok) {
    log.Logf("virtio-snd-eventq: IOCTL_KS_PROPERTY failed: %lu", err);
    return std::nullopt;
  }

  if (bytes < sizeof(out) || out.Size != sizeof(out)) {
    log.Logf("virtio-snd-eventq: unexpected stats size bytes=%lu reported_size=%lu expected=%lu",
             static_cast<unsigned long>(bytes), static_cast<unsigned long>(out.Size),
             static_cast<unsigned long>(sizeof(out)));
  }

  return out;
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

static DEVINST FindDiskDevInstForDiskNumber(Logger& log, DWORD disk_number) {
  HDEVINFO devinfo =
      SetupDiGetClassDevsW(&GUID_DEVINTERFACE_DISK, nullptr, nullptr, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  if (devinfo == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: SetupDiGetClassDevs(GUID_DEVINTERFACE_DISK) failed: %lu", GetLastError());
    return 0;
  }

  DEVINST out = 0;

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

    HANDLE h =
        CreateFileW(detail->DevicePath, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, 0, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
      continue;
    }

    STORAGE_DEVICE_NUMBER devnum{};
    DWORD bytes = 0;
    const bool ok = DeviceIoControl(h, IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0, &devnum, sizeof(devnum),
                                    &bytes, nullptr) != 0;
    CloseHandle(h);
    if (!ok) continue;

    if (devnum.DeviceNumber == disk_number) {
      out = dev.DevInst;
      break;
    }
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return out;
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

  // Populate SCSI addressing fields for pass-through. Some stacks require these to be set correctly.
  // If the query fails, fall back to 0/0/0 (common for single-disk virtio-blk setups).
  SCSI_ADDRESS addr{};
  addr.Length = sizeof(addr);
  DWORD addr_bytes = 0;
  if (DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_GET_ADDRESS, nullptr, 0, &addr, sizeof(addr), &addr_bytes,
                      nullptr)) {
    if (addr_bytes < sizeof(addr)) {
      log.Logf("virtio-blk: REPORT_LUNS warning: IOCTL_SCSI_GET_ADDRESS returned short bytes=%lu (using 0/0/0)",
               static_cast<unsigned long>(addr_bytes));
      addr.PortNumber = 0;
      addr.PathId = 0;
      addr.TargetId = 0;
      addr.Lun = 0;
    }
    log.Logf("virtio-blk: REPORT_LUNS scsi_address port=%u path=%u target=%u lun=%u",
             static_cast<unsigned>(addr.PortNumber), static_cast<unsigned>(addr.PathId),
             static_cast<unsigned>(addr.TargetId), static_cast<unsigned>(addr.Lun));
  } else {
    addr.PortNumber = 0;
    addr.PathId = 0;
    addr.TargetId = 0;
    addr.Lun = 0;
    log.Logf("virtio-blk: REPORT_LUNS warning: IOCTL_SCSI_GET_ADDRESS failed err=%lu (using 0/0/0)",
             static_cast<unsigned long>(GetLastError()));
  }

  constexpr uint32_t kAllocLen = 64;
  // Fill with a non-zero pattern so truncated/short transfers don't get mistaken for an all-zero LUN entry.
  std::vector<uint8_t> resp(kAllocLen, 0xCC);

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
  pkt.sptd.TimeOutValue = 10;
  pkt.sptd.DataBuffer = resp.data();
  pkt.sptd.SenseInfoOffset = static_cast<ULONG>(FIELD_OFFSET(ScsiPassThroughDirectWithSense, sense));
  memcpy(pkt.sptd.Cdb, cdb, sizeof(cdb));

  DWORD returned = 0;
  const BOOL ok = DeviceIoControl(hPhysicalDrive, IOCTL_SCSI_PASS_THROUGH_DIRECT, &pkt, sizeof(pkt), &pkt,
                                  sizeof(pkt), &returned, nullptr);
  const DWORD err = ok ? ERROR_SUCCESS : GetLastError();

  if (!ok) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL DeviceIoControl(IOCTL_SCSI_PASS_THROUGH_DIRECT) err=%lu",
             static_cast<unsigned long>(err));
    log.Logf("virtio-blk: REPORT_LUNS payload[%lu]=%s", static_cast<unsigned long>(resp.size()),
             HexDump(resp.data(), resp.size()).c_str());
    LogScsiSenseSummary(log, "virtio-blk: REPORT_LUNS", reinterpret_cast<const uint8_t*>(pkt.sense),
                        sizeof(pkt.sense));
    log.Logf("virtio-blk: REPORT_LUNS sense[32]=%s",
             HexDump(reinterpret_cast<const uint8_t*>(pkt.sense), sizeof(pkt.sense)).c_str());
    return false;
  }

  if (pkt.sptd.ScsiStatus != 0) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL (SCSI status=0x%02x)", static_cast<unsigned>(pkt.sptd.ScsiStatus));
    log.Logf("virtio-blk: REPORT_LUNS payload[%lu]=%s", static_cast<unsigned long>(resp.size()),
             HexDump(resp.data(), resp.size()).c_str());
    LogScsiSenseSummary(log, "virtio-blk: REPORT_LUNS", reinterpret_cast<const uint8_t*>(pkt.sense),
                        sizeof(pkt.sense));
    log.Logf("virtio-blk: REPORT_LUNS sense[32]=%s",
             HexDump(reinterpret_cast<const uint8_t*>(pkt.sense), sizeof(pkt.sense)).c_str());
    return false;
  }

  const uint32_t list_len = ReadBe32(resp.data());
  const uint32_t reserved = ReadBe32(resp.data() + 4);
  bool lun0_all_zero = true;
  for (size_t i = 8; i < 16; i++) {
    if (resp[i] != 0) lun0_all_zero = false;
  }

  if (list_len != 8 || reserved != 0 || !lun0_all_zero) {
    log.Logf("virtio-blk: REPORT_LUNS FAIL (invalid payload list_len=%lu reserved=%lu lun0_all_zero=%d)",
             static_cast<unsigned long>(list_len), static_cast<unsigned long>(reserved), lun0_all_zero ? 1 : 0);
    log.Logf("virtio-blk: REPORT_LUNS payload[%lu]=%s", static_cast<unsigned long>(resp.size()),
             HexDump(resp.data(), resp.size()).c_str());
    LogScsiSenseSummary(log, "virtio-blk: REPORT_LUNS", reinterpret_cast<const uint8_t*>(pkt.sense),
                        sizeof(pkt.sense));
    log.Logf("virtio-blk: REPORT_LUNS sense[32]=%s",
             HexDump(reinterpret_cast<const uint8_t*>(pkt.sense), sizeof(pkt.sense)).c_str());
    return false;
  }

  log.LogLine("virtio-blk: REPORT_LUNS PASS");
  return true;
}

struct VirtioBlkSelection {
  DWORD disk_number = 0;
  std::wstring base_dir;
};

static std::optional<VirtioBlkSelection> SelectVirtioBlkSelection(Logger& log, const Options& opt) {
  const auto disks = DetectVirtioDiskNumbers(log);
  if (disks.empty()) {
    log.LogLine("virtio-blk: no virtio disk devices detected");
    return std::nullopt;
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
    return std::nullopt;
  }

  const auto base_disk = DiskNumberForDriveLetter(*base_drive);
  if (!base_disk.has_value()) {
    log.Logf("virtio-blk: unable to query disk number for %lc:", *base_drive);
    log.LogLine("virtio-blk: specify --blk-root (e.g. D:\\aero-test\\) on a virtio volume");
    return std::nullopt;
  }

  if (disks.count(*base_disk) == 0 && !DriveLetterLooksLikeVirtio(log, *base_drive)) {
    log.Logf("virtio-blk: test dir is on disk %lu (not detected as virtio)", *base_disk);
    log.LogLine("virtio-blk: ensure a virtio disk is formatted/mounted with a drive letter, or pass --blk-root");
    return std::nullopt;
  }

  VirtioBlkSelection out{};
  out.disk_number = *base_disk;
  out.base_dir = std::move(base_dir);
  return out;
}

struct VirtioBlkTestResult {
  bool ok = false;

  bool write_ok = false;
  uint64_t write_bytes = 0;
  double write_mbps = 0.0;

  bool flush_ok = false;

  // Read path includes both readback verification and a separate sequential read pass.
  bool read_ok = false;
  uint64_t read_bytes = 0;
  double read_mbps = 0.0;

  // Best-effort internal diagnostics; not currently emitted in the public marker.
  bool verify_ok = false;
};

static VirtioBlkTestResult VirtioBlkTest(Logger& log, const Options& opt,
                                         std::optional<AerovblkQueryInfoResult>* miniport_info_out = nullptr,
                                         DEVINST* devinst_out = nullptr) {
  VirtioBlkTestResult out{};
  if (miniport_info_out) miniport_info_out->reset();
  if (devinst_out) *devinst_out = 0;

  const auto sel = SelectVirtioBlkSelection(log, opt);
  if (!sel.has_value()) return out;

  const DWORD disk_number = sel->disk_number;
  const std::wstring& base_dir = sel->base_dir;

  if (devinst_out) {
    *devinst_out = FindDiskDevInstForDiskNumber(log, disk_number);
  }

  // Exercise aero_virtio_blk.sys miniport IOCTL_SCSI_MINIPORT query contract via \\.\PhysicalDrive<N>.
  {
    HANDLE pd = OpenPhysicalDriveForIoctl(log, disk_number);
    if (pd == INVALID_HANDLE_VALUE) {
      log.LogLine("virtio-blk: miniport query FAIL (unable to open PhysicalDrive)");
      return out;
    }

    const auto info = QueryAerovblkMiniportInfo(log, pd);
    bool query_ok = false;
    if (!info.has_value()) {
      log.LogLine("virtio-blk: miniport query FAIL (IOCTL_SCSI_MINIPORT query failed)");
    } else {
      if (miniport_info_out) *miniport_info_out = *info;
      query_ok = ValidateAerovblkMiniportInfo(log, *info);

      // Optional: capacity-change counter (variable-length contract).
      if (query_ok) {
        constexpr size_t kCapEventsEnd =
            offsetof(AEROVBLK_QUERY_INFO, CapacityChangeEvents) + sizeof(ULONG);
        if (info->returned_len >= kCapEventsEnd) {
          log.Logf("virtio-blk: capacity_change_events=%lu",
                   static_cast<unsigned long>(info->info.CapacityChangeEvents));
        } else {
          log.LogLine("virtio-blk: capacity_change_events=not_supported");
        }
      }
      if (query_ok && opt.expect_blk_msi) {
        constexpr size_t kIrqModeEnd = offsetof(AEROVBLK_QUERY_INFO, InterruptMode) + sizeof(ULONG);
        if (info->returned_len < kIrqModeEnd) {
          // Best-effort: older miniport contracts may not report interrupt mode. Do not fail the test in that
          // scenario; the host harness can still observe INTx/MSI via PnP resources if needed.
          log.Logf("virtio-blk: miniport query WARN (expected MSI/MSI-X but InterruptMode not reported; len=%zu)",
                   info->returned_len);
        } else if (info->info.InterruptMode != AEROVBLK_INTERRUPT_MODE_MSI) {
          log.Logf("virtio-blk: miniport query FAIL (expected MSI/MSI-X, got InterruptMode=%lu)",
                   static_cast<unsigned long>(info->info.InterruptMode));
          query_ok = false;
        }
      }
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

    if (!query_ok) return out;
    if (!report_luns_ok) return out;
  }

  const std::wstring test_file = JoinPath(base_dir, L"virtio-blk-test.bin");
  log.Logf("virtio-blk: test_file=%s size_mib=%lu chunk_kib=%lu", WideToUtf8(test_file).c_str(),
           opt.io_file_size_mib, opt.io_chunk_kib);

  const uint64_t total_bytes = static_cast<uint64_t>(opt.io_file_size_mib) * 1024ull * 1024ull;
  const uint32_t chunk_bytes = std::max<DWORD>(1, opt.io_chunk_kib) * 1024u;

  std::vector<uint8_t> buf(chunk_bytes);

  HANDLE h = INVALID_HANDLE_VALUE;
  bool file_created = false;

  auto cleanup = [&]() {
    if (h != INVALID_HANDLE_VALUE) {
      CloseHandle(h);
      h = INVALID_HANDLE_VALUE;
    }
    if (file_created) {
      DeleteFileW(test_file.c_str());
      file_created = false;
    }
  };

  h = CreateFileW(test_file.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                  FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: CreateFile failed: %lu", GetLastError());
    return out;
  }
  file_created = true;

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
        out.write_ok = false;
        out.write_bytes = written_total;
        cleanup();
        return out;
      }
      written_total += written;
    }
    const double sec = std::max(0.000001, t.SecondsSinceStart());
    out.write_ok = true;
    out.write_bytes = written_total;
    out.write_mbps = (written_total / (1024.0 * 1024.0)) / sec;
    log.Logf("virtio-blk: write ok bytes=%llu mbps=%.2f", written_total,
             out.write_mbps);
  }

  if (!FlushFileBuffers(h)) {
    log.Logf("virtio-blk: FlushFileBuffers failed: %lu", GetLastError());
    out.flush_ok = false;
    cleanup();
    return out;
  }
  out.flush_ok = true;
  log.LogLine("virtio-blk: flush ok");

  // Readback verify.
  if (SetFilePointer(h, 0, nullptr, FILE_BEGIN) == INVALID_SET_FILE_POINTER &&
      GetLastError() != NO_ERROR) {
    log.Logf("virtio-blk: SetFilePointer failed: %lu", GetLastError());
    cleanup();
    return out;
  }

  {
    uint64_t read_total = 0;
    while (read_total < total_bytes) {
      const uint32_t to_read =
          static_cast<uint32_t>(std::min<uint64_t>(chunk_bytes, total_bytes - read_total));
      DWORD read = 0;
      if (!ReadFile(h, buf.data(), to_read, &read, nullptr) || read != to_read) {
        log.Logf("virtio-blk: ReadFile failed at offset=%llu err=%lu", read_total, GetLastError());
        out.verify_ok = false;
        cleanup();
        return out;
      }
      for (uint32_t i = 0; i < to_read; i++) {
        const uint8_t expected = static_cast<uint8_t>((read_total + i) & 0xFF);
        if (buf[i] != expected) {
          log.Logf("virtio-blk: data mismatch at offset=%llu expected=0x%02x got=0x%02x",
                   (read_total + i), expected, buf[i]);
          out.verify_ok = false;
          cleanup();
          return out;
        }
      }
      read_total += read;
    }
    out.verify_ok = true;
    log.Logf("virtio-blk: readback verify ok bytes=%llu", read_total);
  }

  CloseHandle(h);
  h = INVALID_HANDLE_VALUE;

  // Separate sequential read pass (reopen file).
  h = CreateFileW(test_file.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                  FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk: reopen for read failed: %lu", GetLastError());
    cleanup();
    return out;
  }

  {
    PerfTimer t;
    uint64_t read_total = 0;
    while (true) {
      DWORD read = 0;
      if (!ReadFile(h, buf.data(), chunk_bytes, &read, nullptr)) {
        log.Logf("virtio-blk: sequential ReadFile failed err=%lu", GetLastError());
        out.read_ok = false;
        cleanup();
        return out;
      }
      if (read == 0) break;
      read_total += read;
    }
    const double sec = std::max(0.000001, t.SecondsSinceStart());
    out.read_ok = true;
    out.read_bytes = read_total;
    out.read_mbps = (read_total / (1024.0 * 1024.0)) / sec;
    log.Logf("virtio-blk: sequential read ok bytes=%llu mbps=%.2f", read_total,
             out.read_mbps);
  }

  cleanup();
  out.read_ok = out.read_ok && out.verify_ok;
  out.ok = out.write_ok && out.flush_ok && out.read_ok;
  return out;
}

struct VirtioBlkResetTestResult {
  bool ok = false;
  bool performed = false;
  bool skipped_not_supported = false;
  std::string fail_reason;
  DWORD win32_error = ERROR_SUCCESS;
  std::optional<uint32_t> counter_before;
  std::optional<uint32_t> counter_after;
};

static bool VirtioBlkResetSmokeIo(Logger& log, const std::wstring& base_dir, DWORD* out_err, const char** out_stage) {
  if (out_err) *out_err = ERROR_SUCCESS;
  if (out_stage) *out_stage = "";

  const std::wstring test_file = JoinPath(base_dir, L"virtio-blk-reset-smoke.bin");
  HANDLE h = CreateFileW(test_file.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                         FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    if (out_err) *out_err = GetLastError();
    if (out_stage) *out_stage = "create";
    return false;
  }

  bool ok = false;
  DWORD err = ERROR_SUCCESS;
  const char* stage = "";
  std::vector<uint8_t> buf(4096);
  for (size_t i = 0; i < buf.size(); i++) {
    buf[i] = static_cast<uint8_t>(0xA5 ^ static_cast<uint8_t>(i & 0xFF));
  }

  DWORD written = 0;
  if (!WriteFile(h, buf.data(), static_cast<DWORD>(buf.size()), &written, nullptr) || written != buf.size()) {
    err = GetLastError();
    stage = "write";
    goto done;
  }
  if (!FlushFileBuffers(h)) {
    err = GetLastError();
    stage = "flush";
    goto done;
  }
  if (SetFilePointer(h, 0, nullptr, FILE_BEGIN) == INVALID_SET_FILE_POINTER && GetLastError() != NO_ERROR) {
    err = GetLastError();
    stage = "seek";
    goto done;
  }

  std::vector<uint8_t> read_buf(buf.size());
  DWORD read = 0;
  if (!ReadFile(h, read_buf.data(), static_cast<DWORD>(read_buf.size()), &read, nullptr) || read != read_buf.size()) {
    err = GetLastError();
    stage = "read";
    goto done;
  }

  if (memcmp(buf.data(), read_buf.data(), buf.size()) != 0) {
    err = ERROR_CRC;
    stage = "verify";
    goto done;
  }

  ok = true;

done:
  CloseHandle(h);
  (void)DeleteFileW(test_file.c_str());
  if (!ok) {
    if (out_err) *out_err = err;
    if (out_stage) *out_stage = stage;
    return false;
  }
  log.LogLine("virtio-blk-reset: smoke IO ok");
  return true;
}

static VirtioBlkResetTestResult VirtioBlkResetTest(Logger& log, const VirtioBlkSelection& target) {
  VirtioBlkResetTestResult out{};

  constexpr size_t kIoctlResetCountEnd = offsetof(AEROVBLK_QUERY_INFO, IoctlResetCount) + sizeof(ULONG);

  DWORD open_err = ERROR_SUCCESS;
  HANDLE pd = TryOpenPhysicalDriveForIoctl(target.disk_number, &open_err);
  if (pd == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-blk-reset: unable to open PhysicalDrive%lu err=%lu",
             static_cast<unsigned long>(target.disk_number), static_cast<unsigned long>(open_err));
    out.fail_reason = "open_physical_drive_failed";
    out.win32_error = open_err;
    return out;
  }

  const auto before_opt = QueryAerovblkMiniportInfo(log, pd);
  if (!before_opt.has_value()) {
    out.win32_error = GetLastError();
    CloseHandle(pd);
    out.fail_reason = "miniport_query_before_failed";
    return out;
  }
  if (!ValidateAerovblkMiniportInfo(log, *before_opt)) {
    CloseHandle(pd);
    out.fail_reason = "miniport_info_before_invalid";
    out.win32_error = ERROR_INVALID_DATA;
    return out;
  }

  if (before_opt->returned_len >= kIoctlResetCountEnd) {
    out.counter_before = before_opt->info.IoctlResetCount;
  }

  bool performed = false;
  const bool reset_ok = ForceAerovblkMiniportReset(log, pd, &performed);
  if (!reset_ok) out.win32_error = GetLastError();
  CloseHandle(pd);

  if (!reset_ok) {
    out.fail_reason = "force_reset_failed";
    return out;
  }
  if (!performed) {
    out.ok = true;
    out.performed = false;
    out.skipped_not_supported = true;
    out.win32_error = ERROR_NOT_SUPPORTED;
    return out;
  }
  out.performed = true;

  const DWORD start = GetTickCount();
  constexpr DWORD kTimeoutMs = 15000;
  DWORD last_err = ERROR_SUCCESS;
  const char* last_stage = "";
  bool logged_retry = false;
  bool smoke_ok = false;
  while (true) {
    DWORD err = ERROR_SUCCESS;
    const char* stage = "";
    if (VirtioBlkResetSmokeIo(log, target.base_dir, &err, &stage)) {
      smoke_ok = true;
      break;
    }
    last_err = err;
    last_stage = stage ? stage : "";
    if (!logged_retry) {
      log.Logf("virtio-blk-reset: smoke IO retrying stage=%s err=%lu", last_stage,
               static_cast<unsigned long>(last_err));
      logged_retry = true;
    }
    if (GetTickCount() - start >= kTimeoutMs) break;
    Sleep(200);
  }
  if (!smoke_ok) {
    log.Logf("virtio-blk-reset: smoke IO failed stage=%s err=%lu", last_stage,
             static_cast<unsigned long>(last_err));
    out.fail_reason = "post_reset_io_failed";
    out.win32_error = last_err;
    return out;
  }

  // Verify the miniport IOCTL query still works after the forced reset.
  AerovblkQueryInfoResult after{};
  bool after_ok = false;
  DWORD last_open_err = ERROR_SUCCESS;
  DWORD last_query_err = ERROR_SUCCESS;
  for (int attempt = 0; attempt < 5; attempt++) {
    DWORD err = ERROR_SUCCESS;
    HANDLE pd2 = TryOpenPhysicalDriveForIoctl(target.disk_number, &err);
    last_open_err = err;
    if (pd2 == INVALID_HANDLE_VALUE) {
      Sleep(200);
      continue;
    }
    const auto q = QueryAerovblkMiniportInfo(log, pd2);
    if (!q.has_value()) last_query_err = GetLastError();
    CloseHandle(pd2);
    if (!q.has_value()) {
      Sleep(200);
      continue;
    }
    if (!ValidateAerovblkMiniportInfo(log, *q)) {
      Sleep(200);
      continue;
    }
    after = *q;
    after_ok = true;
    break;
  }
  if (!after_ok) {
    log.Logf("virtio-blk-reset: miniport query after reset failed err=%lu", static_cast<unsigned long>(last_open_err));
    out.fail_reason = "miniport_query_after_failed";
    out.win32_error = last_query_err != ERROR_SUCCESS ? last_query_err : last_open_err;
    return out;
  }

  if (after.returned_len >= kIoctlResetCountEnd) {
    out.counter_after = after.info.IoctlResetCount;
  }

  if (out.counter_before.has_value() && out.counter_after.has_value()) {
    const uint32_t after_count = *out.counter_after;
    if (after_count < *out.counter_before + 1) {
      log.Logf("virtio-blk-reset: ioctl_reset_count did not increment before=%lu after=%lu",
               static_cast<unsigned long>(*out.counter_before), static_cast<unsigned long>(after_count));
      out.fail_reason = "ioctl_reset_count_not_incremented";
      out.win32_error = ERROR_INVALID_DATA;
      return out;
    }
    log.Logf("virtio-blk-reset: ioctl_reset_count before=%lu after=%lu", static_cast<unsigned long>(*out.counter_before),
             static_cast<unsigned long>(after_count));
  } else {
    log.LogLine("virtio-blk-reset: ioctl_reset_count not available");
  }

  out.ok = true;
  out.fail_reason.clear();
  return out;
}

static std::optional<uint64_t> QueryAerovblkCapacityChangeEvents(Logger& log, HANDLE hPhysicalDrive) {
  const auto q = QueryAerovblkMiniportInfo(log, hPhysicalDrive);
  if (!q.has_value()) return std::nullopt;
  constexpr size_t kCapEventsEnd =
      offsetof(AEROVBLK_QUERY_INFO, CapacityChangeEvents) + sizeof(ULONG);
  if (q->returned_len < kCapEventsEnd) return std::nullopt;
  return static_cast<uint64_t>(q->info.CapacityChangeEvents);
}

static void VirtioBlkResizeProbe(Logger& log) {
  const auto disks = DetectVirtioDiskNumbers(log);
  if (disks.empty()) return;

  const DWORD disk_number = *disks.begin();
  HANDLE pd = OpenPhysicalDriveForIoctl(log, disk_number);
  if (pd == INVALID_HANDLE_VALUE) return;

  const auto before = QueryAerovblkCapacityChangeEvents(log, pd);
  const auto after = QueryAerovblkCapacityChangeEvents(log, pd);

  if (before.has_value()) {
    log.Logf("virtio-blk-resize: capacity_change_events_before=%I64u",
             static_cast<unsigned long long>(*before));
  } else {
    log.LogLine("virtio-blk-resize: capacity_change_events_before=not_supported");
  }

  if (after.has_value()) {
    log.Logf("virtio-blk-resize: capacity_change_events_after=%I64u",
             static_cast<unsigned long long>(*after));
  } else {
    log.LogLine("virtio-blk-resize: capacity_change_events_after=not_supported");
  }

  CloseHandle(pd);
}

struct VirtioBlkResizeTestResult {
  bool ok = false;
  std::string reason;
  DWORD disk_number = 0;
  uint64_t old_bytes = 0;
  uint64_t new_bytes = 0;
  uint64_t last_bytes = 0;
  DWORD win32_error = ERROR_SUCCESS;
  uint32_t elapsed_ms = 0;
};

static bool QueryDiskLengthBytes(HANDLE hPhysicalDrive, uint64_t* out_bytes, DWORD* out_err) {
  if (out_err) *out_err = ERROR_SUCCESS;
  if (!out_bytes) return false;
  *out_bytes = 0;
  if (hPhysicalDrive == INVALID_HANDLE_VALUE) return false;

  GET_LENGTH_INFORMATION len{};
  DWORD bytes = 0;
  if (DeviceIoControl(hPhysicalDrive, IOCTL_DISK_GET_LENGTH_INFO, nullptr, 0, &len, sizeof(len), &bytes,
                      nullptr) &&
      bytes >= sizeof(len)) {
    *out_bytes = static_cast<uint64_t>(len.Length);
    return true;
  }
  DWORD err = GetLastError();

  // Fallback: some stacks may not support IOCTL_DISK_GET_LENGTH_INFO. GeometryEx includes DiskSize.
  DISK_GEOMETRY_EX geom{};
  bytes = 0;
  if (DeviceIoControl(hPhysicalDrive, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, nullptr, 0, &geom, sizeof(geom), &bytes,
                      nullptr) &&
      bytes >= offsetof(DISK_GEOMETRY_EX, Data)) {
    *out_bytes = static_cast<uint64_t>(geom.DiskSize.QuadPart);
    return true;
  }

  if (out_err) *out_err = GetLastError() ? GetLastError() : err;
  return false;
}

static VirtioBlkResizeTestResult VirtioBlkResizeTest(Logger& log, const Options& opt) {
  VirtioBlkResizeTestResult out{};

  const auto sel = SelectVirtioBlkSelection(log, opt);
  if (!sel.has_value()) {
    out.reason = "disk_not_found";
    out.win32_error = ERROR_NOT_FOUND;
    return out;
  }
  out.disk_number = sel->disk_number;

  HANDLE pd = OpenPhysicalDriveForIoctl(log, out.disk_number);
  if (pd == INVALID_HANDLE_VALUE) {
    out.reason = "open_physical_drive_failed";
    out.win32_error = GetLastError();
    return out;
  }

  uint64_t old_bytes = 0;
  DWORD err = ERROR_SUCCESS;
  if (!QueryDiskLengthBytes(pd, &old_bytes, &err)) {
    out.reason = "query_old_size_failed";
    out.win32_error = err;
    CloseHandle(pd);
    return out;
  }
  if (old_bytes == 0) {
    out.reason = "invalid_old_size";
    out.win32_error = ERROR_INVALID_DATA;
    CloseHandle(pd);
    return out;
  }

  out.old_bytes = old_bytes;
  out.last_bytes = old_bytes;

  // Signal the host harness that we are ready for it to issue a QMP block resize. The harness will
  // parse old_bytes and compute a new target size (grow only) before triggering the resize.
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=%lu|old_bytes=%I64u",
           static_cast<unsigned long>(out.disk_number), static_cast<unsigned long long>(out.old_bytes));

  PerfTimer t;
  constexpr double kTimeoutSec = 60.0;

  while (t.SecondsSinceStart() < kTimeoutSec) {
    // Best-effort: request an update so size changes are surfaced promptly.
    DWORD ignored = 0;
    (void)DeviceIoControl(pd, IOCTL_DISK_UPDATE_PROPERTIES, nullptr, 0, nullptr, 0, &ignored, nullptr);

    uint64_t cur = 0;
    err = ERROR_SUCCESS;
    if (!QueryDiskLengthBytes(pd, &cur, &err)) {
      out.reason = "query_new_size_failed";
      out.win32_error = err;
      break;
    }

    out.last_bytes = cur;
    if (cur != old_bytes) {
      if (cur > old_bytes) {
        out.ok = true;
        out.new_bytes = cur;
        out.elapsed_ms = static_cast<uint32_t>(std::round(t.SecondsSinceStart() * 1000.0));
      } else {
        out.ok = false;
        out.reason = "unexpected_shrink";
        out.win32_error = ERROR_INVALID_DATA;
      }
      break;
    }

    Sleep(250);
  }

  if (!out.ok && out.reason.empty()) {
    out.reason = "timeout";
    out.win32_error = ERROR_TIMEOUT;
  }

  CloseHandle(pd);
  return out;
}

struct VirtioInputTestResult {
  bool ok = false;
  // Best-effort devnode handle for a matching virtio-input HID device. This can be used to walk up the device tree
  // to the owning PCI function and query interrupt resources via cfgmgr32.
  DEVINST devinst = 0;
  int matched_devices = 0;
  int keyboard_devices = 0;
  int consumer_devices = 0;
  int mouse_devices = 0;
  int tablet_devices = 0;
  int ambiguous_devices = 0;
  int unknown_devices = 0;
  int keyboard_collections = 0;
  int consumer_collections = 0;
  int mouse_collections = 0;
  int tablet_collections = 0;
  // Underlying virtio-input PCI function binding validation (service name + PnP health).
  int pci_devices = 0;
  int pci_wrong_service = 0;
  int pci_missing_service = 0;
  int pci_problem = 0;
  bool pci_binding_ok = false;
  // Best-effort sample of the PCI binding state for machine-readable markers/diagnostics.
  std::string pci_binding_reason;
  std::wstring pci_sample_pnp_id;
  std::wstring pci_sample_service;
  std::wstring pci_sample_hwid0;
  DWORD pci_sample_cm_problem = 0;
  ULONG pci_sample_cm_status = 0;
  // Best-effort: capture at least one interface path for each virtio-input HID class device so optional
  // end-to-end input report tests can open them.
  std::wstring keyboard_device_path;
  std::wstring consumer_device_path;
  std::wstring mouse_device_path;
  std::wstring tablet_device_path;
  std::string reason;
};

static constexpr const wchar_t* kVirtioInputExpectedService = L"aero_virtio_input";

struct VirtioInputPciDevice {
  DEVINST devinst = 0;
  std::wstring instance_id;
  std::wstring description;
  std::vector<std::wstring> hwids;
  std::wstring service;
  DWORD cm_problem = 0;
  ULONG cm_status = 0;
  bool is_modern = false;
  bool is_transitional = false;
};

static std::vector<VirtioInputPciDevice> DetectVirtioInputPciDevices(Logger& log, bool verbose = true) {
  std::vector<VirtioInputPciDevice> out;

  HDEVINFO devinfo = SetupDiGetClassDevsW(nullptr, L"PCI", nullptr, DIGCF_PRESENT | DIGCF_ALLCLASSES);
  if (devinfo == INVALID_HANDLE_VALUE) {
    if (verbose) {
      log.Logf("virtio-input-binding: SetupDiGetClassDevs(enumerator=PCI) failed: %lu", GetLastError());
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
    bool modern = false;
    bool transitional = false;
    for (const auto& id : hwids) {
      if (ContainsInsensitive(id, L"PCI\\VEN_1AF4&DEV_1052")) modern = true;
      if (ContainsInsensitive(id, L"PCI\\VEN_1AF4&DEV_1011")) transitional = true;
    }
    if (!modern && !transitional) continue;

    VirtioInputPciDevice pci{};
    pci.devinst = dev.DevInst;
    pci.hwids = hwids;
    pci.is_modern = modern;
    pci.is_transitional = transitional;

    if (auto inst = GetDeviceInstanceIdString(devinfo, &dev)) {
      pci.instance_id = *inst;
    }
    if (auto friendly = GetDevicePropertyString(devinfo, &dev, SPDRP_FRIENDLYNAME)) {
      pci.description = *friendly;
    } else if (auto desc = GetDevicePropertyString(devinfo, &dev, SPDRP_DEVICEDESC)) {
      pci.description = *desc;
    }
    if (auto svc = GetDevicePropertyString(devinfo, &dev, SPDRP_SERVICE)) {
      pci.service = *svc;
    }

    ULONG status = 0;
    ULONG problem = 0;
    const CONFIGRET cr = CM_Get_DevNode_Status(&status, &problem, dev.DevInst, 0);
    if (cr == CR_SUCCESS) {
      pci.cm_status = status;
      pci.cm_problem = static_cast<DWORD>(problem);
    } else {
      pci.cm_status = 0;
      pci.cm_problem = MAXDWORD;
      if (verbose) {
        log.Logf("virtio-input-binding: CM_Get_DevNode_Status failed pnp_id=%s cr=%lu",
                 WideToUtf8(pci.instance_id).c_str(), static_cast<unsigned long>(cr));
      }
    }

    if (verbose) {
      log.Logf("virtio-input-binding: detected PCI device instance_id=%s name=%s modern=%d transitional=%d service=%s",
               WideToUtf8(pci.instance_id).c_str(), WideToUtf8(pci.description).c_str(), modern ? 1 : 0,
               transitional ? 1 : 0, pci.service.empty() ? "<missing>" : WideToUtf8(pci.service).c_str());
      if (!hwids.empty()) {
        log.Logf("virtio-input-binding: detected PCI device hwid0=%s", WideToUtf8(hwids[0]).c_str());
      }
    }

    out.push_back(std::move(pci));
  }

  SetupDiDestroyDeviceInfoList(devinfo);
  return out;
}

struct VirtioInputBindingSample {
  std::wstring pnp_id;
  std::wstring service;
  std::wstring hwid0;
  DWORD cm_problem = 0;
  ULONG cm_status = 0;
};

struct VirtioInputBindingCheckResult {
  bool ok = false;
  int devices = 0;
  int wrong_service = 0;
  int missing_service = 0;
  int problem = 0;
  VirtioInputBindingSample ok_sample;
  VirtioInputBindingSample wrong_service_sample;
  VirtioInputBindingSample missing_service_sample;
  VirtioInputBindingSample problem_sample;
};

static VirtioInputBindingCheckResult SummarizeVirtioInputPciBinding(const std::vector<VirtioInputPciDevice>& devices) {
  VirtioInputBindingCheckResult out;
  out.devices = static_cast<int>(devices.size());
  auto set_sample_if_empty = [&](VirtioInputBindingSample& sample, const VirtioInputPciDevice& dev) {
    if (!sample.pnp_id.empty()) return;
    sample.pnp_id = dev.instance_id;
    sample.service = dev.service;
    if (!dev.hwids.empty()) sample.hwid0 = dev.hwids[0];
    sample.cm_problem = dev.cm_problem;
    sample.cm_status = dev.cm_status;
  };

  for (const auto& dev : devices) {
    const bool has_service = !dev.service.empty();
    const bool service_ok = has_service && EqualsInsensitive(dev.service, kVirtioInputExpectedService);
    const bool problem_ok = (dev.cm_problem == 0) && ((dev.cm_status & DN_HAS_PROBLEM) == 0);
    if (!has_service) {
      out.missing_service++;
      set_sample_if_empty(out.missing_service_sample, dev);
    } else if (!service_ok) {
      out.wrong_service++;
      set_sample_if_empty(out.wrong_service_sample, dev);
    }
    if (!problem_ok) {
      out.problem++;
      set_sample_if_empty(out.problem_sample, dev);
    }
    if (service_ok && problem_ok) {
      out.ok = true;
      set_sample_if_empty(out.ok_sample, dev);
    }
  }

  return out;
}

static VirtioInputBindingCheckResult CheckVirtioInputPciBinding(Logger& log,
                                                                const std::vector<VirtioInputPciDevice>& devices) {
  VirtioInputBindingCheckResult out = SummarizeVirtioInputPciBinding(devices);

  for (const auto& dev : devices) {
    const bool has_service = !dev.service.empty();
    const bool service_ok = has_service && EqualsInsensitive(dev.service, kVirtioInputExpectedService);
    const bool problem_ok = (dev.cm_problem == 0) && ((dev.cm_status & DN_HAS_PROBLEM) == 0);

    bool has_rev01 = false;
    if (dev.is_modern) {
      for (const auto& id : dev.hwids) {
        if (ContainsInsensitive(id, L"&REV_01")) {
          has_rev01 = true;
          break;
        }
      }
    }

    if (dev.is_modern && !has_rev01) {
      log.Logf(
        "virtio-input-binding: pci device pnp_id=%s missing REV_01 (Aero contract v1 expects REV_01; QEMU needs x-pci-revision=0x01)",
          WideToUtf8(dev.instance_id).c_str());
    }
    if (!has_service) {
      log.Logf("virtio-input-binding: pci device pnp_id=%s has no bound service (expected %s)",
               WideToUtf8(dev.instance_id).c_str(), WideToUtf8(kVirtioInputExpectedService).c_str());
    } else if (!service_ok) {
      log.Logf("virtio-input-binding: pci device pnp_id=%s bound_service=%s (expected %s)",
               WideToUtf8(dev.instance_id).c_str(), WideToUtf8(dev.service).c_str(),
               WideToUtf8(kVirtioInputExpectedService).c_str());
    }
    if (!problem_ok) {
      log.Logf("virtio-input-binding: pci device pnp_id=%s has ConfigManagerErrorCode=%lu (%s: %s)",
               WideToUtf8(dev.instance_id).c_str(), static_cast<unsigned long>(dev.cm_problem),
               CmProblemCodeToName(dev.cm_problem), CmProblemCodeToMeaning(dev.cm_problem));
    }
  }

  if (!out.ok) {
    log.LogLine("virtio-input-binding: no virtio-input PCI device is healthy and bound to the expected driver");
    log.LogLine("virtio-input-binding: troubleshooting hints:");
    log.LogLine("virtio-input-binding: - check Device Manager for Code 28/52/10 and inspect setupapi.dev.log");
    log.LogLine(
        "virtio-input-binding: - for QEMU contract v1: use disable-legacy=on,x-pci-revision=0x01 and install aero_virtio_input.inf");
  }

  return out;
}

static bool IsVirtioInputHardwareId(const std::vector<std::wstring>& hwids) {
  for (const auto& id : hwids) {
    if (ContainsInsensitive(id, L"VEN_1AF4&DEV_1052")) return true;
    if (ContainsInsensitive(id, L"VEN_1AF4&DEV_1011")) return true;
    // Some stacks may expose HID-style IDs (VID/PID) instead of PCI-style VEN/DEV.
    // The in-tree Aero virtio-input HID minidriver uses:
    //   - Keyboard: VID_1AF4&PID_0001
    //   - Mouse:    VID_1AF4&PID_0002
    //   - Tablet:   VID_1AF4&PID_0003
    if (ContainsInsensitive(id, L"VID_1AF4&PID_0001")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_0002")) return true;
    if (ContainsInsensitive(id, L"VID_1AF4&PID_0003")) return true;
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
         ContainsInsensitive(device_path, L"VID_1AF4&PID_0003") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_1052") ||
         ContainsInsensitive(device_path, L"VID_1AF4&PID_1011");
}

static bool HasHidCollectionToken(const std::wstring& device_path,
                                  const std::vector<std::wstring>& hwids,
                                  const wchar_t* token) {
  if (token && *token) {
    if (ContainsInsensitive(device_path, token)) return true;
    for (const auto& id : hwids) {
      if (ContainsInsensitive(id, token)) return true;
    }
  }
  return false;
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

static const char* VirtioInputInterruptModeToString(ULONG mode) {
  switch (mode) {
    case VioInputInterruptModeIntx:
      return "intx";
    case VioInputInterruptModeMsix:
      return "msix";
    default:
      return "unknown";
  }
}

static const char* VirtioInputInterruptMappingToString(ULONG mapping) {
  switch (mapping) {
    case VioInputInterruptMappingAllOnVector0:
      return "all-on-vector0";
    case VioInputInterruptMappingPerQueue:
      return "per-queue";
    default:
      return "unknown";
  }
}

static std::optional<VIOINPUT_INTERRUPT_INFO> QueryVirtioInputInterruptInfo(Logger& log,
                                                                            const std::wstring& device_path,
                                                                            DWORD* win32_error_out) {
  if (win32_error_out) *win32_error_out = ERROR_SUCCESS;
  if (device_path.empty()) return std::nullopt;

  HANDLE h = OpenHidDeviceForIoctl(device_path.c_str());
  if (h == INVALID_HANDLE_VALUE) {
    const DWORD err = GetLastError();
    if (win32_error_out) *win32_error_out = err;
    log.Logf("virtio-input-msix: CreateFile(%s) failed err=%lu", WideToUtf8(device_path).c_str(),
             static_cast<unsigned long>(err));
    return std::nullopt;
  }

  VIOINPUT_INTERRUPT_INFO info{};
  DWORD bytes = 0;
  const BOOL ok =
      DeviceIoControl(h, IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO, nullptr, 0, &info, sizeof(info), &bytes, nullptr);
  const DWORD err = ok ? ERROR_SUCCESS : GetLastError();
  CloseHandle(h);

  if (!ok) {
    if (win32_error_out) *win32_error_out = err;
    log.Logf("virtio-input-msix: IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO failed err=%lu", static_cast<unsigned long>(err));
    return std::nullopt;
  }

  if (bytes < sizeof(info.Size) + sizeof(info.Version)) {
    log.Logf("virtio-input-msix: IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO returned too few bytes=%lu",
             static_cast<unsigned long>(bytes));
    if (win32_error_out) *win32_error_out = ERROR_INSUFFICIENT_BUFFER;
    return std::nullopt;
  }

  // Best-effort validation: tolerate older/newer versions by trusting the returned size.
  if (info.Size != 0 && info.Size < sizeof(info.Size) + sizeof(info.Version)) {
    log.Logf("virtio-input-msix: IOCTL_VIOINPUT_QUERY_INTERRUPT_INFO returned invalid Size=%lu",
             static_cast<unsigned long>(info.Size));
  }

  return info;
}

struct HidReportDescriptorSummary {
  int keyboard_app_collections = 0;
  int mouse_app_collections = 0;
  int consumer_app_collections = 0;
  int tablet_app_collections = 0;
  // For mouse/pointer (Generic Desktop Page) application collections, count how many contain X/Y inputs
  // marked as Relative vs Absolute. This is used to distinguish relative mice from absolute pointer/tablet
  // devices when multiple virtio-input pointing devices are present.
  int mouse_xy_relative_collections = 0;
  int mouse_xy_absolute_collections = 0;
};

static HidReportDescriptorSummary SummarizeHidReportDescriptor(const std::vector<uint8_t>& desc) {
  HidReportDescriptorSummary out{};

  uint32_t usage_page = 0;
  std::vector<uint32_t> usage_page_stack;
  std::vector<uint32_t> local_usages;
  std::optional<uint32_t> local_usage_min;
  std::optional<uint32_t> local_usage_max;

  // Track collections so we can classify "Mouse" vs "absolute pointer" (tablet-like) devices.
  struct CollectionCtx {
    bool is_application = false;
    uint32_t usage_page = 0;
    uint32_t usage = 0;

    enum class Kind {
      Unknown,
      Keyboard,
      MouseOrPointer,
      Tablet,
    } kind = Kind::Unknown;

    // Only used for Kind::MouseOrPointer to detect absolute/relative X/Y.
    bool saw_x_abs = false;
    bool saw_y_abs = false;
    bool saw_x_rel = false;
    bool saw_y_rel = false;
  };
  std::vector<CollectionCtx> collection_stack;

  auto clear_locals = [&]() {
    local_usages.clear();
    local_usage_min.reset();
    local_usage_max.reset();
  };

  auto local_usage_includes = [&](uint32_t u) -> bool {
    if (std::find(local_usages.begin(), local_usages.end(), u) != local_usages.end()) return true;
    if (local_usage_min.has_value() && local_usage_max.has_value()) {
      return *local_usage_min <= u && u <= *local_usage_max;
    }
    if (local_usage_min.has_value() && !local_usage_max.has_value()) {
      // Best-effort: some descriptors use a single Usage Minimum without a matching maximum.
      return *local_usage_min == u;
    }
    return false;
  };

  auto finalize_collection = [&](const CollectionCtx& ctx) {
    if (!ctx.is_application) return;
    // Consumer Page (0x0C): Consumer Control (0x01)
    if (ctx.usage_page == 0x0C && ctx.usage == 0x01) {
      out.consumer_app_collections++;
      return;
    }
    switch (ctx.kind) {
      case CollectionCtx::Kind::Keyboard:
        out.keyboard_app_collections++;
        break;
      case CollectionCtx::Kind::Tablet:
        out.tablet_app_collections++;
        break;
      case CollectionCtx::Kind::MouseOrPointer: {
        const bool has_rel_xy = (ctx.saw_x_rel && ctx.saw_y_rel);
        const bool abs_only = (ctx.saw_x_abs && ctx.saw_y_abs) && !(ctx.saw_x_rel || ctx.saw_y_rel);

        if (has_rel_xy) out.mouse_xy_relative_collections++;
        if (abs_only) out.mouse_xy_absolute_collections++;

        // Treat "absolute-only" Generic Desktop pointer devices as tablet-like, so we don't misclassify
        // them as relative mice.
        if (abs_only) {
          out.tablet_app_collections++;
        } else {
          out.mouse_app_collections++;
        }
        break;
      }
      case CollectionCtx::Kind::Unknown:
      default:
        break;
    }
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
        // Collection (tag 0xA), End Collection (0xC), Input (0x8)
        if (tag == 0xA) {
          const uint8_t collection_type = static_cast<uint8_t>(value & 0xFF);

          std::optional<uint32_t> usage;
          if (!local_usages.empty()) {
            usage = local_usages.front();
          } else if (local_usage_min.has_value()) {
            usage = *local_usage_min;
          }

          CollectionCtx ctx{};
          ctx.is_application = (collection_type == 0x01);
          ctx.usage_page = usage_page;
          ctx.usage = usage.value_or(0);

          if (ctx.is_application) {
            // Generic Desktop Page (0x01): Keyboard (0x06), Mouse (0x02), Pointer (0x01)
            if (ctx.usage_page == 0x01 && ctx.usage == 0x06) {
              ctx.kind = CollectionCtx::Kind::Keyboard;
            } else if (ctx.usage_page == 0x01 && (ctx.usage == 0x02 || ctx.usage == 0x01)) {
              ctx.kind = CollectionCtx::Kind::MouseOrPointer;
            } else if (ctx.usage_page == 0x0D) {
              // Digitizers (0x0D): treat as "tablet-like".
              ctx.kind = CollectionCtx::Kind::Tablet;
            }
          }

          collection_stack.push_back(ctx);
        } else if (tag == 0xC) {
          if (!collection_stack.empty()) {
            const CollectionCtx ctx = collection_stack.back();
            collection_stack.pop_back();
            finalize_collection(ctx);
          }
        } else if (tag == 0x8) {
          // HID "Input" item flags:
          //   bit0: Data(0) / Constant(1)
          //   bit1: Array(0) / Variable(1)
          //   bit2: Absolute(0) / Relative(1)
          //
          // For pointer axis classification we only consider the common X/Y fields which are
          // expected to be Input(Data,Var,Abs/Rel). Ignore Constant/Array inputs to avoid
          // misclassifying unrelated fields.
          const bool is_data = (value & 0x01u) == 0;
          const bool is_var = (value & 0x02u) != 0;
          const bool is_relative = (value & 0x04u) != 0;
          if (is_data && is_var) {
            // Detect X/Y (Generic Desktop page).
            const bool has_x = (usage_page == 0x01) && local_usage_includes(0x30); // X
            const bool has_y = (usage_page == 0x01) && local_usage_includes(0x31); // Y

            if (has_x || has_y) {
              // Attribute the axis flags to the nearest enclosing Application collection.
              for (auto it = collection_stack.rbegin(); it != collection_stack.rend(); ++it) {
                if (!it->is_application) continue;
                if (it->kind != CollectionCtx::Kind::MouseOrPointer) break;

                if (has_x) {
                  if (is_relative) {
                    it->saw_x_rel = true;
                  } else {
                    it->saw_x_abs = true;
                  }
                }
                if (has_y) {
                  if (is_relative) {
                    it->saw_y_rel = true;
                  } else {
                    it->saw_y_abs = true;
                  }
                }
                break;
              }
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
        } else if (tag == 0x2) { // Usage Maximum
          local_usage_max = value;
        }
        break;
      }
      default:
        break;
    }
  }

  // Best-effort: close any unterminated collections so we still compute classification.
  while (!collection_stack.empty()) {
    const CollectionCtx ctx = collection_stack.back();
    collection_stack.pop_back();
    finalize_collection(ctx);
  }

  return out;
}

static VirtioInputTestResult VirtioInputTest(Logger& log) {
  VirtioInputTestResult out{};

  // Validate that virtio-input PCI devices are present, healthy, and bound to the expected Aero driver service.
  // This prevents false PASS when a different virtio-input stack (e.g. virtio-win `vioinput`) is installed.
  auto pci_devices = DetectVirtioInputPciDevices(log);
  if (pci_devices.empty()) {
    // Like virtio-snd, the scheduled task can run before PnP fully enumerates PCI devices.
    // Give virtio-input a short grace period so we don't report spurious failures due to early boot timing.
    const DWORD deadline_ms = GetTickCount() + 10000;
    int attempt = 0;
    while (pci_devices.empty() && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
      attempt++;
      Sleep(250);
      pci_devices = DetectVirtioInputPciDevices(log, false);
    }
    if (!pci_devices.empty()) {
      log.Logf("virtio-input-binding: pci device detected after wait (attempt=%d)", attempt);
      // Re-run once with verbose logging for baseline device info.
      pci_devices = DetectVirtioInputPciDevices(log);
    }
  }

  auto pci_binding = SummarizeVirtioInputPciBinding(pci_devices);
  if (!pci_binding.ok && pci_binding.wrong_service == 0 && !pci_devices.empty()) {
    // Allow a short grace period for PnP to bind the driver service (common early boot race).
    const DWORD deadline_ms = GetTickCount() + 10000;
    int attempt = 0;
    while (!pci_binding.ok && pci_binding.wrong_service == 0 && static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
      attempt++;
      Sleep(250);
      pci_devices = DetectVirtioInputPciDevices(log, false);
      pci_binding = SummarizeVirtioInputPciBinding(pci_devices);
      if (pci_binding.ok) {
        log.Logf("virtio-input-binding: pci binding became healthy after wait (attempt=%d)", attempt);
        break;
      }
    }

    if (!pci_binding.ok) {
      // Re-run with logging enabled for actionable diagnostics.
      pci_devices = DetectVirtioInputPciDevices(log);
      pci_binding = CheckVirtioInputPciBinding(log, pci_devices);
    }
  } else if (!pci_binding.ok) {
    // Wrong service / other failures: emit diagnostics immediately (no wait).
    pci_binding = CheckVirtioInputPciBinding(log, pci_devices);
  }

  out.pci_devices = pci_binding.devices;
  out.pci_wrong_service = pci_binding.wrong_service;
  out.pci_missing_service = pci_binding.missing_service;
  out.pci_problem = pci_binding.problem;
  out.pci_binding_ok = pci_binding.ok;

  if (out.pci_devices <= 0) {
    out.reason = "pci_device_missing";
    log.LogLine("virtio-input: no virtio-input PCI device detected (PCI\\VEN_1AF4&DEV_1052 or PCI\\VEN_1AF4&DEV_1011)");
    return out;
  }

  // Capture a single sample for machine markers.
  const VirtioInputBindingSample* sample = nullptr;
  if (pci_binding.ok) {
    sample = &pci_binding.ok_sample;
  } else if (pci_binding.devices == 0) {
    out.pci_binding_reason = "device_missing";
  } else if (pci_binding.wrong_service > 0) {
    out.pci_binding_reason = "wrong_service";
    sample = &pci_binding.wrong_service_sample;
  } else if (pci_binding.missing_service > 0) {
    out.pci_binding_reason = "driver_not_bound";
    sample = &pci_binding.missing_service_sample;
  } else if (pci_binding.problem > 0) {
    out.pci_binding_reason = "device_error";
    sample = &pci_binding.problem_sample;
  } else {
    out.pci_binding_reason = "driver_not_bound";
    sample = &pci_binding.missing_service_sample;
  }

  if (sample) {
    out.pci_sample_pnp_id = sample->pnp_id;
    out.pci_sample_service = sample->service;
    out.pci_sample_hwid0 = sample->hwid0;
    out.pci_sample_cm_problem = sample->cm_problem;
    out.pci_sample_cm_status = sample->cm_status;
  }

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
    if (out.devinst == 0) out.devinst = dev.DevInst;

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
    const bool has_mouse = summary.mouse_xy_relative_collections > 0;
    const bool has_tablet = summary.tablet_app_collections > 0;
    const bool is_consumer_collection = HasHidCollectionToken(device_path, hwids, L"col02");
    const bool has_unclassified_mouse_collections =
        summary.mouse_app_collections > summary.mouse_xy_relative_collections;
    const int kind_count = (has_keyboard ? 1 : 0) + (has_mouse ? 1 : 0) + (has_tablet ? 1 : 0);

    if (has_unclassified_mouse_collections) {
      out.unknown_devices++;
    } else if (kind_count > 1) {
      out.ambiguous_devices++;
    } else if (has_keyboard) {
      // Some HID stacks enumerate separate device interfaces for each top-level collection (e.g. keyboard COL01,
      // consumer control COL02). Prefer using the collection token when available so tests open the right interface.
      if (is_consumer_collection) {
        out.consumer_devices++;
        if (out.consumer_device_path.empty()) out.consumer_device_path = device_path;
      } else {
        out.keyboard_devices++;
        if (out.keyboard_device_path.empty()) out.keyboard_device_path = device_path;
      }
    } else if (has_mouse) {
      out.mouse_devices++;
      if (out.mouse_device_path.empty()) out.mouse_device_path = device_path;
    } else if (has_tablet) {
      out.tablet_devices++;
      if (out.tablet_device_path.empty()) out.tablet_device_path = device_path;
    } else {
      out.unknown_devices++;
    }

    out.keyboard_collections += summary.keyboard_app_collections;
    out.consumer_collections += summary.consumer_app_collections;
    out.mouse_collections += summary.mouse_app_collections;
    out.tablet_collections += summary.tablet_app_collections;

    log.Logf("virtio-input: report_descriptor bytes=%zu keyboard_app_collections=%d "
             "mouse_app_collections=%d consumer_app_collections=%d tablet_app_collections=%d",
             report_desc->size(), summary.keyboard_app_collections, summary.mouse_app_collections,
             summary.consumer_app_collections, summary.tablet_app_collections);
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  if (out.matched_devices == 0) {
    out.reason = "no_matching_hid_devices";
    log.LogLine("virtio-input: no virtio-input HID devices detected");
    log.LogLine(
        "virtio-input: hint: if you're running under stock QEMU virtio-input (ID_NAME like 'QEMU Virtio Keyboard'), "
        "enable the Aero virtio-input driver's CompatIdName mode (HKLM\\System\\CurrentControlSet\\Services\\aero_virtio_input\\Parameters\\CompatIdName=1) "
        "or use an Aero contract-compliant virtio-input device model.");
    return out;
  }
  if (had_error) {
    out.reason = "ioctl_or_open_failed";
    return out;
  }
  if (out.keyboard_devices <= 0) {
    out.reason = "missing_keyboard_device";
    log.LogLine(
        "virtio-input: hint: keyboard HID collection missing. On stock QEMU, ensure virtio-input CompatIdName mode is enabled "
        "(HKLM\\System\\CurrentControlSet\\Services\\aero_virtio_input\\Parameters\\CompatIdName=1).");
    return out;
  }
  if (out.mouse_devices <= 0) {
    out.reason = "missing_mouse_device";
    log.LogLine(
        "virtio-input: hint: mouse HID collection missing. On stock QEMU, ensure virtio-input CompatIdName mode is enabled "
        "(HKLM\\System\\CurrentControlSet\\Services\\aero_virtio_input\\Parameters\\CompatIdName=1).");
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
  // Only enforce PCI binding expectations when we actually found virtio-input PCI functions. When no PCI devices are
  // detected (e.g. early boot race), the dedicated virtio-input PCI binding marker provides additional diagnostics.
  if (!out.pci_binding_ok && out.pci_devices > 0) {
    out.ok = false;
    if (!out.pci_binding_reason.empty()) {
      out.reason = out.pci_binding_reason;
    } else if (out.pci_wrong_service > 0) {
      out.reason = "wrong_service";
    } else if (out.pci_missing_service > 0) {
      out.reason = "driver_not_bound";
    } else if (out.pci_problem > 0) {
      out.reason = "device_error";
    } else {
      out.reason = "driver_not_bound";
    }
  }
  return out;
}

struct VirtioInputEventsTestResult {
  bool ok = false;
  bool modifiers_ok = false;
  bool buttons_ok = false;
  bool wheel_ok = false;

  bool saw_key_a_down = false;
  bool saw_key_a_up = false;
  bool saw_mouse_move = false;
  bool saw_mouse_left_down = false;
  bool saw_mouse_left_up = false;

  // Modifier / extra keyboard coverage.
  bool saw_shift_b = false;
  bool saw_ctrl_down = false;
  bool saw_ctrl_up = false;
  bool saw_alt_down = false;
  bool saw_alt_up = false;
  bool saw_f1_down = false;
  bool saw_f1_up = false;

  // Extra mouse buttons / wheel.
  bool saw_mouse_side_down = false;
  bool saw_mouse_side_up = false;
  bool saw_mouse_extra_down = false;
  bool saw_mouse_extra_up = false;
  bool saw_mouse_wheel = false;
  bool saw_mouse_hwheel = false;
  bool saw_mouse_wheel_expected = false;
  bool saw_mouse_hwheel_expected = false;
  bool saw_mouse_wheel_unexpected = false;
  bool saw_mouse_hwheel_unexpected = false;
  int mouse_wheel_unexpected_last = 0;
  int mouse_hwheel_unexpected_last = 0;
  int mouse_wheel_events = 0;
  int mouse_hwheel_events = 0;
  int mouse_wheel_total = 0;
  int mouse_hwheel_total = 0;
  int keyboard_reports = 0;
  int keyboard_bad_reports = 0;
  int mouse_reports = 0;
  int mouse_bad_reports = 0;
  std::string reason;
  DWORD win32_error = 0;
};

// Expected deterministic scroll deltas injected by the host harness when wheel testing is enabled.
// (The host harness may retry injection a few times, so the guest may observe multiples of these deltas.)
static constexpr int kExpectedMouseWheelDelta = 1;
static constexpr int kExpectedMouseHWheelDelta = -2;

struct VirtioInputHidPaths {
  std::wstring keyboard_path;
  std::wstring consumer_path;
  std::wstring mouse_path;
  std::string reason;
  DWORD win32_error = 0;
};

static VirtioInputHidPaths FindVirtioInputHidPaths(Logger& log) {
  // {4D1E55B2-F16F-11CF-88CB-001111000030}
  static const GUID kHidInterfaceGuid = {0x4D1E55B2,
                                         0xF16F,
                                         0x11CF,
                                         {0x88, 0xCB, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30}};

  HDEVINFO devinfo = SetupDiGetClassDevsW(&kHidInterfaceGuid, nullptr, nullptr,
                                         DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
  VirtioInputHidPaths out{};
  if (devinfo == INVALID_HANDLE_VALUE) {
    out.reason = "setupapi_classdevs_failed";
    out.win32_error = GetLastError();
    log.Logf("virtio-input-events: SetupDiGetClassDevs(GUID_DEVINTERFACE_HID) failed: %lu", out.win32_error);
    return out;
  }

  bool had_error = false;
  std::wstring absolute_pointer_path;
  std::wstring unknown_pointer_path;
  int absolute_pointer_candidates = 0;
  int unknown_pointer_candidates = 0;
  std::vector<std::wstring> relative_mouse_candidates;

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
    const bool has_consumer = summary.consumer_app_collections > 0;
    const bool has_tablet = summary.tablet_app_collections > 0;
    const bool has_relative_xy = summary.mouse_xy_relative_collections > 0;
    const bool has_absolute_xy = summary.mouse_xy_absolute_collections > 0;
    const bool is_consumer_collection = HasHidCollectionToken(device_path, hwids, L"col02");

    if (has_keyboard && !has_mouse && !has_tablet) {
      if (is_consumer_collection) {
        if (out.consumer_path.empty()) {
          out.consumer_path = device_path;
          log.Logf("virtio-input-events: selected consumer HID interface: %s", WideToUtf8(device_path).c_str());
        }
      } else if (out.keyboard_path.empty()) {
        out.keyboard_path = device_path;
        log.Logf("virtio-input-events: selected keyboard HID interface: %s", WideToUtf8(device_path).c_str());
      }
    } else if (has_consumer && !has_keyboard && !has_mouse && !has_tablet) {
      if (out.consumer_path.empty()) {
        out.consumer_path = device_path;
        log.Logf("virtio-input-events: selected consumer HID interface: %s", WideToUtf8(device_path).c_str());
      }
    } else if (!has_keyboard) {
      if (has_mouse) {
        if (has_relative_xy) {
          relative_mouse_candidates.push_back(device_path);
        } else if (has_absolute_xy || has_tablet) {
          absolute_pointer_candidates++;
          if (absolute_pointer_path.empty()) absolute_pointer_path = device_path;
          log.Logf("virtio-input-events: found absolute pointer/tablet HID interface (not a mouse): %s",
                   WideToUtf8(device_path).c_str());
        } else {
          unknown_pointer_candidates++;
          if (unknown_pointer_path.empty()) unknown_pointer_path = device_path;
          log.Logf("virtio-input-events: found mouse-like HID interface with unknown XY mode: %s",
                   WideToUtf8(device_path).c_str());
        }
      } else if (has_tablet) {
        absolute_pointer_candidates++;
        if (absolute_pointer_path.empty()) absolute_pointer_path = device_path;
        log.Logf("virtio-input-events: found tablet HID interface (not a mouse): %s",
                 WideToUtf8(device_path).c_str());
      }
    }
  }

  SetupDiDestroyDeviceInfoList(devinfo);

  if (!relative_mouse_candidates.empty()) {
    std::sort(relative_mouse_candidates.begin(), relative_mouse_candidates.end(), LessInsensitive);
    out.mouse_path = relative_mouse_candidates.front();
    log.Logf("virtio-input-events: selected relative mouse HID interface: %s", WideToUtf8(out.mouse_path).c_str());
  }

  if (out.keyboard_path.empty()) {
    if (had_error) {
      out.reason = "ioctl_or_open_failed";
      // Best-effort: `ReadHidReportDescriptor` already logs details, so this is informational only.
      return out;
    }
    out.reason = "missing_keyboard_device";
    return out;
  }
  if (out.mouse_path.empty()) {
    if (!absolute_pointer_path.empty()) {
      out.reason = "no_relative_mouse_device_found_only_absolute_pointer";
      out.win32_error = ERROR_NOT_FOUND;
      log.Logf(
          "virtio-input-events: no relative mouse HID interface found (absolute_pointer_candidates=%d); "
          "example_absolute_pointer=%s",
          absolute_pointer_candidates, WideToUtf8(absolute_pointer_path).c_str());
    } else if (!unknown_pointer_path.empty()) {
      out.reason = "no_relative_mouse_device_found_unknown_pointer";
      out.win32_error = ERROR_NOT_FOUND;
      log.Logf(
          "virtio-input-events: no relative mouse HID interface found (unknown_pointer_candidates=%d); "
          "example_unknown_pointer=%s",
          unknown_pointer_candidates, WideToUtf8(unknown_pointer_path).c_str());
    } else if (had_error) {
      out.reason = "ioctl_or_open_failed";
      // Best-effort: `ReadHidReportDescriptor` already logs details, so this is informational only.
    } else {
      out.reason = "missing_mouse_device";
    }
    return out;
  }

  // If we successfully selected both interfaces, ignore errors from unrelated devices. This keeps the
  // end-to-end input events test robust in environments with multiple virtio-input devices.
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

static bool ContainsKeyUsage(const uint8_t* keys, size_t len, uint8_t usage) {
  if (!keys) return false;
  for (size_t i = 0; i < len; i++) {
    if (keys[i] == usage) return true;
  }
  return false;
}

static bool VirtioInputEventsBaseOk(const VirtioInputEventsTestResult& out) {
  return out.saw_key_a_down && out.saw_key_a_up && out.saw_mouse_move && out.saw_mouse_left_down &&
         out.saw_mouse_left_up;
}

static bool VirtioInputEventsModifiersOk(const VirtioInputEventsTestResult& out) {
  return out.saw_shift_b && out.saw_ctrl_down && out.saw_ctrl_up && out.saw_alt_down && out.saw_alt_up &&
         out.saw_f1_down && out.saw_f1_up;
}

static bool VirtioInputEventsButtonsOk(const VirtioInputEventsTestResult& out) {
  return out.saw_mouse_side_down && out.saw_mouse_side_up && out.saw_mouse_extra_down &&
         out.saw_mouse_extra_up;
}

static bool VirtioInputEventsWheelOk(const VirtioInputEventsTestResult& out) {
  // The host harness may retry QMP injection multiple times after the guest reports READY (to reduce
  // timing flakiness when no user-mode `ReadFile` is pending). Accept multiples of the expected
  // deltas, but require that:
  // - at least one expected delta was observed for each axis, and
  // - no unexpected deltas were observed.
  return out.saw_mouse_wheel && out.saw_mouse_hwheel && out.saw_mouse_wheel_expected &&
         out.saw_mouse_hwheel_expected && !out.saw_mouse_wheel_unexpected && !out.saw_mouse_hwheel_unexpected;
}

static void ProcessKeyboardReport(VirtioInputEventsTestResult& out, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  // virtio-input keyboard input report is typically 9 bytes with ReportID=1:
  //   [1][mod][res][k1..k6]
  if (len < 9 || buf[0] != 1) {
    out.keyboard_bad_reports++;
    return;
  }

  const uint8_t modifiers = buf[1];
  const uint8_t* keys = buf + 3;
  const size_t key_count = 6;

  constexpr uint8_t kUsageA = 0x04;
  constexpr uint8_t kUsageB = 0x05;
  constexpr uint8_t kUsageF1 = 0x3A;

  constexpr uint8_t kModCtrl = 0x01 | 0x10;
  constexpr uint8_t kModShift = 0x02 | 0x20;
  constexpr uint8_t kModAlt = 0x04 | 0x40;

  const bool has_a = ContainsKeyUsage(keys, key_count, kUsageA);
  const bool has_b = ContainsKeyUsage(keys, key_count, kUsageB);
  const bool has_f1 = ContainsKeyUsage(keys, key_count, kUsageF1);

  // Base: 'a' down/up.
  if (has_a) out.saw_key_a_down = true;
  if (out.saw_key_a_down && !has_a) out.saw_key_a_up = true;

  // Extended: Shift + 'b'.
  if ((modifiers & kModShift) != 0 && has_b) {
    out.saw_shift_b = true;
  }

  // Extended: Ctrl down/up.
  if ((modifiers & kModCtrl) != 0) out.saw_ctrl_down = true;
  if (out.saw_ctrl_down && (modifiers & kModCtrl) == 0) out.saw_ctrl_up = true;

  // Extended: Alt down/up.
  if ((modifiers & kModAlt) != 0) out.saw_alt_down = true;
  if (out.saw_alt_down && (modifiers & kModAlt) == 0) out.saw_alt_up = true;

  // Extended: F1 down/up.
  if (has_f1) out.saw_f1_down = true;
  if (out.saw_f1_down && !has_f1) out.saw_f1_up = true;
}

static void ProcessMouseReport(VirtioInputEventsTestResult& out, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  size_t off = 0;
  // virtio-input mouse input report is typically 6 bytes with ReportID=2:
  //   [2][buttons][dx][dy][wheel][pan]
  //
  // Some variants omit the report ID prefix (so the report begins with [buttons]).
  // Avoid mis-detecting a report ID when the first byte is simply a button bitmask by
  // requiring the longer expected size when checking buf[0]==2.
  if (len >= 6 && buf[0] == 2) off = 1;
  if (len < off + 3) {
    out.mouse_bad_reports++;
    return;
  }

  const uint8_t buttons = buf[off + 0];
  const int8_t dx = static_cast<int8_t>(buf[off + 1]);
  const int8_t dy = static_cast<int8_t>(buf[off + 2]);

  const int8_t wheel = (len >= off + 4) ? static_cast<int8_t>(buf[off + 3]) : 0;
  const int8_t pan = (len >= off + 5) ? static_cast<int8_t>(buf[off + 4]) : 0;

  if (dx != 0 || dy != 0) out.saw_mouse_move = true;

  if (wheel != 0) {
    out.saw_mouse_wheel = true;
    out.mouse_wheel_total += wheel;
    out.mouse_wheel_events++;
    if (wheel == kExpectedMouseWheelDelta) {
      out.saw_mouse_wheel_expected = true;
    } else {
      out.saw_mouse_wheel_unexpected = true;
      out.mouse_wheel_unexpected_last = wheel;
    }
  }
  if (pan != 0) {
    out.saw_mouse_hwheel = true;
    out.mouse_hwheel_total += pan;
    out.mouse_hwheel_events++;
    if (pan == kExpectedMouseHWheelDelta) {
      out.saw_mouse_hwheel_expected = true;
    } else {
      out.saw_mouse_hwheel_unexpected = true;
      out.mouse_hwheel_unexpected_last = pan;
    }
  }

  const bool left = (buttons & 0x01) != 0;
  if (left) out.saw_mouse_left_down = true;
  if (out.saw_mouse_left_down && !left) out.saw_mouse_left_up = true;

  // Boot mouse: buttons are bit-indexed (Button 1..N). QEMU maps "side"/"extra" to
  // button4/button5 on most backends.
  const bool side = (buttons & 0x08) != 0;
  if (side) out.saw_mouse_side_down = true;
  if (out.saw_mouse_side_down && !side) out.saw_mouse_side_up = true;

  const bool extra = (buttons & 0x10) != 0;
  if (extra) out.saw_mouse_extra_down = true;
  if (out.saw_mouse_extra_down && !extra) out.saw_mouse_extra_up = true;
}

static VirtioInputEventsTestResult VirtioInputEventsTest(Logger& log, const VirtioInputTestResult& input,
                                                         bool want_modifiers, bool want_buttons, bool want_wheel) {
  VirtioInputEventsTestResult out{};

  std::wstring keyboard_path = input.keyboard_device_path;
  std::wstring mouse_path = input.mouse_device_path;

  // If multiple virtio-input mice are present, do not rely on the first path recorded during the
  // enumeration probe (SetupDi enumeration order can vary). Instead, force a fresh selection pass
  // that deterministically picks a relative-mouse HID interface.
  if (input.mouse_devices > 1) {
    log.Logf("virtio-input-events: multiple mouse devices detected (%d); selecting deterministically",
             input.mouse_devices);
    mouse_path.clear();
  }

  // If `VirtioInputTest` selected a pointing device, validate that it is actually a *relative* mouse.
  // When both a mouse and a tablet are attached, both may advertise a Mouse application collection,
  // but the tablet reports X/Y as Absolute and cannot satisfy this end-to-end relative mouse event test.
  if (!mouse_path.empty()) {
    HANDLE h = OpenHidDeviceForIoctl(mouse_path.c_str());
    if (h != INVALID_HANDLE_VALUE) {
      const auto report_desc = ReadHidReportDescriptor(log, h);
      CloseHandle(h);
      if (report_desc.has_value()) {
        const auto summary = SummarizeHidReportDescriptor(*report_desc);
        const bool has_keyboard = summary.keyboard_app_collections > 0;
        const bool has_mouse = summary.mouse_app_collections > 0;
        const bool has_relative_xy = summary.mouse_xy_relative_collections > 0;
        const bool has_absolute_xy = summary.mouse_xy_absolute_collections > 0;

        if (!(has_mouse && !has_keyboard && has_relative_xy)) {
          if (has_mouse && !has_keyboard && has_absolute_xy && !has_relative_xy) {
            log.Logf("virtio-input-events: input-selected mouse interface is absolute; searching for relative mouse: %s",
                     WideToUtf8(mouse_path).c_str());
          } else {
            log.Logf("virtio-input-events: input-selected mouse interface is not a relative mouse; searching again: %s",
                     WideToUtf8(mouse_path).c_str());
          }
          mouse_path.clear();
        }
      } else {
        // If we can't read the descriptor, fall back to SetupDi enumeration below.
        mouse_path.clear();
      }
    } else {
      mouse_path.clear();
    }
  }

  if (keyboard_path.empty() || mouse_path.empty()) {
    const auto paths = FindVirtioInputHidPaths(log);
    // Only treat "missing_keyboard_device" as fatal if we don't already have a keyboard path from the
    // earlier virtio-input probe.
    if (!paths.reason.empty() && !(paths.reason == "missing_keyboard_device" && !keyboard_path.empty())) {
      out.reason = paths.reason;
      out.win32_error = paths.win32_error;
      return out;
    }
    if (keyboard_path.empty()) keyboard_path = paths.keyboard_path;
    if (mouse_path.empty()) mouse_path = paths.mouse_path;
  }

  HidOverlappedReader kbd{};
  HidOverlappedReader mouse{};
  kbd.buf.resize(64);
  mouse.buf.resize(64);

  kbd.h = OpenHidDeviceForRead(keyboard_path.c_str());
  if (kbd.h == INVALID_HANDLE_VALUE) {
    out.reason = "open_keyboard_failed";
    out.win32_error = GetLastError();
    return out;
  }
  mouse.h = OpenHidDeviceForRead(mouse_path.c_str());
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

  const DWORD timeout_ms = (want_modifiers || want_buttons || want_wheel) ? 15000 : 10000;
  const DWORD deadline_ms = GetTickCount() + timeout_ms;
  bool base_ok = false;
  DWORD wheel_grace_deadline_ms = 0;
  const bool want_any_extra = want_modifiers || want_buttons || want_wheel;
  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    const bool have_base = VirtioInputEventsBaseOk(out);
    if (have_base && !base_ok) {
      base_ok = true;
      wheel_grace_deadline_ms = GetTickCount() + 250;
      if (static_cast<int32_t>(wheel_grace_deadline_ms - deadline_ms) > 0) {
        wheel_grace_deadline_ms = deadline_ms;
      }
    }

    if (want_any_extra) {
      const bool mods_ok = VirtioInputEventsModifiersOk(out);
      const bool btn_ok = VirtioInputEventsButtonsOk(out);
      const bool wheel_ok = VirtioInputEventsWheelOk(out);
      if (have_base && (!want_modifiers || mods_ok) && (!want_buttons || btn_ok) && (!want_wheel || wheel_ok)) {
        break;
      }
    } else if (base_ok) {
      // Base test succeeded. Keep reading for a short grace window so optional wheel/hwheel
      // events injected by the host harness can be observed without delaying the common case.
      if ((out.saw_mouse_wheel && out.saw_mouse_hwheel) ||
          static_cast<int32_t>(GetTickCount() - wheel_grace_deadline_ms) >= 0) {
        break;
      }
    }

    const DWORD now = GetTickCount();
    DWORD effective_deadline_ms = deadline_ms;
    if (!want_any_extra && base_ok) {
      effective_deadline_ms = wheel_grace_deadline_ms;
    }
    const int32_t diff = static_cast<int32_t>(effective_deadline_ms - now);
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

  const bool base = VirtioInputEventsBaseOk(out);
  out.ok = out.reason.empty() && base;
  out.modifiers_ok = out.ok && VirtioInputEventsModifiersOk(out);
  out.buttons_ok = out.ok && VirtioInputEventsButtonsOk(out);
  out.wheel_ok = out.ok && VirtioInputEventsWheelOk(out);

  const bool all_requested_ok = out.ok && (!want_modifiers || out.modifiers_ok) && (!want_buttons || out.buttons_ok) &&
                                (!want_wheel || out.wheel_ok);
  if (all_requested_ok) return out;

  if (out.reason.empty()) {
    out.reason = "timeout";
  }
  return out;
}

struct VirtioInputTabletEventsTestResult {
  bool ok = false;
  bool saw_move_target = false;
  bool saw_left_down = false;
  bool saw_left_up = false;
  int tablet_reports = 0;
  std::string reason;
  DWORD win32_error = 0;
  int32_t last_x = 0;
  int32_t last_y = 0;
  int32_t last_left = 0;
};

struct HidFieldInfo {
  uint8_t report_id = 0; // 0 means no report ID prefix
  uint32_t usage_page = 0;
  uint32_t usage = 0;
  uint32_t bit_offset = 0;
  uint32_t bit_size = 0;
  int32_t logical_min = 0;
  int32_t logical_max = 0;
  bool relative = false;
};

struct TabletHidReportLayout {
  uint8_t report_id = 0;
  bool have_x = false;
  bool have_y = false;
  bool have_left = false;
  HidFieldInfo x{};
  HidFieldInfo y{};
  HidFieldInfo left{};
};

static uint32_t ExtractHidBits(const uint8_t* buf, DWORD len, uint32_t bit_offset, uint32_t bit_size) {
  if (!buf || len == 0 || bit_size == 0) return 0;
  uint32_t out = 0;
  for (uint32_t i = 0; i < bit_size && i < 32; i++) {
    const uint32_t bit = bit_offset + i;
    const uint32_t byte_idx = bit / 8;
    const uint32_t bit_idx = bit % 8;
    if (byte_idx >= len) break;
    if ((buf[byte_idx] >> bit_idx) & 1u) {
      out |= 1u << i;
    }
  }
  return out;
}

static int32_t SignExtendHid(uint32_t v, uint32_t bit_size) {
  if (bit_size == 0 || bit_size >= 32) return static_cast<int32_t>(v);
  const uint32_t sign_bit = 1u << (bit_size - 1);
  if ((v & sign_bit) == 0) return static_cast<int32_t>(v);
  const uint32_t mask = (1u << bit_size) - 1u;
  const int32_t signed_v = static_cast<int32_t>(v | ~mask);
  return signed_v;
}

static std::optional<int32_t> ExtractHidFieldValue(const HidFieldInfo& f, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0 || f.bit_size == 0) return std::nullopt;
  const uint32_t raw = ExtractHidBits(buf, len, f.bit_offset, f.bit_size);
  if (f.logical_min < 0) {
    return SignExtendHid(raw, f.bit_size);
  }
  return static_cast<int32_t>(raw);
}

static std::optional<TabletHidReportLayout> ParseTabletHidReportLayout(const std::vector<uint8_t>& desc) {
  // Minimal HID report descriptor parser sufficient to locate:
  // - Button 1 (left)
  // - Absolute X (Usage Page=Generic Desktop, Usage=X)
  // - Absolute Y (Usage Page=Generic Desktop, Usage=Y)

  struct GlobalState {
    uint32_t usage_page = 0;
    uint32_t report_size = 0;
    uint32_t report_count = 0;
    int32_t logical_min = 0;
    int32_t logical_max = 0;
    uint8_t report_id = 0;
  };

  GlobalState g{};
  std::vector<GlobalState> g_stack;

  std::vector<uint32_t> local_usages;
  std::optional<uint32_t> local_usage_min;
  std::optional<uint32_t> local_usage_max;
  auto clear_locals = [&]() {
    local_usages.clear();
    local_usage_min.reset();
    local_usage_max.reset();
  };

  bool uses_report_ids = false;
  // Per-report current bit offset (includes the implicit 8-bit report ID prefix when used).
  uint32_t bit_off[256];
  for (auto& v : bit_off) v = 0xFFFFFFFFu;

  struct PerReportFields {
    bool have_x = false;
    bool have_y = false;
    bool have_left = false;
    HidFieldInfo x{};
    HidFieldInfo y{};
    HidFieldInfo left{};
  };
  PerReportFields fields[256]{};

  auto ensure_bit_off = [&](uint8_t rid) -> uint32_t& {
    if (bit_off[rid] == 0xFFFFFFFFu) {
      if (uses_report_ids && rid != 0) {
        bit_off[rid] = 8; // report ID byte
      } else {
        bit_off[rid] = 0;
      }
    }
    return bit_off[rid];
  };

  auto sign_extend_value = [&](uint32_t v, uint32_t bits) -> int32_t {
    if (bits == 0) return 0;
    if (bits >= 32) return static_cast<int32_t>(v);
    return SignExtendHid(v, bits);
  };

  auto parse_u32 = [&](uint32_t v, uint32_t bits) -> uint32_t {
    if (bits == 0) return 0;
    if (bits >= 32) return v;
    return v & ((1u << bits) - 1u);
  };

  size_t i = 0;
  while (i < desc.size()) {
    const uint8_t prefix = desc[i++];
    if (prefix == 0xFE) {
      if (i + 2 > desc.size()) break;
      const uint8_t size = desc[i++];
      i++; // long item tag
      if (i + size > desc.size()) break;
      i += size;
      continue;
    }

    const uint8_t size_code = prefix & 0x3;
    const uint8_t type = (prefix >> 2) & 0x3;
    const uint8_t tag = (prefix >> 4) & 0xF;
    const size_t data_size = (size_code == 3) ? 4 : size_code;
    if (i + data_size > desc.size()) break;

    uint32_t value_u = 0;
    for (size_t j = 0; j < data_size; j++) {
      value_u |= static_cast<uint32_t>(desc[i + j]) << (8u * j);
    }
    i += data_size;

    switch (type) {
      case 0: { // Main
        if (tag == 0x8) { // Input
          const uint8_t rid = g.report_id;
          uint32_t& off_bits = ensure_bit_off(rid);

          // Expand the local usage list if only a range is specified.
          std::vector<uint32_t> usages = local_usages;
          if (usages.empty() && local_usage_min.has_value() && local_usage_max.has_value() &&
              *local_usage_max >= *local_usage_min) {
            const uint32_t count = *local_usage_max - *local_usage_min + 1;
            usages.reserve(count);
            for (uint32_t u = *local_usage_min; u <= *local_usage_max; u++) usages.push_back(u);
          }

          const uint32_t rs = g.report_size;
          const uint32_t rc = std::max<uint32_t>(1, g.report_count);
          const bool is_rel = (value_u & 0x04u) != 0;

          for (uint32_t idx = 0; idx < rc; idx++) {
            uint32_t usage = 0;
            if (!usages.empty()) {
              usage = usages[std::min<size_t>(idx, usages.size() - 1)];
            } else if (local_usage_min.has_value()) {
              usage = *local_usage_min;
            }

            HidFieldInfo field{};
            field.report_id = rid;
            field.usage_page = g.usage_page;
            field.usage = usage;
            field.bit_offset = off_bits + idx * rs;
            field.bit_size = rs;
            field.logical_min = g.logical_min;
            field.logical_max = g.logical_max;
            field.relative = is_rel;

            // Button 1: left.
            if (field.usage_page == 0x09 && field.usage == 0x01 && rs > 0) {
              fields[rid].left = field;
              fields[rid].have_left = true;
            }
            // Generic Desktop X/Y axes.
            if (field.usage_page == 0x01 && field.usage == 0x30 && rs > 0) {
              fields[rid].x = field;
              fields[rid].have_x = true;
            }
            if (field.usage_page == 0x01 && field.usage == 0x31 && rs > 0) {
              fields[rid].y = field;
              fields[rid].have_y = true;
            }
          }

          off_bits += rs * rc;
        }
        // Local items are cleared after each main item per HID spec.
        clear_locals();
        break;
      }
      case 1: { // Global
        const uint32_t bits = static_cast<uint32_t>(data_size * 8);
        if (tag == 0x0) { // Usage Page
          g.usage_page = value_u;
        } else if (tag == 0x1) { // Logical Minimum
          g.logical_min = sign_extend_value(value_u, bits);
        } else if (tag == 0x2) { // Logical Maximum
          g.logical_max = sign_extend_value(value_u, bits);
        } else if (tag == 0x7) { // Report Size
          g.report_size = parse_u32(value_u, bits);
        } else if (tag == 0x8) { // Report ID
          g.report_id = static_cast<uint8_t>(value_u & 0xFF);
          uses_report_ids = true;
          // Ensure the bit offset for this report accounts for the implicit report ID byte.
          (void)ensure_bit_off(g.report_id);
        } else if (tag == 0x9) { // Report Count
          g.report_count = parse_u32(value_u, bits);
        } else if (tag == 0xA) { // Push
          g_stack.push_back(g);
        } else if (tag == 0xB) { // Pop
          if (!g_stack.empty()) {
            g = g_stack.back();
            g_stack.pop_back();
          }
        }
        break;
      }
      case 2: { // Local
        if (tag == 0x0) { // Usage
          local_usages.push_back(value_u);
        } else if (tag == 0x1) { // Usage Minimum
          local_usage_min = value_u;
        } else if (tag == 0x2) { // Usage Maximum
          local_usage_max = value_u;
        }
        break;
      }
      default:
        break;
    }
  }

  // Pick the report ID that contains absolute X/Y. Prefer the one that also has Button 1.
  std::optional<TabletHidReportLayout> fallback;
  for (int rid = 0; rid < 256; rid++) {
    if (!fields[rid].have_x || !fields[rid].have_y) continue;
    if (fields[rid].x.relative || fields[rid].y.relative) continue; // must be absolute
    TabletHidReportLayout candidate{};
    candidate.report_id = static_cast<uint8_t>(rid);
    candidate.have_x = true;
    candidate.have_y = true;
    candidate.x = fields[rid].x;
    candidate.y = fields[rid].y;
    if (fields[rid].have_left) {
      candidate.have_left = true;
      candidate.left = fields[rid].left;
      return candidate;
    }
    if (!fallback.has_value()) {
      fallback = candidate;
    }
  }
  return fallback;
}

static bool MatchesTabletCoord(int32_t observed, int32_t host_value, int32_t logical_max) {
  // Host harness injects in QMP's conventional absolute range [0, 32767].
  static const int32_t kQmpAbsMax = 32767;
  const int32_t tol = 2;

  const auto close = [&](int32_t a, int32_t b) { return std::abs(a - b) <= tol; };

  // Candidate 1: raw value (device already uses 0..32767 scale).
  if (close(observed, host_value)) return true;

  if (logical_max > 0) {
    // Candidate 2: clamp (device max smaller than QMP value).
    if (close(observed, std::min(host_value, logical_max))) return true;
    // Candidate 3: scale (device max differs; treat host_value as normalized).
    const int64_t scaled =
        (static_cast<int64_t>(host_value) * static_cast<int64_t>(logical_max) + (kQmpAbsMax / 2)) / kQmpAbsMax;
    if (close(observed, static_cast<int32_t>(scaled))) return true;
  }
  return false;
}

static void ProcessTabletReport(VirtioInputTabletEventsTestResult& out, const TabletHidReportLayout& layout,
                                const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  if (layout.report_id != 0) {
    if (len < 1 || buf[0] != layout.report_id) return;
  }

  const auto x_opt = layout.have_x ? ExtractHidFieldValue(layout.x, buf, len) : std::nullopt;
  const auto y_opt = layout.have_y ? ExtractHidFieldValue(layout.y, buf, len) : std::nullopt;
  const auto left_opt = layout.have_left ? ExtractHidFieldValue(layout.left, buf, len) : std::nullopt;
  if (!x_opt.has_value() || !y_opt.has_value() || !left_opt.has_value()) return;

  const int32_t x = *x_opt;
  const int32_t y = *y_opt;
  const int32_t left = *left_opt;

  out.last_x = x;
  out.last_y = y;
  out.last_left = left;

  // Must match the host harness constants (see invoke_aero_virtio_win7_tests.py).
  static const int32_t kHostTargetX = 10000;
  static const int32_t kHostTargetY = 20000;

  if (MatchesTabletCoord(x, kHostTargetX, layout.x.logical_max) &&
      MatchesTabletCoord(y, kHostTargetY, layout.y.logical_max)) {
    out.saw_move_target = true;
  }

  if (left != 0) out.saw_left_down = true;
  if (out.saw_left_down && left == 0) out.saw_left_up = true;
}

static VirtioInputTabletEventsTestResult VirtioInputTabletEventsTest(Logger& log, const VirtioInputTestResult& input) {
  VirtioInputTabletEventsTestResult out{};

  const std::wstring tablet_path = input.tablet_device_path;
  if (tablet_path.empty()) {
    out.reason = "missing_tablet_device";
    return out;
  }

  // Parse the report descriptor so we can decode X/Y and button fields robustly.
  HANDLE h_ioctl = OpenHidDeviceForIoctl(tablet_path.c_str());
  if (h_ioctl == INVALID_HANDLE_VALUE) {
    out.reason = "open_tablet_failed";
    out.win32_error = GetLastError();
    return out;
  }
  const auto report_desc = ReadHidReportDescriptor(log, h_ioctl);
  CloseHandle(h_ioctl);
  if (!report_desc.has_value()) {
    out.reason = "get_report_descriptor_failed";
    out.win32_error = GetLastError();
    return out;
  }

  const auto layout_opt = ParseTabletHidReportLayout(*report_desc);
  if (!layout_opt.has_value() || !layout_opt->have_left) {
    out.reason = "unsupported_report_descriptor";
    return out;
  }
  const TabletHidReportLayout layout = *layout_opt;
  if (layout.report_id != 4) {
    // Contract: the virtio-input tablet descriptor uses Report ID 4. Enforce this so we catch regressions
    // where the tablet enumerates as a mouse/relative pointer.
    out.reason = "unexpected_report_id";
    return out;
  }

  HidOverlappedReader tablet{};
  tablet.buf.resize(64);
  tablet.h = OpenHidDeviceForRead(tablet_path.c_str());
  if (tablet.h == INVALID_HANDLE_VALUE) {
    out.reason = "open_tablet_failed";
    out.win32_error = GetLastError();
    return out;
  }

  if (!tablet.StartRead()) {
    out.reason = "read_tablet_failed";
    out.win32_error = tablet.last_error;
    tablet.CancelAndClose();
    return out;
  }

  log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY");

  const DWORD deadline_ms = GetTickCount() + 10000;
  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    if (out.saw_move_target && out.saw_left_down && out.saw_left_up) {
      out.ok = true;
      break;
    }

    const DWORD now = GetTickCount();
    const int32_t diff = static_cast<int32_t>(deadline_ms - now);
    const DWORD timeout = diff > 0 ? static_cast<DWORD>(diff) : 0;
    const DWORD wait = WaitForSingleObject(tablet.ev, timeout);
    if (wait == WAIT_TIMEOUT) break;
    if (wait == WAIT_FAILED) {
      out.reason = "wait_failed";
      out.win32_error = GetLastError();
      break;
    }

    DWORD n = 0;
    if (!tablet.FinishRead(n)) {
      out.reason = "read_tablet_failed";
      out.win32_error = tablet.last_error;
      break;
    }

    out.tablet_reports++;
    ProcessTabletReport(out, layout, tablet.buf.data(), n);

    if (!tablet.StartRead()) {
      out.reason = "read_tablet_failed";
      out.win32_error = tablet.last_error;
      break;
    }
  }

  tablet.CancelAndClose();

  if (out.ok) return out;
  if (out.reason.empty()) out.reason = "timeout";
  return out;
}

struct VirtioInputMediaKeysTestResult {
  bool ok = false;
  bool saw_volume_up_down = false;
  bool saw_volume_up_up = false;
  int reports = 0;
  std::string reason;
  DWORD win32_error = 0;
};

static void ProcessConsumerReport(VirtioInputMediaKeysTestResult& out, const uint8_t* buf, DWORD len) {
  if (!buf || len == 0) return;

  size_t off = 0;
  // Consumer control report is typically 2 bytes with ReportID=3:
  //   [3][bits]
  // Some HID stacks may omit the report ID for collection-specific interfaces; handle both.
  if (len >= 2 && buf[0] == 3) {
    off = 1;
  } else if (len != 1) {
    return;
  }
  if (len < off + 1) return;

  const uint8_t bits = buf[off];
  const bool volume_up = (bits & 0x04) != 0; // bit2 in the driver Consumer Control report
  if (volume_up) out.saw_volume_up_down = true;
  if (out.saw_volume_up_down && !volume_up) out.saw_volume_up_up = true;
}

static VirtioInputMediaKeysTestResult VirtioInputMediaKeysTest(Logger& log, const VirtioInputTestResult& input) {
  VirtioInputMediaKeysTestResult out{};

  std::wstring consumer_path = input.consumer_device_path;
  if (consumer_path.empty()) {
    const auto paths = FindVirtioInputHidPaths(log);
    if (!paths.reason.empty()) {
      out.reason = paths.reason;
      out.win32_error = paths.win32_error;
      return out;
    }
    if (!paths.consumer_path.empty()) {
      consumer_path = paths.consumer_path;
    } else {
      // Best-effort fallback: some stacks may not expose a distinct Consumer Control interface.
      consumer_path = paths.keyboard_path;
    }
  }

  if (consumer_path.empty()) {
    out.reason = "missing_consumer_device";
    return out;
  }

  HidOverlappedReader reader{};
  reader.buf.resize(64);

  reader.h = OpenHidDeviceForRead(consumer_path.c_str());
  if (reader.h == INVALID_HANDLE_VALUE) {
    out.reason = "open_consumer_failed";
    out.win32_error = GetLastError();
    return out;
  }

  if (!reader.StartRead()) {
    out.reason = "read_consumer_failed";
    out.win32_error = reader.last_error;
    reader.CancelAndClose();
    return out;
  }

  log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY");

  const DWORD deadline_ms = GetTickCount() + 10000;
  while (static_cast<int32_t>(GetTickCount() - deadline_ms) < 0) {
    if (out.saw_volume_up_down && out.saw_volume_up_up) {
      out.ok = true;
      break;
    }

    const DWORD now = GetTickCount();
    const int32_t diff = static_cast<int32_t>(deadline_ms - now);
    const DWORD timeout = diff > 0 ? static_cast<DWORD>(diff) : 0;
    const DWORD wait = WaitForSingleObject(reader.ev, timeout);
    if (wait == WAIT_TIMEOUT) break;
    if (wait == WAIT_FAILED) {
      out.reason = "wait_failed";
      out.win32_error = GetLastError();
      break;
    }

    DWORD n = 0;
    if (!reader.FinishRead(n)) {
      out.reason = "read_consumer_failed";
      out.win32_error = reader.last_error;
      break;
    }

    out.reports++;
    ProcessConsumerReport(out, reader.buf.data(), n);

    if (!reader.StartRead()) {
      out.reason = "read_consumer_failed";
      out.win32_error = reader.last_error;
      break;
    }
  }

  reader.CancelAndClose();

  if (out.ok) return out;
  if (out.reason.empty()) out.reason = "timeout";
  return out;
}

struct VirtioInputLedTestResult {
  bool ok = false;
  int sent = 0;
  std::string reason;
  DWORD win32_error = 0;

  std::string format;   // e.g. "with_report_id", "no_report_id", "report_id_0"
  std::string led_name; // numlock/capslock/scrolllock
  uint8_t report_id = 0;
  uint32_t report_bytes = 0;

  LONG statusq_submits_delta = 0;
  LONG statusq_completions_delta = 0;
  LONG statusq_full_delta = 0;
  LONG statusq_drops_delta = 0;
  LONG led_writes_requested_delta = 0;
  LONG led_writes_submitted_delta = 0;
  LONG led_writes_dropped_delta = 0;
};

static HANDLE OpenHidDeviceForWrite(const wchar_t* path) {
  const DWORD share = FILE_SHARE_READ | FILE_SHARE_WRITE;
  const DWORD flags = FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED;
  const DWORD desired_accesses[] = {GENERIC_READ | GENERIC_WRITE, GENERIC_WRITE};
  for (const DWORD access : desired_accesses) {
    HANDLE h = CreateFileW(path, access, share, nullptr, OPEN_EXISTING, flags, nullptr);
    if (h != INVALID_HANDLE_VALUE) return h;
  }
  return INVALID_HANDLE_VALUE;
}

static bool HidWriteWithTimeout(HANDLE h, const uint8_t* buf, DWORD len, DWORD timeout_ms, DWORD* err_out) {
  if (err_out) *err_out = ERROR_SUCCESS;
  if (!buf || len == 0 || h == INVALID_HANDLE_VALUE) {
    if (err_out) *err_out = ERROR_INVALID_PARAMETER;
    return false;
  }

  HANDLE ev = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (!ev) {
    if (err_out) *err_out = GetLastError();
    return false;
  }

  OVERLAPPED ov{};
  ov.hEvent = ev;

  DWORD bytes = 0;
  BOOL ok = WriteFile(h, buf, len, nullptr, &ov);
  DWORD err = ok ? ERROR_SUCCESS : GetLastError();
  if (!ok && err != ERROR_IO_PENDING) {
    CloseHandle(ev);
    if (err_out) *err_out = err;
    return false;
  }

  const DWORD wait = WaitForSingleObject(ev, timeout_ms);
  if (wait == WAIT_TIMEOUT) {
    // Best-effort: cancel the outstanding write so CloseHandle doesn't hang.
    CancelIo(h);
    CloseHandle(ev);
    if (err_out) *err_out = ERROR_TIMEOUT;
    return false;
  }
  if (wait == WAIT_FAILED) {
    err = GetLastError();
    CancelIo(h);
    CloseHandle(ev);
    if (err_out) *err_out = err;
    return false;
  }

  if (!GetOverlappedResult(h, &ov, &bytes, FALSE)) {
    err = GetLastError();
    CloseHandle(ev);
    if (err_out) *err_out = err;
    return false;
  }
  CloseHandle(ev);

  if (bytes != len) {
    if (err_out) *err_out = ERROR_WRITE_FAULT;
    return false;
  }
  return true;
}

static bool SetHidBits(uint8_t* buf, size_t len, uint32_t bit_offset, uint32_t bit_size, uint32_t value) {
  if (!buf || len == 0 || bit_size == 0) return false;
  if (bit_size > 32) return false;
  if (bit_offset + bit_size > len * 8) return false;
  for (uint32_t i = 0; i < bit_size; i++) {
    const uint32_t bit = bit_offset + i;
    const uint32_t byte_idx = bit / 8;
    const uint32_t bit_idx = bit % 8;
    const uint8_t mask = static_cast<uint8_t>(1u << bit_idx);
    if ((value >> i) & 1u) {
      buf[byte_idx] |= mask;
    } else {
      buf[byte_idx] &= static_cast<uint8_t>(~mask);
    }
  }
  return true;
}

struct KeyboardLedOutputLayout {
  bool uses_report_ids = false;
  uint8_t report_id = 0;
  uint32_t bit_length = 0;
  uint32_t byte_length = 0;
  HidFieldInfo led_field{};
  std::string led_name;
};

static std::optional<KeyboardLedOutputLayout> ParseKeyboardLedOutputLayout(const std::vector<uint8_t>& desc) {
  struct GlobalState {
    uint32_t usage_page = 0;
    uint32_t report_size = 0;
    uint32_t report_count = 0;
    int32_t logical_min = 0;
    int32_t logical_max = 0;
    uint8_t report_id = 0;
  };

  GlobalState g{};
  std::vector<GlobalState> g_stack;

  std::vector<uint32_t> local_usages;
  std::optional<uint32_t> local_usage_min;
  std::optional<uint32_t> local_usage_max;
  auto clear_locals = [&]() {
    local_usages.clear();
    local_usage_min.reset();
    local_usage_max.reset();
  };

  bool uses_report_ids = false;
  // Per-report current output bit offset (includes implicit report ID byte when used).
  uint32_t out_bit_off[256];
  for (auto& v : out_bit_off) v = 0xFFFFFFFFu;

  struct PerReportLedFields {
    bool have_num = false;
    bool have_caps = false;
    bool have_scroll = false;
    HidFieldInfo num{};
    HidFieldInfo caps{};
    HidFieldInfo scroll{};
  };
  PerReportLedFields fields[256]{};

  auto ensure_out_bit_off = [&](uint8_t rid) -> uint32_t& {
    if (out_bit_off[rid] == 0xFFFFFFFFu) {
      if (uses_report_ids && rid != 0) {
        out_bit_off[rid] = 8; // report ID byte
      } else {
        out_bit_off[rid] = 0;
      }
    }
    return out_bit_off[rid];
  };

  auto sign_extend_value = [&](uint32_t v, uint32_t bits) -> int32_t {
    if (bits == 0) return 0;
    if (bits >= 32) return static_cast<int32_t>(v);
    return SignExtendHid(v, bits);
  };

  auto parse_u32 = [&](uint32_t v, uint32_t bits) -> uint32_t {
    if (bits == 0) return 0;
    if (bits >= 32) return v;
    return v & ((1u << bits) - 1u);
  };

  size_t i = 0;
  while (i < desc.size()) {
    const uint8_t prefix = desc[i++];
    if (prefix == 0xFE) {
      if (i + 2 > desc.size()) break;
      const uint8_t size = desc[i++];
      i++; // long item tag
      if (i + size > desc.size()) break;
      i += size;
      continue;
    }

    const uint8_t size_code = prefix & 0x3;
    const uint8_t type = (prefix >> 2) & 0x3;
    const uint8_t tag = (prefix >> 4) & 0xF;
    const size_t data_size = (size_code == 3) ? 4 : size_code;
    if (i + data_size > desc.size()) break;

    uint32_t value_u = 0;
    for (size_t j = 0; j < data_size; j++) {
      value_u |= static_cast<uint32_t>(desc[i + j]) << (8u * j);
    }
    i += data_size;

    switch (type) {
      case 0: { // Main
        if (tag == 0x9) { // Output
          const uint8_t rid = g.report_id;
          uint32_t& off_bits = ensure_out_bit_off(rid);

          std::vector<uint32_t> usages = local_usages;
          if (usages.empty() && local_usage_min.has_value() && local_usage_max.has_value() &&
              *local_usage_max >= *local_usage_min) {
            const uint32_t count = *local_usage_max - *local_usage_min + 1;
            usages.reserve(count);
            for (uint32_t u = *local_usage_min; u <= *local_usage_max; u++) usages.push_back(u);
          }

          const uint32_t rs = g.report_size;
          const uint32_t rc = std::max<uint32_t>(1, g.report_count);

          for (uint32_t idx = 0; idx < rc; idx++) {
            uint32_t usage = 0;
            if (!usages.empty()) {
              usage = usages[std::min<size_t>(idx, usages.size() - 1)];
            } else if (local_usage_min.has_value()) {
              usage = *local_usage_min;
            }

            HidFieldInfo field{};
            field.report_id = rid;
            field.usage_page = g.usage_page;
            field.usage = usage;
            field.bit_offset = off_bits + idx * rs;
            field.bit_size = rs;
            field.logical_min = g.logical_min;
            field.logical_max = g.logical_max;
            field.relative = false;

            if (field.usage_page == 0x08 && rs > 0) { // LEDs page
              if (field.usage == 0x01) {
                fields[rid].num = field;
                fields[rid].have_num = true;
              } else if (field.usage == 0x02) {
                fields[rid].caps = field;
                fields[rid].have_caps = true;
              } else if (field.usage == 0x03) {
                fields[rid].scroll = field;
                fields[rid].have_scroll = true;
              }
            }
          }

          off_bits += rs * rc;
        }
        clear_locals();
        break;
      }
      case 1: { // Global
        const uint32_t bits = static_cast<uint32_t>(data_size * 8);
        if (tag == 0x0) { // Usage Page
          g.usage_page = value_u;
        } else if (tag == 0x1) { // Logical Minimum
          g.logical_min = sign_extend_value(value_u, bits);
        } else if (tag == 0x2) { // Logical Maximum
          g.logical_max = sign_extend_value(value_u, bits);
        } else if (tag == 0x7) { // Report Size
          g.report_size = parse_u32(value_u, bits);
        } else if (tag == 0x8) { // Report ID
          g.report_id = static_cast<uint8_t>(value_u & 0xFF);
          uses_report_ids = true;
          (void)ensure_out_bit_off(g.report_id);
        } else if (tag == 0x9) { // Report Count
          g.report_count = parse_u32(value_u, bits);
        } else if (tag == 0xA) { // Push
          g_stack.push_back(g);
        } else if (tag == 0xB) { // Pop
          if (!g_stack.empty()) {
            g = g_stack.back();
            g_stack.pop_back();
          }
        }
        break;
      }
      case 2: { // Local
        if (tag == 0x0) { // Usage
          local_usages.push_back(value_u);
        } else if (tag == 0x1) { // Usage Minimum
          local_usage_min = value_u;
        } else if (tag == 0x2) { // Usage Maximum
          local_usage_max = value_u;
        }
        break;
      }
      default:
        break;
    }
  }

  // Select a report ID with at least one of the required LED usages.
  for (int rid = 0; rid < 256; rid++) {
    if (out_bit_off[rid] == 0xFFFFFFFFu) continue;
    const auto& f = fields[rid];
    if (!f.have_num && !f.have_caps && !f.have_scroll) continue;

    KeyboardLedOutputLayout out{};
    out.uses_report_ids = uses_report_ids;
    out.report_id = static_cast<uint8_t>(rid);
    out.bit_length = out_bit_off[rid];
    out.byte_length = (out.bit_length + 7) / 8;

    if (f.have_caps) {
      out.led_field = f.caps;
      out.led_name = "capslock";
    } else if (f.have_num) {
      out.led_field = f.num;
      out.led_name = "numlock";
    } else {
      out.led_field = f.scroll;
      out.led_name = "scrolllock";
    }

    if (out.byte_length == 0) return std::nullopt;
    return out;
  }

  return std::nullopt;
}

static std::optional<VIOINPUT_COUNTERS> QueryVirtioInputCounters(HANDLE h, DWORD* win32_error_out) {
  if (win32_error_out) *win32_error_out = ERROR_SUCCESS;
  if (h == INVALID_HANDLE_VALUE) {
    if (win32_error_out) *win32_error_out = ERROR_INVALID_HANDLE;
    return std::nullopt;
  }

  std::vector<uint8_t> buf(4096);
  DWORD bytes = 0;
  if (!DeviceIoControl(h, IOCTL_VIOINPUT_QUERY_COUNTERS, nullptr, 0, buf.data(), static_cast<DWORD>(buf.size()), &bytes,
                       nullptr)) {
    if (win32_error_out) *win32_error_out = GetLastError();
    return std::nullopt;
  }
  if (bytes < sizeof(ULONG) * 2) {
    if (win32_error_out) *win32_error_out = ERROR_INSUFFICIENT_BUFFER;
    return std::nullopt;
  }

  VIOINPUT_COUNTERS out{};
  memset(&out, 0, sizeof(out));
  memcpy(&out, buf.data(), std::min<size_t>(bytes, sizeof(out)));

  // Ensure we received the fields required by this test (statusq counters).
  if (bytes < offsetof(VIOINPUT_COUNTERS, StatusQFull) + sizeof(out.StatusQFull)) {
    if (win32_error_out) *win32_error_out = ERROR_INSUFFICIENT_BUFFER;
    return std::nullopt;
  }

  return out;
}

static VirtioInputLedTestResult VirtioInputLedTest(Logger& log, const VirtioInputTestResult& input) {
  VirtioInputLedTestResult out{};

  std::wstring keyboard_path = input.keyboard_device_path;
  if (keyboard_path.empty()) {
    const auto paths = FindVirtioInputHidPaths(log);
    if (!paths.reason.empty()) {
      out.reason = paths.reason;
      out.win32_error = paths.win32_error;
      return out;
    }
    keyboard_path = paths.keyboard_path;
  }

  if (keyboard_path.empty()) {
    out.reason = "missing_keyboard_device";
    out.win32_error = ERROR_NOT_FOUND;
    return out;
  }

  HANDLE h_ioctl = OpenHidDeviceForIoctl(keyboard_path.c_str());
  if (h_ioctl == INVALID_HANDLE_VALUE) {
    out.reason = "open_keyboard_failed";
    out.win32_error = GetLastError();
    return out;
  }

  const auto report_desc = ReadHidReportDescriptor(log, h_ioctl);
  if (!report_desc.has_value()) {
    out.reason = "get_report_descriptor_failed";
    out.win32_error = GetLastError();
    CloseHandle(h_ioctl);
    return out;
  }

  const auto layout_opt = ParseKeyboardLedOutputLayout(*report_desc);
  if (!layout_opt.has_value()) {
    out.reason = "unsupported_report_descriptor";
    out.win32_error = ERROR_NOT_SUPPORTED;
    CloseHandle(h_ioctl);
    return out;
  }
  const KeyboardLedOutputLayout layout = *layout_opt;
  out.report_id = layout.report_id;
  out.report_bytes = layout.byte_length;
  out.led_name = layout.led_name;

  // Best-effort: reset the driver's counters so we can compute clean deltas. Not fatal if unavailable.
  DWORD ignored = 0;
  (void)DeviceIoControl(h_ioctl, IOCTL_VIOINPUT_RESET_COUNTERS, nullptr, 0, nullptr, 0, &ignored, nullptr);

  DWORD err = ERROR_SUCCESS;
  const auto before_opt = QueryVirtioInputCounters(h_ioctl, &err);
  if (!before_opt.has_value()) {
    out.reason = "query_counters_failed";
    out.win32_error = err;
    CloseHandle(h_ioctl);
    return out;
  }
  const VIOINPUT_COUNTERS before = *before_opt;

  struct CandidateFormat {
    std::string name;
    uint8_t report_id_byte = 0;
    uint32_t report_bytes = 0;
    uint32_t led_bit_offset = 0;
    uint32_t led_bit_size = 0;
  };

  std::vector<CandidateFormat> formats;

  const bool primary_has_report_id = layout.report_id != 0;
  if (primary_has_report_id) {
    formats.push_back(CandidateFormat{
        "with_report_id",
        layout.report_id,
        layout.byte_length,
        layout.led_field.bit_offset,
        layout.led_field.bit_size,
    });
    if (layout.byte_length > 1 && layout.led_field.bit_offset >= 8) {
      formats.push_back(CandidateFormat{
          "no_report_id",
          0,
          layout.byte_length - 1,
          layout.led_field.bit_offset - 8,
          layout.led_field.bit_size,
      });
    }
  } else {
    formats.push_back(CandidateFormat{
        "no_report_id",
        0,
        layout.byte_length,
        layout.led_field.bit_offset,
        layout.led_field.bit_size,
    });
    // Some HID stacks require an explicit ReportID=0 prefix even when the descriptor does not use report IDs.
    formats.push_back(CandidateFormat{
        "report_id_0",
        0,
        layout.byte_length + 1,
        layout.led_field.bit_offset + 8,
        layout.led_field.bit_size,
    });
  }

  HANDLE h_write = OpenHidDeviceForWrite(keyboard_path.c_str());
  if (h_write == INVALID_HANDLE_VALUE) {
    out.reason = "open_keyboard_write_failed";
    out.win32_error = GetLastError();
    CloseHandle(h_ioctl);
    return out;
  }

  // Pick a working write format by issuing a single "LED off" report and observing the driver's counters.
  std::optional<CandidateFormat> chosen;
  DWORD last_write_err = ERROR_SUCCESS;
  for (const auto& fmt : formats) {
    if (fmt.report_bytes == 0) continue;

    std::vector<uint8_t> report(fmt.report_bytes, 0);
    if (fmt.name != "no_report_id") {
      // For with_report_id/report_id_0, the first byte carries the report ID value.
      report[0] = fmt.report_id_byte;
    } else if (primary_has_report_id == false && fmt.report_bytes == layout.byte_length + 1) {
      // report_id_0 uses name != no_report_id, handled above.
    } else if (primary_has_report_id && fmt.report_bytes == layout.byte_length) {
      // with_report_id uses name != no_report_id, handled above.
    }

    if (!SetHidBits(report.data(), report.size(), fmt.led_bit_offset, fmt.led_bit_size, 0)) {
      continue;
    }

    last_write_err = ERROR_SUCCESS;
    if (!HidWriteWithTimeout(h_write, report.data(), static_cast<DWORD>(report.size()), 2000, &last_write_err)) {
      continue;
    }
    out.sent++;

    const auto after_opt = QueryVirtioInputCounters(h_ioctl, &err);
    if (!after_opt.has_value()) {
      out.reason = "query_counters_failed";
      out.win32_error = err;
      CloseHandle(h_write);
      CloseHandle(h_ioctl);
      return out;
    }
    const auto& after = *after_opt;

    if (after.LedWritesRequested > before.LedWritesRequested) {
      chosen = fmt;
      out.format = fmt.name;
      break;
    }
  }

  if (!chosen.has_value()) {
    out.reason = "write_not_processed";
    out.win32_error = last_write_err ? last_write_err : ERROR_INVALID_DATA;
    CloseHandle(h_write);
    CloseHandle(h_ioctl);
    return out;
  }

  // Record the final chosen report size for markers (may differ from the descriptor-derived default
  // when we fall back to adding/removing an explicit report ID prefix).
  out.report_bytes = chosen->report_bytes;

  // Send additional toggles so we exercise multiple statusq buffers.
  constexpr int kWriteCount = 32;
  for (int i = 1; i < kWriteCount; i++) {
    std::vector<uint8_t> report(chosen->report_bytes, 0);
    if (chosen->name != "no_report_id") {
      report[0] = chosen->report_id_byte;
    }

    const uint32_t value = (i & 1) ? 1u : 0u;
    if (!SetHidBits(report.data(), report.size(), chosen->led_bit_offset, chosen->led_bit_size, value)) {
      out.reason = "build_report_failed";
      out.win32_error = ERROR_INVALID_DATA;
      break;
    }

    err = ERROR_SUCCESS;
    if (!HidWriteWithTimeout(h_write, report.data(), static_cast<DWORD>(report.size()), 2000, &err)) {
      out.reason = (err == ERROR_TIMEOUT) ? "write_timeout" : "write_failed";
      out.win32_error = err;
      break;
    }
    out.sent++;
  }

  CloseHandle(h_write);

  if (!out.reason.empty()) {
    CloseHandle(h_ioctl);
    return out;
  }

  // Snapshot counters immediately after writes so we can fail with a clearer reason when nothing was
  // submitted to the statusq (e.g. statusq inactive or the write wasn't classified as a keyboard LED report).
  const auto after_send_opt = QueryVirtioInputCounters(h_ioctl, &err);
  if (!after_send_opt.has_value()) {
    out.reason = "query_counters_failed";
    out.win32_error = err;
    CloseHandle(h_ioctl);
    return out;
  }
  const VIOINPUT_COUNTERS after_send = *after_send_opt;
  if (after_send.LedWritesRequested <= before.LedWritesRequested) {
    out.reason = "no_led_writes_recorded";
    out.win32_error = ERROR_INVALID_DATA;
    CloseHandle(h_ioctl);
    return out;
  }
  if (after_send.StatusQSubmits <= before.StatusQSubmits) {
    out.reason = "no_statusq_submits";
    out.win32_error = ERROR_INVALID_DATA;
    CloseHandle(h_ioctl);
    return out;
  }

  // Wait for all statusq submissions we triggered to complete.
  const DWORD poll_deadline_ms = GetTickCount() + 5000;
  VIOINPUT_COUNTERS last = after_send;
  while (static_cast<int32_t>(GetTickCount() - poll_deadline_ms) < 0) {
    const auto cur_opt = QueryVirtioInputCounters(h_ioctl, &err);
    if (!cur_opt.has_value()) {
      out.reason = "query_counters_failed";
      out.win32_error = err;
      break;
    }
    last = *cur_opt;

    const LONG submits = last.StatusQSubmits - before.StatusQSubmits;
    const LONG completions = last.StatusQCompletions - before.StatusQCompletions;

    if (submits > 0 && completions >= submits) {
      out.ok = true;
      break;
    }
    Sleep(50);
  }

  CloseHandle(h_ioctl);

  if (!out.ok && out.reason.empty()) {
    out.reason = "timeout_waiting_statusq_completions";
    out.win32_error = ERROR_TIMEOUT;
  }

  // Compute deltas for diagnostics.
  out.statusq_submits_delta = last.StatusQSubmits - before.StatusQSubmits;
  out.statusq_completions_delta = last.StatusQCompletions - before.StatusQCompletions;
  out.statusq_full_delta = last.StatusQFull - before.StatusQFull;
  out.statusq_drops_delta = last.VirtioStatusDrops - before.VirtioStatusDrops;
  out.led_writes_requested_delta = last.LedWritesRequested - before.LedWritesRequested;
  out.led_writes_submitted_delta = last.LedWritesSubmitted - before.LedWritesSubmitted;
  out.led_writes_dropped_delta = last.LedWritesDropped - before.LedWritesDropped;

  if (out.ok) {
    if (out.statusq_completions_delta < out.statusq_submits_delta) {
      out.ok = false;
      out.reason = "incomplete_statusq_completions";
      out.win32_error = ERROR_TIMEOUT;
    }
  }

  return out;
}

struct VirtioNetAdapter {
  // NetCfg instance GUID (used by GetAdaptersAddresses).
  std::wstring instance_id;   // e.g. "{GUID}"
  // PnP devnode for the adapter (used for cfgmgr32 resource queries).
  DEVINST devinst = 0;
  // PnP device instance ID string (diagnostics only).
  std::wstring pnp_instance_id;
  std::wstring friendly_name; // optional
  std::wstring service;       // SPDRP_SERVICE (bound driver service name)
  std::vector<std::wstring> hardware_ids; // SPDRP_HARDWAREID (optional; for debugging/contract checks)
};

struct VirtioNetCtrlVqDiag {
  uint64_t host_features = 0;
  uint64_t guest_features = 0;
  DWORD ctrl_vq_negotiated = 0;
  DWORD ctrl_rx_negotiated = 0;
  DWORD ctrl_vlan_negotiated = 0;
  DWORD ctrl_mac_addr_negotiated = 0;
  DWORD ctrl_vq_queue_index = 0;
  DWORD ctrl_vq_queue_size = 0;
  uint64_t cmd_sent = 0;
  uint64_t cmd_ok = 0;
  uint64_t cmd_err = 0;
  uint64_t cmd_timeout = 0;
};

static std::optional<DWORD> ReadRegDword(HKEY key, const wchar_t* name) {
  if (!key || !name) return std::nullopt;
  DWORD type = 0;
  DWORD value = 0;
  DWORD size = sizeof(value);
  const LONG rc = RegQueryValueExW(key, name, nullptr, &type, reinterpret_cast<LPBYTE>(&value), &size);
  if (rc != ERROR_SUCCESS) return std::nullopt;
  if (type != REG_DWORD || size != sizeof(value)) return std::nullopt;
  return value;
}

static std::optional<uint64_t> ReadRegQword(HKEY key, const wchar_t* name) {
  if (!key || !name) return std::nullopt;
  DWORD type = 0;
  ULONGLONG value = 0;
  DWORD size = sizeof(value);
  const LONG rc = RegQueryValueExW(key, name, nullptr, &type, reinterpret_cast<LPBYTE>(&value), &size);
  if (rc != ERROR_SUCCESS) return std::nullopt;
  if (type != REG_QWORD || size != sizeof(value)) return std::nullopt;
  return static_cast<uint64_t>(value);
}

static std::optional<VirtioNetCtrlVqDiag> QueryVirtioNetCtrlVqDiag(Logger& log, const VirtioNetAdapter& adapter) {
  if (adapter.devinst == 0) return std::nullopt;

  HKEY dev_key = nullptr;
  CONFIGRET cr = CM_Open_DevNode_Key(adapter.devinst,
                                    KEY_READ,
                                    0,
                                    RegDisposition_OpenExisting,
                                    &dev_key,
                                    CM_REGISTRY_HARDWARE);
  if (cr != CR_SUCCESS || !dev_key) {
    log.Logf("virtio-net: ctrl_vq diag: CM_Open_DevNode_Key failed cr=%lu", static_cast<unsigned long>(cr));
    return std::nullopt;
  }

  HKEY aero_key = nullptr;
  const LONG rc = RegOpenKeyExW(dev_key, L"Device Parameters\\AeroVirtioNet", 0, KEY_READ, &aero_key);
  RegCloseKey(dev_key);
  dev_key = nullptr;

  if (rc != ERROR_SUCCESS || !aero_key) {
    return std::nullopt;
  }

  VirtioNetCtrlVqDiag out{};

  if (auto v = ReadRegQword(aero_key, L"HostFeatures")) out.host_features = *v;
  if (auto v = ReadRegQword(aero_key, L"GuestFeatures")) out.guest_features = *v;

  if (auto v = ReadRegDword(aero_key, L"CtrlVqNegotiated")) out.ctrl_vq_negotiated = *v;
  if (auto v = ReadRegDword(aero_key, L"CtrlRxNegotiated")) out.ctrl_rx_negotiated = *v;
  if (auto v = ReadRegDword(aero_key, L"CtrlVlanNegotiated")) out.ctrl_vlan_negotiated = *v;
  if (auto v = ReadRegDword(aero_key, L"CtrlMacAddrNegotiated")) out.ctrl_mac_addr_negotiated = *v;

  if (auto v = ReadRegDword(aero_key, L"CtrlVqQueueIndex")) out.ctrl_vq_queue_index = *v;
  if (auto v = ReadRegDword(aero_key, L"CtrlVqQueueSize")) out.ctrl_vq_queue_size = *v;

  if (auto v = ReadRegQword(aero_key, L"CtrlVqCmdSent")) out.cmd_sent = *v;
  if (auto v = ReadRegQword(aero_key, L"CtrlVqCmdOk")) out.cmd_ok = *v;
  if (auto v = ReadRegQword(aero_key, L"CtrlVqCmdErr")) out.cmd_err = *v;
  if (auto v = ReadRegQword(aero_key, L"CtrlVqCmdTimeout")) out.cmd_timeout = *v;

  RegCloseKey(aero_key);
  return out;
}

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
    adapter.devinst = dev.DevInst;
    adapter.hardware_ids = hwids;
    if (auto inst_id = GetDeviceInstanceIdString(devinfo, &dev)) {
      adapter.pnp_instance_id = *inst_id;
    }
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

  // GetAdaptersAddresses requires a retry loop: the adapter list can change between calls,
  // returning ERROR_BUFFER_OVERFLOW even after sizing. Treat this as best-effort (return nullopt
  // on non-recoverable errors), but retry a few times to avoid transient failures during link flaps.
  ULONG size = 0;
  const ULONG flags = GAA_FLAG_INCLUDE_PREFIX;
  ULONG rc = GetAdaptersAddresses(AF_INET, flags, nullptr, nullptr, &size);
  if (rc != ERROR_BUFFER_OVERFLOW && rc != NO_ERROR) return std::nullopt;
  if (size == 0) return std::nullopt;

  std::vector<BYTE> buf;
  IP_ADAPTER_ADDRESSES* addrs = nullptr;
  for (int attempt = 0; attempt < 4; attempt++) {
    buf.resize(size);
    addrs = reinterpret_cast<IP_ADAPTER_ADDRESSES*>(buf.data());
    rc = GetAdaptersAddresses(AF_INET, flags, nullptr, addrs, &size);
    if (rc == NO_ERROR) break;
    if (rc == ERROR_BUFFER_OVERFLOW && size != 0) continue;
    return std::nullopt;
  }
  if (rc != NO_ERROR) return std::nullopt;

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

static std::vector<IN_ADDR> GetDnsServersForAdapterGuid(const std::wstring& adapter_guid) {
  std::vector<IN_ADDR> out;

  ULONG size = 0;
  if (GetAdaptersInfo(nullptr, &size) != ERROR_BUFFER_OVERFLOW || size == 0) {
    return out;
  }

  std::vector<BYTE> buf(size);
  auto* info = reinterpret_cast<IP_ADAPTER_INFO*>(buf.data());
  if (GetAdaptersInfo(info, &size) != NO_ERROR) {
    return out;
  }

  const auto needle = NormalizeGuidLikeString(adapter_guid);

  for (auto* a = info; a != nullptr; a = a->Next) {
    const auto name = NormalizeGuidLikeString(AnsiToWide(a->AdapterName));
    if (name != needle) continue;

    std::set<uint32_t> seen;
    for (auto* dns = &a->DnsServerList; dns != nullptr; dns = dns->Next) {
      if (dns->IpAddress.String[0] == '\0') continue;

      IN_ADDR addr{};
      if (InetPtonA(AF_INET, dns->IpAddress.String, &addr) != 1) continue;
      const uint32_t host = ntohl(addr.S_un.S_addr);
      if (host == 0u) continue;
      if (seen.insert(host).second) {
        out.push_back(addr);
      }
    }

    break;
  }

  return out;
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

static std::vector<std::wstring> DnsResolveCandidates(const std::wstring& primary_host) {
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

  return candidates;
}

static bool DnsResolveWithFallback(Logger& log, const std::wstring& primary_host) {
  for (const auto& host : DnsResolveCandidates(primary_host)) {
    if (DnsResolve(log, host)) return true;
  }
  return false;
}

static bool BuildDnsQueryPacket(const std::string& host, uint16_t txid, std::vector<uint8_t>* out) {
  if (!out) return false;
  out->clear();

  if (host.empty() || host.size() > 253) return false;

  // DNS header (12 bytes).
  auto push16 = [&](uint16_t v) {
    out->push_back(static_cast<uint8_t>((v >> 8) & 0xFF));
    out->push_back(static_cast<uint8_t>(v & 0xFF));
  };

  push16(txid);   // ID
  push16(0x0100); // Flags: standard query + recursion desired
  push16(1);      // QDCOUNT
  push16(0);      // ANCOUNT
  push16(0);      // NSCOUNT
  push16(0);      // ARCOUNT

  // QNAME.
  size_t start = 0;
  while (start < host.size()) {
    size_t dot = host.find('.', start);
    if (dot == std::string::npos) dot = host.size();
    if (dot == start) return false; // empty label (e.g. "..")

    const size_t label_len = dot - start;
    if (label_len == 0 || label_len > 63) return false;
    out->push_back(static_cast<uint8_t>(label_len));
    for (size_t i = 0; i < label_len; i++) {
      const unsigned char c = static_cast<unsigned char>(host[start + i]);
      // Restrict to ASCII for the selftest; IDN handling isn't needed here.
      if (c == 0 || c >= 0x80) return false;
      out->push_back(static_cast<uint8_t>(c));
    }

    start = dot + 1;
  }
  out->push_back(0); // root terminator

  // QTYPE=A, QCLASS=IN.
  push16(1);
  push16(1);
  return true;
}

static bool UdpDnsQuery(Logger& log,
                        const IN_ADDR& dns_server,
                        const std::wstring& query_host,
                        DWORD timeout_ms,
                        uint16_t* rcode_out,
                        int* bytes_sent_out,
                        int* bytes_recv_out) {
  if (rcode_out) *rcode_out = 0;
  if (bytes_sent_out) *bytes_sent_out = 0;
  if (bytes_recv_out) *bytes_recv_out = 0;

  sockaddr_in dns{};
  dns.sin_family = AF_INET;
  dns.sin_port = htons(53);
  dns.sin_addr = dns_server;

  const uint16_t txid = static_cast<uint16_t>(GetTickCount() & 0xFFFFu);
  std::vector<uint8_t> pkt;
  if (!BuildDnsQueryPacket(WideToUtf8(query_host), txid, &pkt)) {
    return false;
  }

  SOCKET s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
  if (s == INVALID_SOCKET) {
    log.Logf("virtio-net: udp dns query: socket failed err=%d", WSAGetLastError());
    return false;
  }

  const int timeout = static_cast<int>(timeout_ms);
  (void)setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char*>(&timeout), sizeof(timeout));

  const int sent =
      sendto(s, reinterpret_cast<const char*>(pkt.data()), static_cast<int>(pkt.size()), 0,
             reinterpret_cast<const sockaddr*>(&dns), sizeof(dns));
  if (sent == SOCKET_ERROR) {
    log.Logf("virtio-net: udp dns query: sendto failed err=%d", WSAGetLastError());
    closesocket(s);
    return false;
  }

  if (bytes_sent_out) *bytes_sent_out = sent;

  uint8_t resp[512];
  sockaddr_in from{};
  int from_len = sizeof(from);
  const int recvd = recvfrom(s, reinterpret_cast<char*>(resp), static_cast<int>(sizeof(resp)), 0,
                             reinterpret_cast<sockaddr*>(&from), &from_len);
  if (recvd == SOCKET_ERROR) {
    log.Logf("virtio-net: udp dns query: recvfrom failed err=%d", WSAGetLastError());
    closesocket(s);
    return false;
  }

  closesocket(s);
  if (bytes_recv_out) *bytes_recv_out = recvd;

  if (recvd < 12) {
    log.Logf("virtio-net: udp dns query: response too short bytes=%d", recvd);
    return false;
  }

  const uint16_t rid = ReadBe16(resp + 0);
  const uint16_t flags = ReadBe16(resp + 2);
  const uint16_t rcode = static_cast<uint16_t>(flags & 0x000Fu);
  if (rcode_out) *rcode_out = rcode;

  if (rid != txid) {
    log.Logf("virtio-net: udp dns query: txid mismatch sent=0x%04x recv=0x%04x", txid, rid);
    return false;
  }

  // Any response (including NXDOMAIN/SERVFAIL) is sufficient to prove UDP TX/RX is working.
  return true;
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

static bool HttpGetLargeDeterministic(Logger& log, const std::wstring& url, uint64_t* bytes_read_out,
                                      uint64_t* fnv1a64_out, double* mbps_out) {
  static const uint64_t kExpectedBytes = 1024ull * 1024ull;
  // FNV-1a 64-bit hash of bytes 0..255 repeated to 1 MiB.
  static const uint64_t kExpectedHash = 0x8505ae4435522325ull;
  static const uint64_t kFnvOffsetBasis = 14695981039346656037ull; // 0xcbf29ce484222325

  if (bytes_read_out) *bytes_read_out = 0;
  if (fnv1a64_out) *fnv1a64_out = 0;
  if (mbps_out) *mbps_out = 0.0;

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

  const bool status_ok = status >= 200 && status < 300;

  DWORD content_len = 0;
  DWORD content_len_size = sizeof(content_len);
  bool has_content_len = WinHttpQueryHeaders(request, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
                                             WINHTTP_HEADER_NAME_BY_INDEX, &content_len, &content_len_size,
                                             WINHTTP_NO_HEADER_INDEX) != 0;

  auto query_header_string = [&](DWORD query) -> std::wstring {
    DWORD needed = 0;
    WinHttpQueryHeaders(request, query, WINHTTP_HEADER_NAME_BY_INDEX, WINHTTP_NO_OUTPUT_BUFFER, &needed,
                        WINHTTP_NO_HEADER_INDEX);
    const DWORD err = GetLastError();
    if (err != ERROR_INSUFFICIENT_BUFFER || needed == 0) return L"";

    std::vector<wchar_t> buf((needed / sizeof(wchar_t)) + 1, L'\0');
    DWORD size = needed;
    if (!WinHttpQueryHeaders(request, query, WINHTTP_HEADER_NAME_BY_INDEX, buf.data(), &size,
                             WINHTTP_NO_HEADER_INDEX)) {
      return L"";
    }
    return std::wstring(buf.data());
  };

  const std::wstring content_type = query_header_string(WINHTTP_QUERY_CONTENT_TYPE);
  const std::wstring etag = query_header_string(WINHTTP_QUERY_ETAG);
  if (!content_type.empty() || !etag.empty()) {
    log.Logf("virtio-net: HTTP GET large headers content_type=%s etag=%s",
             content_type.empty() ? "-" : WideToUtf8(content_type).c_str(),
             etag.empty() ? "-" : WideToUtf8(etag).c_str());
  }
  if (status_ok && !content_type.empty() && !StartsWithInsensitive(content_type, L"application/octet-stream")) {
    log.Logf("virtio-net: HTTP GET large unexpected Content-Type: %s", WideToUtf8(content_type).c_str());
  }
  if (status_ok && !etag.empty()) {
    // Best-effort: accept weak ETags and quoted values. Only used for logging/hints; the
    // pass/fail criteria remains size+hash.
    std::wstring e = etag;
    // Trim whitespace.
    while (!e.empty() && iswspace(e.front())) e.erase(e.begin());
    while (!e.empty() && iswspace(e.back())) e.pop_back();
    if (StartsWithInsensitive(e, L"W/")) e.erase(0, 2);
    if (!e.empty() && e.front() == L'"') e.erase(e.begin());
    if (!e.empty() && e.back() == L'"') e.pop_back();
    e = ToLower(std::move(e));
    if (!e.empty() && e != L"8505ae4435522325") {
      log.Logf("virtio-net: HTTP GET large unexpected ETag token=%s expected=%s", WideToUtf8(e).c_str(),
               "8505ae4435522325");
    }
  }

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
      if (total_read > kExpectedBytes) {
        log.Logf("virtio-net: HTTP GET large read exceeded expected size bytes_read=%llu expected=%llu",
                 static_cast<unsigned long long>(total_read),
                 static_cast<unsigned long long>(kExpectedBytes));
        read_ok = false;
        break;
      }
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
  if (bytes_read_out) *bytes_read_out = total_read;
  if (fnv1a64_out) *fnv1a64_out = hash;
  if (mbps_out) *mbps_out = mbps;
  log.Logf("virtio-net: HTTP GET large done url=%s status=%lu bytes_read=%llu sec=%.2f mbps=%.2f "
           "fnv1a64=0x%016I64x%s",
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
    log.Logf("virtio-net: HTTP GET large body mismatch bytes_read=%llu expected_bytes=%llu hash=0x%016I64x "
             "expected_hash=0x%016I64x",
             static_cast<unsigned long long>(total_read), static_cast<unsigned long long>(kExpectedBytes),
             static_cast<unsigned long long>(hash), static_cast<unsigned long long>(kExpectedHash));
    return false;
  }

  log.LogLine("virtio-net: HTTP GET large ok (size+hash match)");
  return true;
}

static bool HttpPostLargeDeterministic(Logger& log, const std::wstring& url, uint64_t* bytes_sent_out,
                                       double* mbps_out) {
  static const uint64_t kExpectedBytes = 1024ull * 1024ull;

  if (bytes_sent_out) *bytes_sent_out = 0;
  if (mbps_out) *mbps_out = 0.0;

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
  HINTERNET request = WinHttpOpenRequest(connect, L"POST", path.c_str(), nullptr, WINHTTP_NO_REFERER,
                                         WINHTTP_DEFAULT_ACCEPT_TYPES, flags);
  if (!request) {
    log.Logf("virtio-net: WinHttpOpenRequest(POST) failed err=%lu", GetLastError());
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  static const wchar_t kHeaders[] = L"Content-Type: application/octet-stream\r\n";
  if (!WinHttpSendRequest(request, kHeaders, static_cast<DWORD>(-1), WINHTTP_NO_REQUEST_DATA, 0,
                          static_cast<DWORD>(kExpectedBytes), 0)) {
    log.Logf("virtio-net: WinHttpSendRequest(POST) failed err=%lu", GetLastError());
    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  uint64_t total_written = 0;
  std::vector<uint8_t> buf(64 * 1024);
  PerfTimer timer;
  bool write_ok = true;

  while (total_written < kExpectedBytes) {
    const size_t remaining = static_cast<size_t>(kExpectedBytes - total_written);
    const size_t n = std::min(remaining, buf.size());
    for (size_t i = 0; i < n; i++) {
      buf[i] = static_cast<uint8_t>((total_written + i) & 0xFFu);
    }

    // WinHTTP is expected to write the full requested length in synchronous mode, but handle
    // partial writes defensively so we never accidentally skip bytes in the deterministic payload.
    size_t sent = 0;
    while (sent < n) {
      DWORD written = 0;
      const DWORD to_write = static_cast<DWORD>(std::min<size_t>(n - sent, UINT_MAX));
      if (!WinHttpWriteData(request, buf.data() + sent, to_write, &written)) {
        log.Logf("virtio-net: WinHttpWriteData failed err=%lu", GetLastError());
        write_ok = false;
        break;
      }
      if (written == 0) {
        log.LogLine("virtio-net: WinHttpWriteData returned 0 bytes written");
        write_ok = false;
        break;
      }
      sent += static_cast<size_t>(written);
      total_written += static_cast<uint64_t>(written);
    }
    if (!write_ok) break;
  }

  if (bytes_sent_out) *bytes_sent_out = total_written;

  if (!write_ok || total_written != kExpectedBytes) {
    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return false;
  }

  if (!WinHttpReceiveResponse(request, nullptr)) {
    log.Logf("virtio-net: WinHttpReceiveResponse(POST) failed err=%lu", GetLastError());
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

  // Read a small response body for diagnostics.
  std::string resp;
  DWORD avail = 0;
  if (WinHttpQueryDataAvailable(request, &avail) && avail > 0) {
    std::vector<char> tmp(std::min<DWORD>(avail, 256));
    DWORD read = 0;
    if (WinHttpReadData(request, tmp.data(), static_cast<DWORD>(tmp.size()), &read) && read > 0) {
      resp.assign(tmp.data(), tmp.data() + read);
    }
  }

  WinHttpCloseHandle(request);
  WinHttpCloseHandle(connect);
  WinHttpCloseHandle(session);

  const double sec = std::max(0.000001, timer.SecondsSinceStart());
  const double mbps = (static_cast<double>(total_written) / (1024.0 * 1024.0)) / sec;
  if (mbps_out) *mbps_out = mbps;

  log.Logf("virtio-net: HTTP POST large done url=%s status=%lu bytes_sent=%llu sec=%.2f mbps=%.2f resp=%s",
           WideToUtf8(url).c_str(), status, static_cast<unsigned long long>(total_written), sec, mbps,
           resp.empty() ? "-" : resp.c_str());

  return status >= 200 && status < 300;
}

struct VirtioNetUdpTestResult {
  bool ok = false;
  // Bytes in the datagram for the last attempted roundtrip (or 0 if not attempted).
  uint32_t bytes = 0;
  // Diagnostic: configured payload sizes.
  uint32_t small_bytes = 0;
  uint32_t mtu_bytes = 0;
  std::string fail_reason;
  int wsa_error = 0;
};

static VirtioNetUdpTestResult VirtioNetUdpEchoTest(Logger& log, USHORT port) {
  VirtioNetUdpTestResult out{};
  out.small_bytes = 32;
  out.mtu_bytes = 1400;

  // Keep bounded and deterministic: fixed destination, fixed payload(s), fixed timeouts.
  const DWORD timeout_ms = 2000;

  SOCKET s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
  if (s == INVALID_SOCKET) {
    out.fail_reason = "socket_failed";
    out.wsa_error = WSAGetLastError();
    log.Logf("virtio-net: UDP echo socket() failed wsa=%d", out.wsa_error);
    return out;
  }

  auto close_sock = [&]() {
    if (s != INVALID_SOCKET) {
      closesocket(s);
      s = INVALID_SOCKET;
    }
  };

  (void)setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char*>(&timeout_ms), sizeof(timeout_ms));
  (void)setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<const char*>(&timeout_ms), sizeof(timeout_ms));

  sockaddr_in dst{};
  dst.sin_family = AF_INET;
  dst.sin_port = htons(port);
  dst.sin_addr.S_un.S_addr = inet_addr("10.0.2.2");
  if (dst.sin_addr.S_un.S_addr == INADDR_NONE) {
    out.fail_reason = "bad_dst_addr";
    close_sock();
    return out;
  }

  if (connect(s, reinterpret_cast<const sockaddr*>(&dst), sizeof(dst)) == SOCKET_ERROR) {
    out.fail_reason = "connect_failed";
    out.wsa_error = WSAGetLastError();
    log.Logf("virtio-net: UDP echo connect(10.0.2.2:%u) failed wsa=%d", static_cast<unsigned>(port),
             out.wsa_error);
    close_sock();
    return out;
  }

  auto roundtrip = [&](uint32_t len) -> bool {
    out.bytes = len;
    std::vector<uint8_t> sendbuf(len);
    for (uint32_t i = 0; i < len; i++) {
      sendbuf[i] = static_cast<uint8_t>(i & 0xFFu);
    }

    const int sent = send(s, reinterpret_cast<const char*>(sendbuf.data()), static_cast<int>(sendbuf.size()), 0);
    if (sent == SOCKET_ERROR) {
      out.fail_reason = "send_failed";
      out.wsa_error = WSAGetLastError();
      log.Logf("virtio-net: UDP echo send(bytes=%lu) failed sent=%d wsa=%d", static_cast<unsigned long>(len),
               sent, out.wsa_error);
      return false;
    }
    if (sent != static_cast<int>(sendbuf.size())) {
      // UDP datagrams are expected to be sent atomically. Treat a short send as a failure, but do not
      // report a WSA error code since the send call did not fail with SOCKET_ERROR.
      out.fail_reason = "short_send";
      out.wsa_error = 0;
      log.Logf("virtio-net: UDP echo send(bytes=%lu) short sent=%d (expected=%lu)", static_cast<unsigned long>(len),
               sent, static_cast<unsigned long>(len));
      return false;
    }

    std::vector<uint8_t> recvbuf(len + 16);
    const int recvd = recv(s, reinterpret_cast<char*>(recvbuf.data()), static_cast<int>(recvbuf.size()), 0);
    if (recvd == SOCKET_ERROR) {
      const int err = WSAGetLastError();
      out.wsa_error = err;
      out.fail_reason = (err == WSAETIMEDOUT || err == WSAEWOULDBLOCK) ? "timeout" : "recv_failed";
      log.Logf("virtio-net: UDP echo recv(bytes=%lu) failed recvd=%d wsa=%d", static_cast<unsigned long>(len),
               recvd, err);
      return false;
    }
    if (recvd != static_cast<int>(len)) {
      // Datagram received but has an unexpected length. Treat it as a failure, but do not report a
      // WSA error code since recv() did not fail with SOCKET_ERROR.
      out.wsa_error = 0;
      out.fail_reason = "unexpected_len";
      log.Logf("virtio-net: UDP echo recv(bytes=%lu) unexpected len recvd=%d", static_cast<unsigned long>(len), recvd);
      return false;
    }

    if (memcmp(recvbuf.data(), sendbuf.data(), len) != 0) {
      out.fail_reason = "mismatch";
      out.wsa_error = 0;
      // Log a small prefix of both buffers for debugging.
      const size_t prefix = std::min<size_t>(32, len);
      log.Logf("virtio-net: UDP echo mismatch bytes=%lu prefix_sent=%s prefix_recv=%s",
               static_cast<unsigned long>(len), HexDump(sendbuf.data(), prefix).c_str(),
               HexDump(recvbuf.data(), prefix).c_str());
      return false;
    }

    return true;
  };

  if (!roundtrip(out.small_bytes)) {
    close_sock();
    return out;
  }
  if (!roundtrip(out.mtu_bytes)) {
    close_sock();
    return out;
  }

  close_sock();
  out.ok = true;
  out.fail_reason.clear();
  out.wsa_error = 0;
  log.Logf("virtio-net: UDP echo ok port=%u bytes_small=%lu bytes_mtu=%lu", static_cast<unsigned>(port),
           static_cast<unsigned long>(out.small_bytes), static_cast<unsigned long>(out.mtu_bytes));
  return out;
}

struct VirtioNetTestResult {
  bool ok = false;
  // Best-effort devnode for the selected adapter (if any).
  DEVINST devinst = 0;
  VirtioNetUdpTestResult udp;
  bool large_ok = false;
  uint64_t large_bytes = 0;
  uint64_t large_hash = 0;
  double large_mbps = 0.0;
  bool upload_ok = false;
  uint64_t upload_bytes = 0;
  double upload_mbps = 0.0;
  // Best-effort diagnostic: number of allocated message-signaled interrupt
  // vectors (0 means INTx; -1 means query failed).
  int msi_messages = -1;
  bool link_flap_ok = true;
};

struct VirtioNetLinkFlapTestResult {
  bool ok = false;
  std::string reason;
  double down_sec = 0.0;
  double up_sec = 0.0;
  std::optional<IN_ADDR> ip_after;
};

static VirtioNetLinkFlapTestResult VirtioNetLinkFlapTest(Logger& log, const Options& opt,
                                                         const VirtioNetAdapter& adapter) {
  VirtioNetLinkFlapTestResult out{};
  if (adapter.instance_id.empty()) {
    out.reason = "missing_adapter_guid";
    return out;
  }

  log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY");

  // Wait for link to go down (host harness toggles QMP set_link down/up).
  const DWORD down_start = GetTickCount();
  const DWORD down_deadline = down_start + 30000;
  bool saw_down = false;
  int down_samples = 0;
  while (static_cast<int32_t>(GetTickCount() - down_deadline) < 0) {
    bool up = false;
    std::wstring friendly;
    (void)FindIpv4AddressForAdapterGuid(adapter.instance_id, &up, &friendly);
    // Require multiple consecutive "down" observations to reduce false positives from transient
    // adapter enumeration failures during link transitions.
    if (!up) {
      down_samples++;
    } else {
      down_samples = 0;
    }
    if (down_samples >= 2) {
      saw_down = true;
      break;
    }
    Sleep(200);
  }
  out.down_sec = (GetTickCount() - down_start) / 1000.0;
  if (!saw_down) {
    out.reason = "timeout_waiting_link_down";
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=%s", out.reason.c_str());
    return out;
  }

  // Wait for link to come back up with a valid non-APIPA IPv4.
  const DWORD up_start = GetTickCount();
  const DWORD up_deadline = up_start + (opt.net_timeout_sec * 1000);
  bool saw_up = false;
  IN_ADDR ip_after{};
  while (static_cast<int32_t>(GetTickCount() - up_deadline) < 0) {
    bool up = false;
    std::wstring friendly;
    const auto ip = FindIpv4AddressForAdapterGuid(adapter.instance_id, &up, &friendly);
    if (up && ip.has_value()) {
      ip_after = *ip;
      out.ip_after = *ip;
      saw_up = true;
      break;
    }
    Sleep(500);
  }
  out.up_sec = (GetTickCount() - up_start) / 1000.0;
  if (!saw_up) {
    out.reason = "timeout_waiting_link_up";
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=%s", out.reason.c_str());
    return out;
  }

  // Confirm datapath after recovery.
  if (!HttpGet(log, opt.http_url)) {
    out.reason = "http_get_failed";
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=%s", out.reason.c_str());
    return out;
  }

  const uint32_t host = ntohl(ip_after.S_un.S_addr);
  const uint8_t a = static_cast<uint8_t>((host >> 24) & 0xFF);
  const uint8_t b = static_cast<uint8_t>((host >> 16) & 0xFF);
  const uint8_t c = static_cast<uint8_t>((host >> 8) & 0xFF);
  const uint8_t d = static_cast<uint8_t>(host & 0xFF);

  out.ok = true;
  out.reason.clear();
  log.Logf(
      "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS|down_sec=%.2f|up_sec=%.2f|ipv4=%u.%u.%u.%u",
      out.down_sec, out.up_sec, a, b, c, d);
  return out;
}

static VirtioNetTestResult VirtioNetTest(Logger& log, const Options& opt) {
  VirtioNetTestResult out{};
  out.link_flap_ok = !opt.test_net_link_flap;
  const auto adapters = DetectVirtioNetAdapters(log);
  if (adapters.empty()) {
    log.LogLine("virtio-net: no virtio net adapters detected");
    return out;
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
    return out;
  }
  out.devinst = chosen->devinst;

  // Emit ctrl_vq diagnostics as early as possible (even if the adapter is later
  // rejected due to binding/contract checks) so bring-up logs always include
  // feature negotiation state when available.
  {
    const auto irq = QueryDevInstIrqModeWithParentFallback(chosen->devinst);
    if (irq.ok) {
      out.msi_messages = irq.info.is_msi ? static_cast<int>(irq.info.messages) : 0;
      log.Logf("virtio-net: interrupt mode=%s message_count=%d", irq.info.is_msi ? "MSI" : "INTx",
               out.msi_messages);
    } else {
      log.LogLine("virtio-net: interrupt mode query failed");
    }
  }

  if (const auto diag = QueryVirtioNetCtrlVqDiag(log, *chosen)) {
    log.Logf("virtio-net-ctrl-vq|INFO|host_features=0x%016I64x|guest_features=0x%016I64x|ctrl_vq=%lu|ctrl_rx=%lu|ctrl_vlan=%lu|ctrl_mac_addr=%lu|queue_index=%lu|queue_size=%lu|cmd_sent=%llu|cmd_ok=%llu|cmd_err=%llu|cmd_timeout=%llu",
             static_cast<unsigned long long>(diag->host_features),
             static_cast<unsigned long long>(diag->guest_features),
             static_cast<unsigned long>(diag->ctrl_vq_negotiated),
             static_cast<unsigned long>(diag->ctrl_rx_negotiated),
             static_cast<unsigned long>(diag->ctrl_vlan_negotiated),
             static_cast<unsigned long>(diag->ctrl_mac_addr_negotiated),
             static_cast<unsigned long>(diag->ctrl_vq_queue_index),
             static_cast<unsigned long>(diag->ctrl_vq_queue_size),
             static_cast<unsigned long long>(diag->cmd_sent),
             static_cast<unsigned long long>(diag->cmd_ok),
             static_cast<unsigned long long>(diag->cmd_err),
             static_cast<unsigned long long>(diag->cmd_timeout));
  } else {
    log.LogLine("virtio-net-ctrl-vq|INFO|diag_unavailable");
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
    return out;
  }
  if (!contract_rev01) {
    log.LogLine("virtio-net: note: contract HWID matched but no &REV_01 entry was found");
  }

  const auto dhcp_enabled = IsDhcpEnabledForAdapterGuid(chosen->instance_id);
  if (!dhcp_enabled.has_value()) {
    log.LogLine("virtio-net: failed to query DHCP enabled state");
    return out;
  }
  if (!*dhcp_enabled) {
    log.LogLine("virtio-net: DHCP is not enabled for the virtio adapter");
    return out;
  }

  const uint32_t host = ntohl(chosen_ip.S_un.S_addr);
  const uint8_t a = static_cast<uint8_t>((host >> 24) & 0xFF);
  const uint8_t b = static_cast<uint8_t>((host >> 16) & 0xFF);
  const uint8_t c = static_cast<uint8_t>((host >> 8) & 0xFF);
  const uint8_t d = static_cast<uint8_t>(host & 0xFF);
  log.Logf("virtio-net: adapter up name=%s guid=%s ipv4=%u.%u.%u.%u",
           WideToUtf8(chosen_friendly).c_str(), WideToUtf8(chosen->instance_id).c_str(), a, b, c,
           d);

  out.udp = VirtioNetUdpEchoTest(log, opt.udp_port);
  if (!out.udp.ok) return out;

  if (!DnsResolveWithFallback(log, opt.dns_host)) return out;

  // Best-effort UDP traffic test (exercises UDP TX/RX on the virtio-net path, which is where
  // UDP checksum offload matters). This does not affect overall PASS/FAIL.
  {
    const auto servers = GetDnsServersForAdapterGuid(chosen->instance_id);
    if (servers.empty()) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|SKIP|no_dns_server");
    } else {
      bool udp_ok = false;
      IN_ADDR used_server{};
      std::wstring used_host;
      uint16_t used_rcode = 0;
      int bytes_sent = 0;
      int bytes_recv = 0;

      for (const auto& s : servers) {
        for (const auto& h : DnsResolveCandidates(opt.dns_host)) {
          if (UdpDnsQuery(log, s, h, 2000, &used_rcode, &bytes_sent, &bytes_recv)) {
            udp_ok = true;
            used_server = s;
            used_host = h;
            break;
          }
        }
        if (udp_ok) break;
      }

      const uint32_t dns_host = ntohl(used_server.S_un.S_addr);
      const uint8_t da = static_cast<uint8_t>((dns_host >> 24) & 0xFF);
      const uint8_t db = static_cast<uint8_t>((dns_host >> 16) & 0xFF);
      const uint8_t dc = static_cast<uint8_t>((dns_host >> 8) & 0xFF);
      const uint8_t dd = static_cast<uint8_t>(dns_host & 0xFF);

      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|%s|server=%u.%u.%u.%u|query=%s|sent=%d|recv=%d|rcode=%u",
               udp_ok ? "PASS" : "FAIL",
               da,
               db,
               dc,
               dd,
               udp_ok ? WideToUtf8(used_host).c_str() : "-",
               bytes_sent,
               bytes_recv,
               static_cast<unsigned>(used_rcode));
    }
  }

  if (!HttpGet(log, opt.http_url)) return out;

  if (opt.test_net_link_flap && chosen.has_value()) {
    const auto flap = VirtioNetLinkFlapTest(log, opt, *chosen);
    out.link_flap_ok = flap.ok;
  }

  const std::wstring large_url = UrlAppendSuffix(opt.http_url, L"-large");
  out.large_ok = HttpGetLargeDeterministic(log, large_url, &out.large_bytes, &out.large_hash, &out.large_mbps);
  if (!out.large_ok) return out;

  out.upload_ok = HttpPostLargeDeterministic(log, large_url, &out.upload_bytes, &out.upload_mbps);
  if (!out.upload_ok) return out;

  out.ok = true;
  return out;
}

static std::optional<AEROVNET_OFFLOAD_STATS> QueryAerovnetOffloadStats(Logger& log) {
  HANDLE h = CreateFileW(kAerovnetOffloadDevicePath, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                         OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
  if (h == INVALID_HANDLE_VALUE) {
    log.Logf("virtio-net: open AeroVirtioNet control device failed err=%lu", GetLastError());
    return std::nullopt;
  }

  AEROVNET_OFFLOAD_STATS stats{};
  DWORD bytes = 0;
  const BOOL ok = DeviceIoControl(h, kAerovnetIoctlQueryOffloadStats, nullptr, 0, &stats, sizeof(stats), &bytes,
                                  nullptr);
  const DWORD err = ok ? ERROR_SUCCESS : GetLastError();
  CloseHandle(h);

  if (!ok) {
    log.Logf("virtio-net: IOCTL query offload stats failed err=%lu", static_cast<unsigned long>(err));
    return std::nullopt;
  }
  if (bytes < sizeof(AEROVNET_OFFLOAD_STATS)) {
    log.Logf("virtio-net: IOCTL query offload stats returned too few bytes=%lu", bytes);
    return std::nullopt;
  }
  if (stats.Size < sizeof(AEROVNET_OFFLOAD_STATS)) {
    log.Logf("virtio-net: IOCTL query offload stats returned Size=%lu (expected >=%zu)",
             static_cast<unsigned long>(stats.Size), sizeof(AEROVNET_OFFLOAD_STATS));
    return std::nullopt;
  }
  return stats;
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

  unsigned valid_bits = fmt->wBitsPerSample;
  if (WaveFormatIsExtensible(fmt)) {
    const auto* ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(fmt);
    if (ext->Samples.wValidBitsPerSample != 0 && ext->Samples.wValidBitsPerSample <= fmt->wBitsPerSample) {
      valid_bits = static_cast<unsigned>(ext->Samples.wValidBitsPerSample);
    }
  }

  char buf[256];
  snprintf(buf, sizeof(buf), "tag=0x%04x type=%s rate=%lu ch=%u bits=%u valid=%u align=%u",
           static_cast<unsigned>(fmt->wFormatTag), type, static_cast<unsigned long>(fmt->nSamplesPerSec),
           static_cast<unsigned>(fmt->nChannels), static_cast<unsigned>(fmt->wBitsPerSample), valid_bits,
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

  if (is_float && bytes_per_sample != 4 && bytes_per_sample != 8) return false;
  if (is_pcm && bytes_per_sample != 1 && bytes_per_sample != 2 && bytes_per_sample != 3 &&
      bytes_per_sample != 4) {
    return false;
  }

  // Respect WAVEFORMATEXTENSIBLE valid-bits when generating 32-bit container samples (e.g. 24-in-32).
  // This keeps generated tones within the advertised numeric range.
  WORD valid_bits = fmt->wBitsPerSample;
  if (WaveFormatIsExtensible(fmt)) {
    const auto* ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(fmt);
    if (ext->Samples.wValidBitsPerSample != 0 && ext->Samples.wValidBitsPerSample <= fmt->wBitsPerSample) {
      valid_bits = ext->Samples.wValidBitsPerSample;
    }
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
        if (bytes_per_sample == 4) {
          const float v = static_cast<float>(sample);
          memcpy(out, &v, sizeof(v));
        } else {
          const double v = sample;
          memcpy(out, &v, sizeof(v));
        }
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
        if (valid_bits < 2) {
          return false;
        }

        // For integer PCM in WAVEFORMATEXTENSIBLE, valid bits are typically
        // left-aligned within the container (with low-order padding bits set to
        // zero). Keep the generated samples aligned to the MSBs so downstream
        // WAV verification (which often treats 32-bit PCM as full-scale int32)
        // sees the expected amplitude for 24-in-32.
        const WORD container_bits = static_cast<WORD>(bytes_per_sample * 8u);
        WORD shift = 0;
        if (valid_bits < container_bits) {
          shift = static_cast<WORD>(container_bits - valid_bits);
          if (shift >= 32) return false;
        }

        // Scale using the valid-bit width and then shift into the container.
        // Example: valid_bits=24 in a 32-bit container => scale to +/-8388607
        // then shift left by 8 so the LSB byte is padding.
        const double scale = (valid_bits >= 32) ? 2147483647.0 : (std::pow(2.0, static_cast<int>(valid_bits) - 1) - 1.0);
        int64_t v64 = static_cast<int64_t>(std::llround(clamped * scale));
        v64 <<= shift;
        if (v64 > INT32_MAX) v64 = INT32_MAX;
        if (v64 < INT32_MIN) v64 = INT32_MIN;
        const int32_t v = static_cast<int32_t>(v64);
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

static size_t WaveFormatTotalSizeBytes(const WAVEFORMATEX* fmt);
static std::vector<BYTE> CopyWaveFormatBytes(const WAVEFORMATEX* fmt);

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
  bool used_mix_format = false;

  WAVEFORMATEX* mix = nullptr;
  hr = client->GetMixFormat(&mix);
  if (SUCCEEDED(hr) && mix) {
    out.mix_format = WaveFormatToString(mix);
    log.Logf("virtio-snd: mix format=%s", out.mix_format.c_str());

    fmt_bytes = CopyWaveFormatBytes(mix);
    CoTaskMemFree(mix);
    mix = nullptr;

    if (fmt_bytes.empty()) {
      out.fail_reason = "invalid_mix_format";
      out.hr = E_FAIL;
      log.LogLine("virtio-snd: GetMixFormat returned an invalid format header");
      return out;
    }

    used_mix_format = true;
    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0,
                            reinterpret_cast<WAVEFORMATEX*>(fmt_bytes.data()), nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_shared_failed";
      out.hr = hr;
      log.Logf("virtio-snd: Initialize(shared mix format) failed hr=0x%08lx", static_cast<unsigned long>(hr));
      return out;
    }
  } else {
    log.Logf("virtio-snd: GetMixFormat failed hr=0x%08lx; falling back to 48kHz S16 stereo",
             static_cast<unsigned long>(hr));

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
        log.Logf("virtio-snd: Initialize(shared desired extensible) failed hr=0x%08lx",
                 static_cast<unsigned long>(hr));
        return out;
      }
    }

    out.mix_format = WaveFormatToString(reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data()));
  }

  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  log.Logf("virtio-snd: stream format=%s used_mix=%d", WaveFormatToString(fmt).c_str(), used_mix_format ? 1 : 0);
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
  log.Logf("virtio-snd: render smoke ok (format=%s, used_mix=%d)", WaveFormatToString(fmt).c_str(),
           used_mix_format ? 1 : 0);
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
    // Best-effort: even if we're only checking for endpoint presence, query the
    // shared-mode mix format so the overall virtio-snd-format marker can still
    // surface the driver's negotiated tuple. Do not fail the test if this
    // probing fails.
    ComPtr<IAudioClient> probe;
    hr = chosen->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                          reinterpret_cast<void**>(probe.Put()));
    if (SUCCEEDED(hr) && probe) {
      WAVEFORMATEX* mix = nullptr;
      HRESULT mix_hr = probe->GetMixFormat(&mix);
      if (SUCCEEDED(mix_hr) && mix) {
        out.mix_format = WaveFormatToString(mix);
        log.Logf("virtio-snd: capture mix format=%s", out.mix_format.c_str());
        CoTaskMemFree(mix);
        mix = nullptr;
      } else {
        log.Logf("virtio-snd: capture GetMixFormat failed hr=0x%08lx", static_cast<unsigned long>(mix_hr));
      }
    } else {
      log.Logf("virtio-snd: capture Activate(IAudioClient) for mix format failed hr=0x%08lx",
               static_cast<unsigned long>(hr));
    }

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

  constexpr REFERENCE_TIME kBufferDuration100ms = 1000000; // 100ms in 100ns units

  std::vector<BYTE> fmt_bytes;
  bool used_mix_format = false;

  WAVEFORMATEX* mix = nullptr;
  hr = client->GetMixFormat(&mix);
  if (SUCCEEDED(hr) && mix) {
    out.mix_format = WaveFormatToString(mix);
    log.Logf("virtio-snd: capture mix format=%s", out.mix_format.c_str());

    fmt_bytes = CopyWaveFormatBytes(mix);
    CoTaskMemFree(mix);
    mix = nullptr;

    if (fmt_bytes.empty()) {
      out.fail_reason = "invalid_mix_format";
      out.hr = E_FAIL;
      log.LogLine("virtio-snd: capture GetMixFormat returned an invalid format header");
      return out;
    }

    used_mix_format = true;
    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0,
                            reinterpret_cast<WAVEFORMATEX*>(fmt_bytes.data()), nullptr);
    if (FAILED(hr)) {
      out.fail_reason = "initialize_fixed_failed";
      out.hr = hr;
      log.Logf("virtio-snd: capture Initialize(shared mix format) failed hr=0x%08lx", static_cast<unsigned long>(hr));
      return out;
    }
  } else {
    log.Logf("virtio-snd: capture GetMixFormat failed hr=0x%08lx; falling back to 48kHz S16 mono",
             static_cast<unsigned long>(hr));

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

    log.Logf("virtio-snd: capture desired fallback format=%s", WaveFormatToString(desired).c_str());

    hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0, desired, nullptr);
    if (FAILED(hr)) {
      log.Logf(
          "virtio-snd: capture Initialize(shared desired 48kHz S16 mono) failed hr=0x%08lx; trying WAVE_FORMAT_EXTENSIBLE",
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

    out.mix_format = WaveFormatToString(reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data()));
  }

  const auto* fmt = reinterpret_cast<const WAVEFORMATEX*>(fmt_bytes.data());
  log.Logf("virtio-snd: capture stream format=%s used_mix=%d", WaveFormatToString(fmt).c_str(),
           used_mix_format ? 1 : 0);
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

  std::vector<BYTE> render_fmt_bytes;
  std::vector<BYTE> capture_fmt_bytes;
  bool render_used_mix = false;
  bool capture_used_mix = false;

  // Render: use shared-mode mix format so the test follows the driver's negotiated capabilities.
  {
    WAVEFORMATEX* mix = nullptr;
    hr = render_client->GetMixFormat(&mix);
    if (SUCCEEDED(hr) && mix) {
      log.Logf("virtio-snd: duplex render mix format=%s", WaveFormatToString(mix).c_str());
      render_fmt_bytes = CopyWaveFormatBytes(mix);
      CoTaskMemFree(mix);
      mix = nullptr;

      if (render_fmt_bytes.empty()) {
        out.fail_reason = "invalid_render_mix_format";
        out.hr = E_FAIL;
        log.LogLine("virtio-snd: duplex render GetMixFormat returned an invalid format header");
        return out;
      }

      render_used_mix = true;
      hr = render_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0,
                                     reinterpret_cast<WAVEFORMATEX*>(render_fmt_bytes.data()), nullptr);
      if (FAILED(hr)) {
        out.fail_reason = "initialize_render_shared_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex render Initialize(shared mix format) failed hr=0x%08lx",
                 static_cast<unsigned long>(hr));
        return out;
      }
    } else {
      log.Logf("virtio-snd: duplex render GetMixFormat failed hr=0x%08lx; falling back to 48kHz S16 stereo",
               static_cast<unsigned long>(hr));

      render_fmt_bytes.resize(sizeof(WAVEFORMATEX));
      auto* render_desired = reinterpret_cast<WAVEFORMATEX*>(render_fmt_bytes.data());
      *render_desired = {};
      render_desired->wFormatTag = WAVE_FORMAT_PCM;
      render_desired->nChannels = 2;
      render_desired->nSamplesPerSec = 48000;
      render_desired->wBitsPerSample = 16;
      render_desired->nBlockAlign =
          static_cast<WORD>((render_desired->nChannels * render_desired->wBitsPerSample) / 8);
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
    }
  }

  // Capture: use shared-mode mix format.
  {
    WAVEFORMATEX* mix = nullptr;
    hr = capture_client->GetMixFormat(&mix);
    if (SUCCEEDED(hr) && mix) {
      log.Logf("virtio-snd: duplex capture mix format=%s", WaveFormatToString(mix).c_str());
      capture_fmt_bytes = CopyWaveFormatBytes(mix);
      CoTaskMemFree(mix);
      mix = nullptr;

      if (capture_fmt_bytes.empty()) {
        out.fail_reason = "invalid_capture_mix_format";
        out.hr = E_FAIL;
        log.LogLine("virtio-snd: duplex capture GetMixFormat returned an invalid format header");
        return out;
      }

      capture_used_mix = true;
      hr = capture_client->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, kBufferDuration100ms, 0,
                                      reinterpret_cast<WAVEFORMATEX*>(capture_fmt_bytes.data()), nullptr);
      if (FAILED(hr)) {
        out.fail_reason = "initialize_capture_shared_failed";
        out.hr = hr;
        log.Logf("virtio-snd: duplex capture Initialize(shared mix format) failed hr=0x%08lx",
                 static_cast<unsigned long>(hr));
        return out;
      }
    } else {
      log.Logf("virtio-snd: duplex capture GetMixFormat failed hr=0x%08lx; falling back to 48kHz S16 mono",
               static_cast<unsigned long>(hr));

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
    }
  }

  const auto* render_fmt = reinterpret_cast<const WAVEFORMATEX*>(render_fmt_bytes.data());
  const auto* capture_fmt = reinterpret_cast<const WAVEFORMATEX*>(capture_fmt_bytes.data());
  log.Logf("virtio-snd: duplex render stream format=%s used_mix=%d", WaveFormatToString(render_fmt).c_str(),
           render_used_mix ? 1 : 0);
  log.Logf("virtio-snd: duplex capture stream format=%s used_mix=%d", WaveFormatToString(capture_fmt).c_str(),
           capture_used_mix ? 1 : 0);

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
      "  --expect-blk-msi          Fail virtio-blk test if still using INTx (expected MSI/MSI-X)\n"
      "  --test-blk-resize         Run virtio-blk runtime resize test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE=1)\n"
      "  --test-blk-reset          Run virtio-blk miniport reset/recovery test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_BLK_RESET=1)\n"
      "  --http-url <url>          HTTP URL for TCP connectivity test (also expects <url>-large)\n"
      "  --udp-port <port>         UDP echo server port for virtio-net UDP smoke test (host is 10.0.2.2)\n"
      "  --dns-host <hostname>     Hostname for DNS resolution test\n"
      "  --log-file <path>         Log file path (default C:\\\\aero-virtio-selftest.log)\n"
      "  --disable-snd             Skip virtio-snd test (emit SKIP)\n"
      "  --disable-snd-capture     Skip virtio-snd capture test (emit SKIP)\n"
      "  --require-snd             Fail if virtio-snd is missing (default: SKIP)\n"
      "  --test-snd                Alias for --require-snd\n"
      "  --test-input-events       Run virtio-input end-to-end HID input report test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS=1)\n"
      "  --require-input-msix      Fail if virtio-input is not using MSI-X interrupts\n"
      "  --test-input-events-extended  Also test modifiers/buttons/wheel via additional markers:\n"
      "                           virtio-input-events-modifiers/buttons/wheel\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED=1)\n"
      "  --test-input-events-modifiers  Enable virtio-input-events-modifiers subtest\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_MODIFIERS=1)\n"
      "  --test-input-events-buttons    Enable virtio-input-events-buttons subtest\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_BUTTONS=1)\n"
      "  --test-input-events-wheel      Enable virtio-input-events-wheel subtest\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_WHEEL=1)\n"
      "  --test-input-tablet-events Run virtio-input tablet (absolute pointer) HID input report test (optional)\n"
      "                           (alias: --test-tablet-events)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1 or AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1)\n"
      "  --test-input-media-keys   Run virtio-input Consumer Control (media keys) HID input report test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS=1)\n"
      "  --test-input-led          Run virtio-input keyboard LED/statusq smoke test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1)\n"
      "  --test-net-link-flap      Run virtio-net link flap regression test (optional)\n"
      "                           (or set env var AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP=1)\n"
      "  --require-snd-capture     Fail if virtio-snd capture is missing (default: SKIP)\n"
      "  --test-snd-capture        Run virtio-snd capture smoke test if available (default: auto when virtio-snd is present)\n"
      "  --test-snd-buffer-limits  Run virtio-snd large WASAPI buffer/period stress test (optional)\n"
      "  --require-non-silence     Fail capture smoke test if only silence is captured\n"
      "  --allow-virtio-snd-transitional  Also accept legacy PCI\\VEN_1AF4&DEV_1018\n"
      "  --require-net-msix        Fail if virtio-net is not using MSI-X (default: allow INTx)\n"
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
    } else if (arg == L"--udp-port") {
      const wchar_t* v = next();
      const auto parsed = ParseU32(v);
      if (!parsed || *parsed == 0 || *parsed > 65535u) {
        PrintUsage();
        return 2;
      }
      opt.udp_port = static_cast<USHORT>(*parsed);
    } else if (arg == L"--blk-root") {
      const wchar_t* v = next();
      if (!v) {
        PrintUsage();
        return 2;
      }
      opt.blk_root = v;
    } else if (arg == L"--expect-blk-msi") {
      opt.expect_blk_msi = true;
    } else if (arg == L"--test-blk-resize") {
      opt.test_blk_resize = true;
    } else if (arg == L"--test-blk-reset") {
      opt.test_blk_reset = true;
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
    } else if (arg == L"--test-input-events") {
      opt.test_input_events = true;
    } else if (arg == L"--test-input-events-extended") {
      opt.test_input_events = true;
      opt.test_input_events_modifiers = true;
      opt.test_input_events_buttons = true;
      opt.test_input_events_wheel = true;
    } else if (arg == L"--test-input-events-modifiers") {
      opt.test_input_events = true;
      opt.test_input_events_modifiers = true;
    } else if (arg == L"--test-input-events-buttons") {
      opt.test_input_events = true;
      opt.test_input_events_buttons = true;
    } else if (arg == L"--test-input-events-wheel") {
      opt.test_input_events = true;
      opt.test_input_events_wheel = true;
    } else if (arg == L"--test-input-tablet-events" || arg == L"--test-tablet-events") {
      opt.test_input_tablet_events = true;
    } else if (arg == L"--test-input-media-keys") {
      opt.test_input_media_keys = true;
    } else if (arg == L"--test-input-led") {
      opt.test_input_led = true;
    } else if (arg == L"--require-input-msix") {
      opt.require_input_msix = true;
    } else if (arg == L"--test-net-link-flap") {
      opt.test_net_link_flap = true;
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
    } else if (arg == L"--require-net-msix") {
      opt.require_net_msix = true;
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

  if (!opt.test_blk_resize && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_BLK_RESIZE")) {
    opt.test_blk_resize = true;
  }

  if (!opt.test_input_events && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS")) {
    opt.test_input_events = true;
  }
  if (EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_EXTENDED")) {
    opt.test_input_events = true;
    opt.test_input_events_modifiers = true;
    opt.test_input_events_buttons = true;
    opt.test_input_events_wheel = true;
  }
  if (EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_MODIFIERS")) {
    opt.test_input_events = true;
    opt.test_input_events_modifiers = true;
  }
  if (EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_BUTTONS")) {
    opt.test_input_events = true;
    opt.test_input_events_buttons = true;
  }
  if (EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_EVENTS_WHEEL")) {
    opt.test_input_events = true;
    opt.test_input_events_wheel = true;
  }

  if (!opt.test_input_tablet_events &&
      (EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS") ||
       EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS"))) {
    opt.test_input_tablet_events = true;
  }

  if (!opt.test_input_media_keys && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_MEDIA_KEYS")) {
    opt.test_input_media_keys = true;
  }
  if (!opt.test_net_link_flap && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_NET_LINK_FLAP")) {
    opt.test_net_link_flap = true;
  }

  if (!opt.test_input_led && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_INPUT_LED")) {
    opt.test_input_led = true;
  }

  if (!opt.expect_blk_msi && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI")) {
    opt.expect_blk_msi = true;
  }

  if (!opt.test_blk_reset && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_TEST_BLK_RESET")) {
    opt.test_blk_reset = true;
  }
  if (!opt.require_net_msix && EnvVarTruthy(L"AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX")) {
    opt.require_net_msix = true;
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
  log.Logf(
      "AERO_VIRTIO_SELFTEST|CONFIG|http_url=%s|http_url_large=%s|udp_port=%lu|dns_host=%s|blk_root=%s|expect_blk_msi=%d|test_net_link_flap=%d",
      WideToUtf8(opt.http_url).c_str(),
      WideToUtf8(UrlAppendSuffix(opt.http_url, L"-large")).c_str(), static_cast<unsigned long>(opt.udp_port),
      WideToUtf8(opt.dns_host).c_str(), WideToUtf8(opt.blk_root).c_str(), opt.expect_blk_msi ? 1 : 0,
      opt.test_net_link_flap ? 1 : 0);

  bool all_ok = true;

  std::optional<AerovblkQueryInfoResult> blk_miniport_info;
  DEVINST blk_devinst = 0;
  const auto blk = VirtioBlkTest(log, opt, &blk_miniport_info, &blk_devinst);

  std::string marker = std::string("AERO_VIRTIO_SELFTEST|TEST|virtio-blk|") + (blk.ok ? "PASS" : "FAIL");

  // Populate IRQ fields for the virtio-blk per-test marker. Prefer miniport IOCTL fields when present, but
  // fall back to PnP resource inspection (via the disk devnode) so we always emit `irq_*` fields for the
  // host harness to scrape.
  std::string irq_mode = "none";
  uint32_t irq_message_count = 0;
  if (blk_devinst != 0) {
    const auto irq = QueryDevInstIrqModeWithParentFallback(blk_devinst);
    if (irq.ok) {
      irq_mode = irq.info.is_msi ? "msi" : "intx";
      irq_message_count = irq.info.is_msi ? irq.info.messages : 0;
    }
  }

  if (blk_miniport_info.has_value()) {
    // Only include interrupt diagnostics if the miniport returned the extended fields.
    constexpr size_t kIrqModeEnd = offsetof(AEROVBLK_QUERY_INFO, InterruptMode) + sizeof(ULONG);
    constexpr size_t kMsixCfgEnd = offsetof(AEROVBLK_QUERY_INFO, MsixConfigVector) + sizeof(USHORT);
    constexpr size_t kMsixQ0End = offsetof(AEROVBLK_QUERY_INFO, MsixQueue0Vector) + sizeof(USHORT);
    constexpr size_t kMsgCountEnd = offsetof(AEROVBLK_QUERY_INFO, MessageCount) + sizeof(ULONG);

    if (blk_miniport_info->returned_len >= kIrqModeEnd) {
      irq_mode = AerovblkIrqModeForMarker(blk_miniport_info->info);
      if (strcmp(irq_mode.c_str(), "intx") == 0 || strcmp(irq_mode.c_str(), "none") == 0) {
        irq_message_count = 0;
      }
    }

    if (blk_miniport_info->returned_len >= kMsgCountEnd &&
        (strcmp(irq_mode.c_str(), "msi") == 0 || strcmp(irq_mode.c_str(), "msix") == 0)) {
      irq_message_count = static_cast<uint32_t>(blk_miniport_info->info.MessageCount);
    }

    marker += "|irq_mode=";
    marker += irq_mode;
    marker += "|irq_message_count=";
    marker += std::to_string(static_cast<unsigned long>(irq_message_count));

    if (blk_miniport_info->returned_len >= kMsixCfgEnd) {
      char vec[16];
      snprintf(vec, sizeof(vec), "0x%04x", static_cast<unsigned>(blk_miniport_info->info.MsixConfigVector));
      marker += "|msix_config_vector=";
      marker += vec;
    }

    if (blk_miniport_info->returned_len >= kMsixQ0End) {
      char vec[16];
      snprintf(vec, sizeof(vec), "0x%04x", static_cast<unsigned>(blk_miniport_info->info.MsixQueue0Vector));
      marker += "|msix_queue_vector=";
      marker += vec;
    }

    /*
     * Dedicated marker for MSI/MSI-X diagnostics (used by the host harness).
     *
     * This marker is independent of the overall virtio-blk PASS/FAIL and is
     * emitted only when the extended IOCTL payload is available.
     */
    if (blk_miniport_info->returned_len >= kMsgCountEnd && blk_miniport_info->returned_len >= kMsixCfgEnd &&
        blk_miniport_info->returned_len >= kMsixQ0End) {
      const auto& info = blk_miniport_info->info;
      const char* mode = AerovblkIrqModeForMarker(info);
      // For virtio-blk modern, message-signaled interrupts imply MSI-X routing.
      if (strcmp(mode, "msi") == 0) {
        mode = "msix";
      }
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=%s|messages=%lu|config_vector=%u|queue_vector=%u",
          mode, static_cast<unsigned long>(info.MessageCount), static_cast<unsigned>(info.MsixConfigVector),
          static_cast<unsigned>(info.MsixQueue0Vector));
    } else {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|SKIP|reason=ioctl_payload_v1_or_truncated|returned_len=%zu",
               blk_miniport_info->returned_len);
    }

    /*
     * Dedicated marker for miniport recovery/reset/abort counters (used by the host harness).
     *
     * Keep this separate from the virtio-blk perf marker so its stable fields/order remain
     * unchanged for throughput regression tracking.
     */
    {
      constexpr size_t kCountersEnd = offsetof(AEROVBLK_QUERY_INFO, IoctlResetCount) + sizeof(ULONG);
      constexpr size_t kCapEventsEnd = offsetof(AEROVBLK_QUERY_INFO, CapacityChangeEvents) + sizeof(ULONG);
      if (blk_miniport_info->returned_len >= kCountersEnd) {
        const auto& info = blk_miniport_info->info;
        std::string counter_marker = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO";
        counter_marker += "|abort=";
        counter_marker += std::to_string(static_cast<unsigned long>(info.AbortSrbCount));
        counter_marker += "|reset_device=";
        counter_marker += std::to_string(static_cast<unsigned long>(info.ResetDeviceSrbCount));
        counter_marker += "|reset_bus=";
        counter_marker += std::to_string(static_cast<unsigned long>(info.ResetBusSrbCount));
        counter_marker += "|pnp=";
        counter_marker += std::to_string(static_cast<unsigned long>(info.PnpSrbCount));
        counter_marker += "|ioctl_reset=";
        counter_marker += std::to_string(static_cast<unsigned long>(info.IoctlResetCount));
        counter_marker += "|capacity_change_events=";
        if (blk_miniport_info->returned_len >= kCapEventsEnd) {
          counter_marker += std::to_string(static_cast<unsigned long>(info.CapacityChangeEvents));
        } else {
          counter_marker += "not_supported";
        }
        log.LogLine(counter_marker);
      } else {
        log.Logf(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=ioctl_payload_truncated|returned_len=%zu",
            blk_miniport_info->returned_len);
      }
    }
  } else {
    marker += "|irq_mode=";
    marker += irq_mode;
    marker += "|irq_message_count=";
    marker += std::to_string(static_cast<unsigned long>(irq_message_count));
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|SKIP|reason=no_miniport_info");
  }

  // Always include perf fields (stable ordering) so the host harness can surface throughput regressions.
  char write_mbps[32];
  char read_mbps[32];
  snprintf(write_mbps, sizeof(write_mbps), "%.2f", blk.write_mbps);
  snprintf(read_mbps, sizeof(read_mbps), "%.2f", blk.read_mbps);
  marker += "|write_ok=";
  marker += std::to_string(blk.write_ok ? 1 : 0);
  marker += "|write_bytes=";
  marker += std::to_string(static_cast<unsigned long long>(blk.write_bytes));
  marker += "|write_mbps=";
  marker += write_mbps;
  marker += "|flush_ok=";
  marker += std::to_string(blk.flush_ok ? 1 : 0);
  marker += "|read_ok=";
  marker += std::to_string(blk.read_ok ? 1 : 0);
  marker += "|read_bytes=";
  marker += std::to_string(static_cast<unsigned long long>(blk.read_bytes));
  marker += "|read_mbps=";
  marker += read_mbps;
  log.LogLine(marker);

  all_ok = all_ok && blk.ok;

  if (blk_devinst != 0) {
    EmitVirtioIrqMarkerForDevInst(log, "virtio-blk", blk_devinst);
  } else {
    EmitVirtioIrqMarker(log, "virtio-blk", {L"PCI\\VEN_1AF4&DEV_1042"});
  }

  if (!opt.test_blk_resize) {
    // Best-effort: record the miniport's resize counter for diagnostics (driver-internal).
    // This subtest requires host-side intervention to actually trigger a runtime resize.
    VirtioBlkResizeProbe(log);
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set");
  } else {
    const auto resize = VirtioBlkResizeTest(log, opt);
    if (resize.ok) {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=%lu|old_bytes=%I64u|new_bytes=%I64u|elapsed_ms=%lu",
               static_cast<unsigned long>(resize.disk_number),
               static_cast<unsigned long long>(resize.old_bytes),
               static_cast<unsigned long long>(resize.new_bytes),
               static_cast<unsigned long>(resize.elapsed_ms));
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=%s|disk=%lu|old_bytes=%I64u|last_bytes=%I64u|err=%lu",
          resize.reason.empty() ? "unknown" : resize.reason.c_str(),
          static_cast<unsigned long>(resize.disk_number),
          static_cast<unsigned long long>(resize.old_bytes),
          static_cast<unsigned long long>(resize.last_bytes),
          static_cast<unsigned long>(resize.win32_error));
    }
    all_ok = all_ok && resize.ok;
  }

  if (opt.test_blk_reset) {
    if (!blk.ok) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=blk_test_failed|err=0");
      all_ok = false;
    } else if (const auto target = SelectVirtioBlkSelection(log, opt); !target.has_value()) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=resolve_target_failed|err=0");
      all_ok = false;
    } else {
      const auto reset = VirtioBlkResetTest(log, *target);
      if (reset.skipped_not_supported) {
        log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported");
      } else if (reset.ok) {
        const std::string counter_before = reset.counter_before.has_value()
                                               ? std::to_string(static_cast<unsigned long>(*reset.counter_before))
                                               : "not_supported";
        const std::string counter_after = reset.counter_after.has_value()
                                              ? std::to_string(static_cast<unsigned long>(*reset.counter_after))
                                              : "not_supported";
        std::string marker = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=";
        marker += counter_before;
        marker += "|counter_after=";
        marker += counter_after;
        log.LogLine(marker);
      } else {
        log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=%s|err=%lu",
                 reset.fail_reason.empty() ? "unknown" : reset.fail_reason.c_str(),
                 static_cast<unsigned long>(reset.win32_error));
        all_ok = false;
      }
    }
  }

  const auto input = VirtioInputTest(log);
  const std::string input_irq_fields =
      (input.devinst != 0)
          ? IrqFieldsForTestMarkerFromDevInst(input.devinst)
          : IrqFieldsForTestMarker({L"PCI\\VEN_1AF4&DEV_1052", L"PCI\\VEN_1AF4&DEV_1011"},
                                   {L"VID_1AF4&PID_0001", L"VID_1AF4&PID_0002", L"VID_1AF4&PID_0003",
                                     L"VID_1AF4&PID_1052", L"VID_1AF4&PID_1011"});
  const std::string input_expected_service = WideToUtf8(kVirtioInputExpectedService);
  const std::string input_bind_pnp_id = WideToUtf8(input.pci_sample_pnp_id);
  const std::string input_bind_service = WideToUtf8(input.pci_sample_service);
  if (input.pci_binding_ok) {
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|service=%s|pnp_id=%s|devices=%d|wrong_service=%d|missing_service=%d|problem=%d",
        input_expected_service.c_str(), input_bind_pnp_id.empty() ? "-" : input_bind_pnp_id.c_str(),
        input.pci_devices, input.pci_wrong_service, input.pci_missing_service, input.pci_problem);
  } else {
    const char* bind_reason = input.pci_binding_reason.empty() ? "driver_not_bound" : input.pci_binding_reason.c_str();
    const char* actual_service =
        input_bind_service.empty() ? "<missing>" : input_bind_service.c_str();
    log.Logf(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=%s|expected=%s|actual=%s|pnp_id=%s|devices=%d|wrong_service=%d|missing_service=%d|problem=%d",
        bind_reason, input_expected_service.c_str(), actual_service,
        input_bind_pnp_id.empty() ? "-" : input_bind_pnp_id.c_str(), input.pci_devices, input.pci_wrong_service,
        input.pci_missing_service, input.pci_problem);
  }

  // Detailed virtio-input PCI binding marker (service name + PnP ID) so the host harness can fail fast with a clear
  // reason when a non-Aero virtio-input driver is installed.
  {
    std::string reason = input.pci_binding_reason.empty() ? "-" : input.pci_binding_reason;
    std::string marker;
    if (input.pci_binding_ok) {
      marker = std::string("AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=") +
               WideToUtf8(input.pci_sample_service.empty() ? kVirtioInputExpectedService : input.pci_sample_service) +
               "|pnp_id=" + WideToUtf8(input.pci_sample_pnp_id);
      if (!input.pci_sample_hwid0.empty()) {
        marker += "|hwid0=";
        marker += WideToUtf8(input.pci_sample_hwid0);
      }
    } else {
      if (reason == "-") reason = "driver_not_bound";
      marker = std::string("AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=") + reason;
      if (!input.pci_sample_pnp_id.empty()) {
        marker += "|pnp_id=";
        marker += WideToUtf8(input.pci_sample_pnp_id);
      }
      if (!input.pci_sample_hwid0.empty()) {
        marker += "|hwid0=";
        marker += WideToUtf8(input.pci_sample_hwid0);
      }

      if (reason == "wrong_service") {
        marker += "|expected=";
        marker += WideToUtf8(kVirtioInputExpectedService);
        marker += "|actual=";
        marker += WideToUtf8(input.pci_sample_service.empty() ? L"-" : input.pci_sample_service);
      } else if (reason == "driver_not_bound" || reason == "device_missing") {
        marker += "|expected=";
        marker += WideToUtf8(kVirtioInputExpectedService);
      } else if (reason == "device_error") {
        if (!input.pci_sample_service.empty()) {
          marker += "|service=";
          marker += WideToUtf8(input.pci_sample_service);
        }
        marker += "|cm_problem=";
        marker += std::to_string(static_cast<unsigned long>(input.pci_sample_cm_problem));
        char cm_status_hex[16];
        snprintf(cm_status_hex, sizeof(cm_status_hex), "0x%08lx",
                 static_cast<unsigned long>(input.pci_sample_cm_status));
        marker += "|cm_status=";
        marker += cm_status_hex;
      } else {
        marker += "|expected=";
        marker += WideToUtf8(kVirtioInputExpectedService);
      }
    }
    log.LogLine(marker);
  }

  const bool input_ok = input.ok;
  const char* input_reason = input.reason.empty() ? "-" : input.reason.c_str();
  log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input|%s|devices=%d|keyboard_devices=%d|"
           "consumer_devices=%d|mouse_devices=%d|ambiguous_devices=%d|unknown_devices=%d|"
           "keyboard_collections=%d|consumer_collections=%d|mouse_collections=%d|tablet_devices=%d|"
           "tablet_collections=%d|reason=%s%s",
           input_ok ? "PASS" : "FAIL", input.matched_devices, input.keyboard_devices, input.consumer_devices,
           input.mouse_devices, input.ambiguous_devices, input.unknown_devices, input.keyboard_collections,
           input.consumer_collections, input.mouse_collections, input.tablet_devices, input.tablet_collections,
           input_reason, input_irq_fields.c_str());
  // Optional: tablet enumeration marker. Do not fail the overall selftest if absent; tablet devices
  // are not always attached by the host harness.
  if (input.tablet_devices > 0 && input.tablet_collections > 0) {
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet|PASS|tablet_devices=%d|tablet_collections=%d",
             input.tablet_devices, input.tablet_collections);
  } else {
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet|SKIP|not_present|tablet_devices=%d|tablet_collections=%d",
             input.tablet_devices, input.tablet_collections);
  }
  if (input.devinst != 0) {
    EmitVirtioIrqMarkerForDevInst(log, "virtio-input", input.devinst);
  } else {
    EmitVirtioIrqMarker(log, "virtio-input", {L"PCI\\VEN_1AF4&DEV_1052", L"PCI\\VEN_1AF4&DEV_1011"},
                        {L"VID_1AF4&PID_0001", L"VID_1AF4&PID_0002", L"VID_1AF4&PID_0003",
                         L"VID_1AF4&PID_1052", L"VID_1AF4&PID_1011"});
  }
  all_ok = all_ok && input_ok;

  // virtio-input interrupt mode diagnostics (INTx vs MSI-X).
  {
    // Try to query both the keyboard and mouse interfaces (when present) so we can detect mixed configurations.
    std::vector<VIOINPUT_INTERRUPT_INFO> infos;
    DWORD last_err = ERROR_SUCCESS;

    if (!input.keyboard_device_path.empty()) {
      DWORD err = ERROR_SUCCESS;
      if (const auto info = QueryVirtioInputInterruptInfo(log, input.keyboard_device_path, &err)) {
        infos.push_back(*info);
      } else if (err != ERROR_SUCCESS) {
        last_err = err;
      }
    }
    if (!input.mouse_device_path.empty() && input.mouse_device_path != input.keyboard_device_path) {
      DWORD err = ERROR_SUCCESS;
      if (const auto info = QueryVirtioInputInterruptInfo(log, input.mouse_device_path, &err)) {
        infos.push_back(*info);
      } else if (err != ERROR_SUCCESS) {
        last_err = err;
      }
    }

    if (infos.empty()) {
      const bool ioctl_not_supported = last_err == ERROR_INVALID_FUNCTION || last_err == ERROR_NOT_SUPPORTED ||
                                       last_err == ERROR_INVALID_PARAMETER;
      if (ioctl_not_supported && !opt.require_input_msix) {
        log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|SKIP|reason=ioctl_not_supported|err=%lu",
                 static_cast<unsigned long>(last_err));
      } else {
        log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|FAIL|reason=query_failed|err=%lu",
                 static_cast<unsigned long>(last_err));
        if (opt.require_input_msix) all_ok = false;
      }
    } else {
      int msix_devices = 0;
      int intx_devices = 0;
      int unknown_devices = 0;
      for (const auto& info : infos) {
        if (info.Mode == VioInputInterruptModeMsix) {
          msix_devices++;
        } else if (info.Mode == VioInputInterruptModeIntx) {
          intx_devices++;
        } else {
          unknown_devices++;
        }
      }

      const bool all_msix = msix_devices == static_cast<int>(infos.size());
      const bool any_intx = intx_devices > 0;
      const bool any_unknown = unknown_devices > 0;

      // Choose a representative device whose fields match the overall mode we report.
      const VIOINPUT_INTERRUPT_INFO* chosen = &infos.front();
      if (any_intx) {
        for (const auto& info : infos) {
          if (info.Mode == VioInputInterruptModeIntx) {
            chosen = &info;
            break;
          }
        }
      } else if (!all_msix && any_unknown) {
        for (const auto& info : infos) {
          if (info.Mode != VioInputInterruptModeMsix && info.Mode != VioInputInterruptModeIntx) {
            chosen = &info;
            break;
          }
        }
      } else {
        // Prefer keyboard when available (it tends to be enumerated earlier and is stable across images).
        if (!input.keyboard_device_path.empty() && infos.size() > 1) {
          chosen = &infos.front();
        }
      }

      const char* overall_mode = all_msix ? "msix" : (any_intx ? "intx" : "unknown");
      const bool require_ok = !opt.require_input_msix || std::string(overall_mode) == "msix";
      const char* status = require_ok ? "PASS" : "FAIL";

      auto vec_to_string = [](USHORT v) -> std::string {
        if (v == VIOINPUT_INTERRUPT_VECTOR_NONE) return "none";
        return std::to_string(static_cast<unsigned int>(v));
      };

      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|%s|mode=%s|messages=%lu|mapping=%s|used_vectors=%u|"
          "config_vector=%s|queue0_vector=%s|queue1_vector=%s|msix_devices=%d|intx_devices=%d|unknown_devices=%d|"
          "intx_spurious=%ld|total_interrupts=%ld|total_dpcs=%ld|config_irqs=%ld|queue0_irqs=%ld|queue1_irqs=%ld",
          status, overall_mode, static_cast<unsigned long>(chosen->MessageCount),
          VirtioInputInterruptMappingToString(chosen->Mapping), static_cast<unsigned int>(chosen->UsedVectorCount),
          vec_to_string(chosen->ConfigVector).c_str(), vec_to_string(chosen->Queue0Vector).c_str(),
          vec_to_string(chosen->Queue1Vector).c_str(), msix_devices, intx_devices, unknown_devices,
          static_cast<long>(chosen->IntxSpuriousCount), static_cast<long>(chosen->TotalInterruptCount),
          static_cast<long>(chosen->TotalDpcCount), static_cast<long>(chosen->ConfigInterruptCount),
          static_cast<long>(chosen->Queue0InterruptCount), static_cast<long>(chosen->Queue1InterruptCount));

      if (opt.require_input_msix && std::string(overall_mode) != "msix") {
        all_ok = false;
      }
    }
  }

  // virtio-input statusq / keyboard LED smoke test.
  if (!opt.test_input_led) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|flag_not_set");
  } else {
    const auto led = VirtioInputLedTest(log, input);
    if (led.ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS|sent=%d|format=%s|led=%s|report_id=%u|report_bytes=%u|statusq_submits=%ld|statusq_completions=%ld|statusq_full=%ld|statusq_drops=%ld|led_writes_requested=%ld|led_writes_submitted=%ld|led_writes_dropped=%ld",
          led.sent, led.format.empty() ? "-" : led.format.c_str(), led.led_name.empty() ? "-" : led.led_name.c_str(),
          static_cast<unsigned>(led.report_id), static_cast<unsigned>(led.report_bytes),
          static_cast<long>(led.statusq_submits_delta), static_cast<long>(led.statusq_completions_delta),
          static_cast<long>(led.statusq_full_delta), static_cast<long>(led.statusq_drops_delta),
          static_cast<long>(led.led_writes_requested_delta), static_cast<long>(led.led_writes_submitted_delta),
          static_cast<long>(led.led_writes_dropped_delta));
    } else {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|reason=%s|err=%lu|sent=%d|format=%s|led=%s",
               led.reason.empty() ? "unknown" : led.reason.c_str(),
               static_cast<unsigned long>(led.win32_error), led.sent, led.format.empty() ? "-" : led.format.c_str(),
               led.led_name.empty() ? "-" : led.led_name.c_str());
      all_ok = false;
    }
  }

  const bool want_input_modifiers = opt.test_input_events_modifiers;
  const bool want_input_buttons = opt.test_input_events_buttons;
  const bool want_input_wheel = opt.test_input_events_wheel;

  if (!opt.test_input_events) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|flag_not_set");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|flag_not_set");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP|flag_not_set");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP|flag_not_set");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP|flag_not_set");
  } else {
    const auto input_events = VirtioInputEventsTest(log, input, want_input_modifiers, want_input_buttons,
                                                    want_input_wheel);

    if (input_events.ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS|kbd_reports=%d|mouse_reports=%d|kbd_bad_reports=%d|mouse_bad_reports=%d|kbd_a_down=%d|kbd_a_up=%d|mouse_move=%d|mouse_left_down=%d|mouse_left_up=%d",
          input_events.keyboard_reports, input_events.mouse_reports, input_events.keyboard_bad_reports,
          input_events.mouse_bad_reports, input_events.saw_key_a_down ? 1 : 0, input_events.saw_key_a_up ? 1 : 0,
          input_events.saw_mouse_move ? 1 : 0, input_events.saw_mouse_left_down ? 1 : 0,
          input_events.saw_mouse_left_up ? 1 : 0);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|reason=%s|err=%lu|kbd_reports=%d|mouse_reports=%d|kbd_bad_reports=%d|mouse_bad_reports=%d|kbd_a_down=%d|kbd_a_up=%d|mouse_move=%d|mouse_left_down=%d|mouse_left_up=%d",
          input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
          static_cast<unsigned long>(input_events.win32_error), input_events.keyboard_reports,
          input_events.mouse_reports, input_events.keyboard_bad_reports, input_events.mouse_bad_reports,
          input_events.saw_key_a_down ? 1 : 0, input_events.saw_key_a_up ? 1 : 0, input_events.saw_mouse_move ? 1 : 0,
          input_events.saw_mouse_left_down ? 1 : 0, input_events.saw_mouse_left_up ? 1 : 0);
    }
    all_ok = all_ok && input_events.ok;

    // Optional wheel/hwheel coverage (requires host-side injection via QMP).
    // Keep this separate from the base virtio-input-events result so existing images/harnesses
    // that do not inject scroll events can still pass.
    if (!input_events.ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|input_events_failed|reason=%s|err=%lu|wheel_total=%d|hwheel_total=%d",
          input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
          static_cast<unsigned long>(input_events.win32_error), input_events.mouse_wheel_total,
          input_events.mouse_hwheel_total);
    } else if (!input_events.saw_mouse_wheel && !input_events.saw_mouse_hwheel) {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|not_observed|wheel_total=%d|hwheel_total=%d",
               input_events.mouse_wheel_total, input_events.mouse_hwheel_total);
    } else if (!input_events.saw_mouse_wheel || !input_events.saw_mouse_hwheel) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=missing_axis|wheel_total=%d|hwheel_total=%d|saw_wheel=%d|saw_hwheel=%d",
          input_events.mouse_wheel_total, input_events.mouse_hwheel_total, input_events.saw_mouse_wheel ? 1 : 0,
          input_events.saw_mouse_hwheel ? 1 : 0);
    } else if (input_events.saw_mouse_wheel_unexpected || input_events.saw_mouse_hwheel_unexpected) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=unexpected_delta|wheel_total=%d|hwheel_total=%d|expected_wheel=%d|expected_hwheel=%d|wheel_events=%d|hwheel_events=%d|wheel_unexpected_last=%d|hwheel_unexpected_last=%d",
          input_events.mouse_wheel_total, input_events.mouse_hwheel_total, kExpectedMouseWheelDelta,
          kExpectedMouseHWheelDelta, input_events.mouse_wheel_events, input_events.mouse_hwheel_events,
          input_events.mouse_wheel_unexpected_last, input_events.mouse_hwheel_unexpected_last);
    } else if (input_events.saw_mouse_wheel_expected && input_events.saw_mouse_hwheel_expected) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|wheel_total=%d|hwheel_total=%d|expected_wheel=%d|expected_hwheel=%d|wheel_events=%d|hwheel_events=%d",
          input_events.mouse_wheel_total, input_events.mouse_hwheel_total, kExpectedMouseWheelDelta,
          kExpectedMouseHWheelDelta, input_events.mouse_wheel_events, input_events.mouse_hwheel_events);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=delta_mismatch|wheel_total=%d|hwheel_total=%d|expected_wheel=%d|expected_hwheel=%d|wheel_events=%d|hwheel_events=%d|saw_wheel_expected=%d|saw_hwheel_expected=%d",
          input_events.mouse_wheel_total, input_events.mouse_hwheel_total, kExpectedMouseWheelDelta,
          kExpectedMouseHWheelDelta, input_events.mouse_wheel_events, input_events.mouse_hwheel_events,
          input_events.saw_mouse_wheel_expected ? 1 : 0, input_events.saw_mouse_hwheel_expected ? 1 : 0);
    }

    if (!want_input_modifiers) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP|flag_not_set");
    } else if (input_events.modifiers_ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS|kbd_reports=%d|kbd_bad_reports=%d|shift_b=%d|ctrl=%d|alt=%d|f1=%d",
          input_events.keyboard_reports, input_events.keyboard_bad_reports, input_events.saw_shift_b ? 1 : 0,
          (input_events.saw_ctrl_down && input_events.saw_ctrl_up) ? 1 : 0,
          (input_events.saw_alt_down && input_events.saw_alt_up) ? 1 : 0,
          (input_events.saw_f1_down && input_events.saw_f1_up) ? 1 : 0);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|reason=%s|err=%lu|kbd_reports=%d|kbd_bad_reports=%d|shift_b=%d|ctrl_down=%d|ctrl_up=%d|alt_down=%d|alt_up=%d|f1_down=%d|f1_up=%d",
          input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
          static_cast<unsigned long>(input_events.win32_error), input_events.keyboard_reports,
          input_events.keyboard_bad_reports, input_events.saw_shift_b ? 1 : 0, input_events.saw_ctrl_down ? 1 : 0,
          input_events.saw_ctrl_up ? 1 : 0, input_events.saw_alt_down ? 1 : 0, input_events.saw_alt_up ? 1 : 0,
          input_events.saw_f1_down ? 1 : 0, input_events.saw_f1_up ? 1 : 0);
    }
    if (want_input_modifiers) all_ok = all_ok && input_events.modifiers_ok;

    if (!want_input_buttons) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP|flag_not_set");
    } else if (input_events.buttons_ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS|mouse_reports=%d|mouse_bad_reports=%d|side_down=%d|side_up=%d|extra_down=%d|extra_up=%d",
          input_events.mouse_reports, input_events.mouse_bad_reports, input_events.saw_mouse_side_down ? 1 : 0,
          input_events.saw_mouse_side_up ? 1 : 0, input_events.saw_mouse_extra_down ? 1 : 0,
          input_events.saw_mouse_extra_up ? 1 : 0);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|reason=%s|err=%lu|mouse_reports=%d|mouse_bad_reports=%d|side_down=%d|side_up=%d|extra_down=%d|extra_up=%d",
          input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
          static_cast<unsigned long>(input_events.win32_error), input_events.mouse_reports, input_events.mouse_bad_reports,
          input_events.saw_mouse_side_down ? 1 : 0, input_events.saw_mouse_side_up ? 1 : 0,
          input_events.saw_mouse_extra_down ? 1 : 0, input_events.saw_mouse_extra_up ? 1 : 0);
    }
    if (want_input_buttons) all_ok = all_ok && input_events.buttons_ok;

    if (!want_input_wheel) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP|flag_not_set");
    } else if (input_events.wheel_ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS|mouse_reports=%d|mouse_bad_reports=%d|wheel_total=%d|hwheel_total=%d|expected_wheel=%d|expected_hwheel=%d|saw_wheel=%d|saw_hwheel=%d",
          input_events.mouse_reports, input_events.mouse_bad_reports, input_events.mouse_wheel_total,
          input_events.mouse_hwheel_total, kExpectedMouseWheelDelta, kExpectedMouseHWheelDelta,
          input_events.saw_mouse_wheel ? 1 : 0, input_events.saw_mouse_hwheel ? 1 : 0);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|reason=%s|err=%lu|mouse_reports=%d|mouse_bad_reports=%d|wheel_total=%d|hwheel_total=%d|expected_wheel=%d|expected_hwheel=%d|saw_wheel=%d|saw_hwheel=%d",
          input_events.reason.empty() ? "unknown" : input_events.reason.c_str(),
          static_cast<unsigned long>(input_events.win32_error), input_events.mouse_reports,
          input_events.mouse_bad_reports, input_events.mouse_wheel_total, input_events.mouse_hwheel_total,
          kExpectedMouseWheelDelta, kExpectedMouseHWheelDelta, input_events.saw_mouse_wheel ? 1 : 0,
          input_events.saw_mouse_hwheel ? 1 : 0);
    }
    if (want_input_wheel) all_ok = all_ok && input_events.wheel_ok;
  }

  if (!opt.test_input_media_keys) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|SKIP|flag_not_set");
  } else {
    const auto media = VirtioInputMediaKeysTest(log, input);
    if (media.ok) {
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS|reports=%d|volume_up_down=%d|volume_up_up=%d",
               media.reports, media.saw_volume_up_down ? 1 : 0, media.saw_volume_up_up ? 1 : 0);
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|reason=%s|err=%lu|reports=%d|volume_up_down=%d|volume_up_up=%d",
          media.reason.empty() ? "unknown" : media.reason.c_str(), static_cast<unsigned long>(media.win32_error),
          media.reports, media.saw_volume_up_down ? 1 : 0, media.saw_volume_up_up ? 1 : 0);
    }
    all_ok = all_ok && media.ok;
  }

  if (!opt.test_input_tablet_events) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set");
  } else {
    const auto tablet_events = VirtioInputTabletEventsTest(log, input);
    if (tablet_events.ok) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|tablet_reports=%d|move_target=%d|left_down=%d|left_up=%d|last_x=%d|last_y=%d|last_left=%d",
          tablet_events.tablet_reports, tablet_events.saw_move_target ? 1 : 0,
          tablet_events.saw_left_down ? 1 : 0, tablet_events.saw_left_up ? 1 : 0, tablet_events.last_x,
          tablet_events.last_y, tablet_events.last_left);
      all_ok = all_ok && tablet_events.ok;
    } else if (tablet_events.reason == "missing_tablet_device") {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|no_tablet_device");
      // Do not affect overall result: this test is opt-in and may be enabled in images without a tablet.
    } else {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|reason=%s|err=%lu|tablet_reports=%d|move_target=%d|left_down=%d|left_up=%d|last_x=%d|last_y=%d|last_left=%d",
          tablet_events.reason.empty() ? "unknown" : tablet_events.reason.c_str(),
          static_cast<unsigned long>(tablet_events.win32_error), tablet_events.tablet_reports,
          tablet_events.saw_move_target ? 1 : 0, tablet_events.saw_left_down ? 1 : 0,
          tablet_events.saw_left_up ? 1 : 0, tablet_events.last_x, tablet_events.last_y, tablet_events.last_left);
      all_ok = false;
    }
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

  const std::string snd_irq_fields =
      opt.allow_virtio_snd_transitional
          ? IrqFieldsForTestMarker({L"PCI\\VEN_1AF4&DEV_1059", L"PCI\\VEN_1AF4&DEV_1018"})
          : IrqFieldsForTestMarker({L"PCI\\VEN_1AF4&DEV_1059"});

  const bool want_snd_playback = opt.require_snd || !snd_pci.empty();
  const bool capture_smoke_test = opt.test_snd_capture || opt.require_non_silence || want_snd_playback;
  const bool want_snd_capture =
      !opt.disable_snd_capture &&
      (opt.require_snd_capture || opt.test_snd_capture || opt.require_non_silence || want_snd_playback);

  if (opt.disable_snd) {
    log.LogLine("virtio-snd: disabled by --disable-snd");
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP%s", snd_irq_fields.c_str());
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled");
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled");
  } else if (!want_snd_playback && !opt.require_snd_capture && !opt.test_snd_capture &&
             !opt.require_non_silence) {
    log.LogLine("virtio-snd: skipped (enable with --test-snd)");
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP%s", snd_irq_fields.c_str());
    log.LogLine(opt.disable_snd_capture ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled"
                                        : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set");
    log.LogLine(opt.disable_snd_capture ? "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled"
                                        : "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set");
  } else {
    if (!want_snd_playback) {
      log.LogLine("virtio-snd: skipped (enable with --test-snd)");
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP%s", snd_irq_fields.c_str());
    }

    if (snd_pci.empty()) {
      if (opt.allow_virtio_snd_transitional) {
        log.LogLine(
            "virtio-snd: PCI\\VEN_1AF4&DEV_1059 (or legacy PCI\\VEN_1AF4&DEV_1018) device not detected");
      } else {
        log.LogLine("virtio-snd: PCI\\VEN_1AF4&DEV_1059 device not detected (contract v1 modern-only)");
      }

      if (want_snd_playback) {
        log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL%s", snd_irq_fields.c_str());
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
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|%s%s", reason, snd_irq_fields.c_str());
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
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL%s", snd_irq_fields.c_str());
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
        std::string force_null_backend_pnp_id;
        std::string force_null_backend_source = "unknown";
        for (const auto& dev : snd_pci) {
          if (dev.force_null_backend.has_value() && *dev.force_null_backend != 0) {
            force_null_backend = true;
            if (!dev.instance_id.empty()) {
              force_null_backend_pnp_id = WideToUtf8(dev.instance_id);
            }
            if (dev.force_null_backend_source.has_value()) {
              force_null_backend_source =
                  VirtioSndToggleRegSourceToString(*dev.force_null_backend_source);
            }
            break;
          }
        }

        if (force_null_backend) {
          if (!force_null_backend_pnp_id.empty()) {
            log.Logf(
                "virtio-snd: ForceNullBackend=1 set (pnp_id=%s source=%s); virtio transport disabled (host wav capture will be silent)",
                force_null_backend_pnp_id.c_str(), force_null_backend_source.c_str());
          } else {
            log.Logf(
                "virtio-snd: ForceNullBackend=1 set (source=%s); virtio transport disabled (host wav capture will be silent)",
                force_null_backend_source.c_str());
          }

          if (want_snd_playback) {
            log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend%s", snd_irq_fields.c_str());
            all_ok = false;
          } else {
            log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP%s", snd_irq_fields.c_str());
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

          std::string snd_render_mix_format = "<unknown>";
          std::string snd_capture_mix_format = opt.disable_snd_capture ? "<disabled>" : "<unknown>";

          if (want_snd_playback) {
            bool snd_ok = false;
            const auto snd = VirtioSndTest(log, match_names, opt.allow_virtio_snd_transitional);
            if (!snd.mix_format.empty()) {
              snd_render_mix_format = snd.mix_format;
            } else if (snd.fail_reason == "no_matching_endpoint") {
              snd_render_mix_format = "<missing>";
            }
            if (snd.ok) {
              snd_ok = true;
            } else {
              log.Logf("virtio-snd: WASAPI failed reason=%s hr=0x%08lx",
                       snd.fail_reason.empty() ? "unknown" : snd.fail_reason.c_str(),
                       static_cast<unsigned long>(snd.hr));
              log.LogLine("virtio-snd: trying waveOut fallback");
              snd_ok = WaveOutToneTest(log, match_names, opt.allow_virtio_snd_transitional);
            }

            log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|%s%s", snd_ok ? "PASS" : "FAIL",
                     snd_irq_fields.c_str());
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
            if (!capture.mix_format.empty()) {
              snd_capture_mix_format = capture.mix_format;
            } else if (capture.fail_reason == "no_matching_endpoint") {
              snd_capture_mix_format = "<missing>";
            }
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
            snd_capture_mix_format = "<skipped>";
          }

          // Surface the negotiated virtio-snd format/rate selected by the driver, as visible via the
          // Windows shared-mode mix format. This helps the host harness diagnose non-contract devices.
          log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=%s|capture=%s",
                   snd_render_mix_format.c_str(), snd_capture_mix_format.c_str());

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

  // Best-effort virtio-snd eventq diagnostics:
  // Query the topology miniport for eventq counters and emit a stable marker so
  // host harnesses can observe whether a device model emitted events.
  if (opt.disable_snd) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|disabled");
  } else if (snd_pci.empty()) {
    log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|device_missing");
  } else {
    std::optional<std::wstring> topo_path;
    for (const auto& dev : snd_pci) {
      if (dev.instance_id.empty()) continue;
      topo_path =
          GetDeviceInterfacePathForInstance(log, kKsCategoryTopology, dev.instance_id, "KSCATEGORY_TOPOLOGY");
      if (topo_path) break;
    }

    if (!topo_path) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|topology_interface_missing");
    } else if (auto stats = QueryVirtioSndEventqStats(log, *topo_path)) {
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=%ld|parsed=%ld|short=%ld|unknown=%ld|jack_connected=%ld|jack_disconnected=%ld|pcm_period=%ld|xrun=%ld|ctl_notify=%ld",
          stats->Completions, stats->Parsed, stats->ShortBuffers, stats->UnknownType, stats->JackConnected,
          stats->JackDisconnected, stats->PcmPeriodElapsed, stats->PcmXrun, stats->CtlNotify);
    } else {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|query_failed");
    }
  }

  if (!snd_pci.empty() && snd_pci.front().devinst != 0) {
    // Prefer driver-provided diag info (vector mapping + counters); fall back to resource inspection.
    EmitVirtioSndMsixMarker(log, snd_pci.front().devinst);
    EmitVirtioSndIrqMarker(log, snd_pci.front().devinst);
  } else if (opt.allow_virtio_snd_transitional) {
    EmitVirtioSndMsixMarker(log, 0);
    EmitVirtioIrqMarker(log, "virtio-snd", {L"PCI\\VEN_1AF4&DEV_1059", L"PCI\\VEN_1AF4&DEV_1018"});
  } else {
    EmitVirtioSndMsixMarker(log, 0);
    EmitVirtioIrqMarker(log, "virtio-snd", {L"PCI\\VEN_1AF4&DEV_1059"});
  }

  const std::string net_irq_fields_fallback =
      IrqFieldsForTestMarker({L"PCI\\VEN_1AF4&DEV_1041", L"PCI\\VEN_1AF4&DEV_1000"});

  // Network tests require Winsock initialized for getaddrinfo.
  DEVINST net_devinst = 0;
  WSADATA wsa{};
  const int wsa_rc = WSAStartup(MAKEWORD(2, 2), &wsa);
  if (wsa_rc != 0) {
    log.Logf("virtio-net: WSAStartup failed rc=%d", wsa_rc);
    log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL%s", net_irq_fields_fallback.c_str());
    all_ok = false;
    } else {
      const auto net = VirtioNetTest(log, opt);
      net_devinst = net.devinst;
      const std::string net_irq_fields =
          (net_devinst != 0) ? IrqFieldsForTestMarkerFromDevInst(net_devinst) : net_irq_fields_fallback;
      log.Logf("AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|%s|bytes=%lu|small_bytes=%lu|mtu_bytes=%lu|reason=%s|wsa=%d",
               net.udp.ok ? "PASS" : "FAIL", static_cast<unsigned long>(net.udp.bytes),
               static_cast<unsigned long>(net.udp.small_bytes), static_cast<unsigned long>(net.udp.mtu_bytes),
               net.udp.ok ? "-" : (net.udp.fail_reason.empty() ? "unknown" : net.udp.fail_reason.c_str()),
               net.udp.wsa_error);
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-net|%s|large_ok=%d|large_bytes=%llu|large_fnv1a64=0x%016I64x|large_mbps=%.2f|"
          "upload_ok=%d|upload_bytes=%llu|upload_mbps=%.2f|msi=%d|msi_messages=%d%s",
          net.ok ? "PASS" : "FAIL", net.large_ok ? 1 : 0,
        static_cast<unsigned long long>(net.large_bytes), static_cast<unsigned long long>(net.large_hash),
        net.large_mbps, net.upload_ok ? 1 : 0, static_cast<unsigned long long>(net.upload_bytes), net.upload_mbps,
        (net.msi_messages < 0) ? -1 : (net.msi_messages > 0 ? 1 : 0), net.msi_messages, net_irq_fields.c_str());

    const auto offload = QueryAerovnetOffloadStats(log);
    if (!offload.has_value()) {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|FAIL");
      all_ok = false;
    } else {
      const uint64_t tx_csum = offload->TxCsumOffloadTcp4 + offload->TxCsumOffloadTcp6 +
                               offload->TxCsumOffloadUdp4 + offload->TxCsumOffloadUdp6;
      const uint64_t rx_csum = offload->RxCsumValidatedTcp4 + offload->RxCsumValidatedTcp6 +
                               offload->RxCsumValidatedUdp4 + offload->RxCsumValidatedUdp6;
      const uint64_t tx_tcp = offload->TxCsumOffloadTcp4 + offload->TxCsumOffloadTcp6;
      const uint64_t tx_udp = offload->TxCsumOffloadUdp4 + offload->TxCsumOffloadUdp6;
      const uint64_t rx_tcp = offload->RxCsumValidatedTcp4 + offload->RxCsumValidatedTcp6;
      const uint64_t rx_udp = offload->RxCsumValidatedUdp4 + offload->RxCsumValidatedUdp6;
      const uint64_t fallback = offload->TxCsumFallback;

      log.Logf("virtio-net: csum_offload tx=%llu rx=%llu fallback=%llu features=%s",
               static_cast<unsigned long long>(tx_csum), static_cast<unsigned long long>(rx_csum),
               static_cast<unsigned long long>(fallback),
               VirtioFeaturesToString(static_cast<ULONGLONG>(offload->GuestFeatures)).c_str());
      log.Logf(
          "AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=%llu|rx_csum=%llu|fallback=%llu|"
          "tx_tcp=%llu|tx_udp=%llu|rx_tcp=%llu|rx_udp=%llu|"
          "tx_tcp4=%llu|tx_tcp6=%llu|tx_udp4=%llu|tx_udp6=%llu|"
          "rx_tcp4=%llu|rx_tcp6=%llu|rx_udp4=%llu|rx_udp6=%llu",
          static_cast<unsigned long long>(tx_csum),
          static_cast<unsigned long long>(rx_csum),
          static_cast<unsigned long long>(fallback),
          static_cast<unsigned long long>(tx_tcp),
          static_cast<unsigned long long>(tx_udp),
          static_cast<unsigned long long>(rx_tcp),
          static_cast<unsigned long long>(rx_udp),
          static_cast<unsigned long long>(offload->TxCsumOffloadTcp4),
          static_cast<unsigned long long>(offload->TxCsumOffloadTcp6),
          static_cast<unsigned long long>(offload->TxCsumOffloadUdp4),
          static_cast<unsigned long long>(offload->TxCsumOffloadUdp6),
          static_cast<unsigned long long>(offload->RxCsumValidatedTcp4),
          static_cast<unsigned long long>(offload->RxCsumValidatedTcp6),
          static_cast<unsigned long long>(offload->RxCsumValidatedUdp4),
          static_cast<unsigned long long>(offload->RxCsumValidatedUdp6));
    }
    all_ok = all_ok && net.ok;
    if (opt.test_net_link_flap) {
      all_ok = all_ok && net.link_flap_ok;
    } else {
      log.LogLine("AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set");
    }
    WSACleanup();
  }

  if (!EmitVirtioNetMsixMarker(log, opt.require_net_msix)) {
    all_ok = false;
  }
  EmitVirtioNetDiagMarker(log);
  if (net_devinst != 0) {
    EmitVirtioIrqMarkerForDevInst(log, "virtio-net", net_devinst);
  } else {
    EmitVirtioIrqMarker(log, "virtio-net", {L"PCI\\VEN_1AF4&DEV_1041"});
  }

  log.Logf("AERO_VIRTIO_SELFTEST|RESULT|%s", all_ok ? "PASS" : "FAIL");
  return all_ok ? 0 : 1;
}
