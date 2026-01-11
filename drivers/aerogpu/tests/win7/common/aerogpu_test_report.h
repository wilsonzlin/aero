#pragma once

#include "aerogpu_test_common.h"

// Minimal JSON reporting utilities for AeroGPU validation tests.
//
// The goal is *deterministic*, machine-readable output without introducing
// additional dependencies (no STL iostreams, no external JSON lib).

namespace aerogpu_test {

static const int kAeroGpuTestReportSchemaVersion = 1;

#ifndef va_copy
#define va_copy(dest, src) ((dest) = (src))
#endif

static inline std::wstring Utf8ToWideFallbackAcp(const std::string& s) {
  if (s.empty()) {
    return std::wstring();
  }
  int len = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.c_str(), (int)s.size(), NULL, 0);
  UINT cp = CP_UTF8;
  DWORD flags = MB_ERR_INVALID_CHARS;
  if (len <= 0) {
    // Fall back to the current ANSI code page (argv on Win7 is typically ANSI).
    cp = CP_ACP;
    flags = 0;
    len = MultiByteToWideChar(cp, flags, s.c_str(), (int)s.size(), NULL, 0);
  }
  if (len <= 0) {
    return std::wstring();
  }
  std::wstring out;
  out.resize((size_t)len);
  MultiByteToWideChar(cp, flags, s.c_str(), (int)s.size(), &out[0], len);
  return out;
}

static inline std::string WideToUtf8(const std::wstring& w) {
  if (w.empty()) {
    return std::string();
  }
  int len = WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), NULL, 0, NULL, NULL);
  if (len <= 0) {
    // Fallback: best-effort ANSI.
    len = WideCharToMultiByte(CP_ACP, 0, w.c_str(), (int)w.size(), NULL, 0, NULL, NULL);
    if (len <= 0) {
      return std::string();
    }
    std::string out;
    out.resize((size_t)len);
    WideCharToMultiByte(CP_ACP, 0, w.c_str(), (int)w.size(), &out[0], len, NULL, NULL);
    return out;
  }
  std::string out;
  out.resize((size_t)len);
  WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), &out[0], len, NULL, NULL);
  return out;
}

static inline std::string FormatStringV(const char* fmt, va_list ap) {
  if (!fmt) {
    return std::string();
  }

  // Try a small stack buffer first.
  char stack_buf[512];
  va_list ap_copy;
  va_copy(ap_copy, ap);
  int n = _vsnprintf(stack_buf, sizeof(stack_buf), fmt, ap_copy);
  va_end(ap_copy);
  if (n >= 0 && n < (int)sizeof(stack_buf)) {
    return std::string(stack_buf, stack_buf + n);
  }

  // Fallback to heap buffer; grow until it fits (capped).
  size_t cap = 1024;
  for (int i = 0; i < 8; ++i) {
    std::vector<char> buf(cap);
    va_copy(ap_copy, ap);
    n = _vsnprintf(&buf[0], buf.size(), fmt, ap_copy);
    va_end(ap_copy);
    if (n >= 0 && (size_t)n < buf.size()) {
      return std::string(&buf[0], &buf[0] + n);
    }
    cap *= 2;
    if (cap > 128 * 1024) {
      break;
    }
  }
  return std::string("<formatting failed>");
}

static inline std::string FormatString(const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  std::string out = FormatStringV(fmt, ap);
  va_end(ap);
  return out;
}

static inline std::string FormatHexU16(uint32_t v) {
  char buf[16];
  _snprintf(buf, sizeof(buf), "0x%04X", (unsigned)(v & 0xFFFFu));
  return std::string(buf);
}

static inline void JsonAppendEscaped(std::string* out, const std::string& s) {
  if (!out) {
    return;
  }
  out->push_back('"');
  for (size_t i = 0; i < s.size(); ++i) {
    const unsigned char c = (unsigned char)s[i];
    switch (c) {
      case '\\':
        out->append("\\\\");
        break;
      case '"':
        out->append("\\\"");
        break;
      case '\b':
        out->append("\\b");
        break;
      case '\f':
        out->append("\\f");
        break;
      case '\n':
        out->append("\\n");
        break;
      case '\r':
        out->append("\\r");
        break;
      case '\t':
        out->append("\\t");
        break;
      default:
        if (c < 0x20) {
          char buf[8];
          _snprintf(buf, sizeof(buf), "\\u%04X", (unsigned)c);
          out->append(buf);
        } else {
          out->push_back((char)c);
        }
        break;
    }
  }
  out->push_back('"');
}

static inline std::string JsonFormatDouble(double v) {
  char buf[64];
  _snprintf(buf, sizeof(buf), "%.6f", v);
  // JSON requires '.' as the decimal separator; be robust to locale settings.
  for (char* p = buf; *p; ++p) {
    if (*p == ',') {
      *p = '.';
    }
  }
  return std::string(buf);
}

static inline bool WriteFileStringW(const std::wstring& path, const std::string& contents, std::string* err) {
  HANDLE h = CreateFileW(path.c_str(),
                         GENERIC_WRITE,
                         0,
                         NULL,
                         CREATE_ALWAYS,
                         FILE_ATTRIBUTE_NORMAL,
                         NULL);
  if (h == INVALID_HANDLE_VALUE) {
    if (err) {
      *err = "CreateFileW failed: " + Win32ErrorToString(GetLastError());
    }
    return false;
  }

  DWORD written = 0;
  BOOL ok = WriteFile(h, contents.data(), (DWORD)contents.size(), &written, NULL);
  DWORD last_err = ok ? 0 : GetLastError();
  CloseHandle(h);
  if (!ok || written != (DWORD)contents.size()) {
    if (err) {
      *err = "WriteFile failed: " + Win32ErrorToString(ok ? ERROR_WRITE_FAULT : last_err);
    }
    return false;
  }
  return true;
}

struct TestReportAdapterInfo {
  bool present;
  std::string description_utf8;
  uint32_t vendor_id;
  uint32_t device_id;

  TestReportAdapterInfo() : present(false), vendor_id(0), device_id(0) {}
};

struct TestReportTimingInfo {
  bool present;
  std::vector<double> samples_ms;
  double avg_ms;
  double min_ms;
  double max_ms;

  TestReportTimingInfo() : present(false), avg_ms(0.0), min_ms(0.0), max_ms(0.0) {}
};

struct TestReport {
  int schema_version;
  std::string test_name;
  std::string status;  // "PASS" or "FAIL"
  int exit_code;
  std::string failure;  // empty => null

  bool skipped;
  std::string skip_reason;  // empty => null

  TestReportAdapterInfo adapter;
  TestReportTimingInfo timing;
  std::vector<std::string> artifacts_utf8;

  TestReport()
      : schema_version(kAeroGpuTestReportSchemaVersion),
        status("FAIL"),
        exit_code(1),
        skipped(false) {}
};

static inline std::string BuildTestReportJson(const TestReport& r) {
  std::string out;
  out.reserve(1024);
  out.push_back('{');

  out.append("\"schema_version\":");
  out.append(FormatString("%d", r.schema_version));

  out.append(",\"test_name\":");
  JsonAppendEscaped(&out, r.test_name);

  out.append(",\"status\":");
  JsonAppendEscaped(&out, r.status);

  out.append(",\"exit_code\":");
  out.append(FormatString("%d", r.exit_code));

  out.append(",\"failure\":");
  if (r.failure.empty()) {
    out.append("null");
  } else {
    JsonAppendEscaped(&out, r.failure);
  }

  out.append(",\"skipped\":");
  out.append(r.skipped ? "true" : "false");

  out.append(",\"skip_reason\":");
  if (!r.skipped || r.skip_reason.empty()) {
    out.append("null");
  } else {
    JsonAppendEscaped(&out, r.skip_reason);
  }

  out.append(",\"adapter\":");
  if (!r.adapter.present) {
    out.append("null");
  } else {
    out.push_back('{');
    out.append("\"description\":");
    JsonAppendEscaped(&out, r.adapter.description_utf8);
    out.append(",\"vid\":");
    JsonAppendEscaped(&out, FormatHexU16(r.adapter.vendor_id));
    out.append(",\"did\":");
    JsonAppendEscaped(&out, FormatHexU16(r.adapter.device_id));
    out.push_back('}');
  }

  out.append(",\"timing\":");
  if (!r.timing.present) {
    out.append("null");
  } else {
    out.push_back('{');
    out.append("\"samples_ms\":[");
    for (size_t i = 0; i < r.timing.samples_ms.size(); ++i) {
      if (i) {
        out.push_back(',');
      }
      out.append(JsonFormatDouble(r.timing.samples_ms[i]));
    }
    out.append("],\"avg_ms\":");
    out.append(JsonFormatDouble(r.timing.avg_ms));
    out.append(",\"min_ms\":");
    out.append(JsonFormatDouble(r.timing.min_ms));
    out.append(",\"max_ms\":");
    out.append(JsonFormatDouble(r.timing.max_ms));
    out.push_back('}');
  }

  out.append(",\"artifacts\":[");
  for (size_t i = 0; i < r.artifacts_utf8.size(); ++i) {
    if (i) {
      out.push_back(',');
    }
    JsonAppendEscaped(&out, r.artifacts_utf8[i]);
  }
  out.push_back(']');

  out.push_back('}');
  return out;
}

class TestReporter {
 public:
  TestReporter(const char* test_name, int argc, char** argv)
      : enabled_(false), finalized_(false) {
    report_.test_name = test_name ? test_name : "";

    // Parse `--json[=PATH]` without the ambiguity of `--json PATH` consuming
    // the next flag (e.g. `--json --dump`). We only consume the next argv as a
    // value if it doesn't look like another flag.
    const char* json_value = NULL;
    const char* kJsonPrefix = "--json=";
    for (int i = 1; i < argc; ++i) {
      const char* arg = argv[i];
      if (!arg) {
        continue;
      }
      if (StrIStartsWith(arg, kJsonPrefix)) {
        enabled_ = true;
        json_value = arg + strlen(kJsonPrefix);
        break;
      }
      if (lstrcmpiA(arg, "--json") == 0) {
        enabled_ = true;
        if (i + 1 < argc && argv[i + 1] && argv[i + 1][0] != '-') {
          json_value = argv[i + 1];
        }
        break;
      }
    }

    if (enabled_) {
      if (!json_value || !*json_value) {
        // Default: next to the module (typically win7/bin/<test>.json).
        const std::wstring dir = GetModuleDir();
        const std::wstring leaf = Utf8ToWideFallbackAcp(report_.test_name + ".json");
        json_path_ = JoinPath(dir, leaf.c_str());
      } else {
        json_path_ = Utf8ToWideFallbackAcp(std::string(json_value));
      }
    }
  }

  ~TestReporter() { WriteIfEnabled(); }

  void SetAdapterInfoA(const char* desc, uint32_t vid, uint32_t did) {
    report_.adapter.present = true;
    report_.adapter.description_utf8 = desc ? desc : "";
    report_.adapter.vendor_id = vid;
    report_.adapter.device_id = did;
  }

  void SetAdapterInfoW(const wchar_t* desc, uint32_t vid, uint32_t did) {
    report_.adapter.present = true;
    report_.adapter.description_utf8 = WideToUtf8(desc ? std::wstring(desc) : std::wstring());
    report_.adapter.vendor_id = vid;
    report_.adapter.device_id = did;
  }

  void AddArtifactPathW(const std::wstring& path) { report_.artifacts_utf8.push_back(WideToUtf8(path)); }

  void SetTimingSamplesMs(const std::vector<double>& samples_ms) {
    report_.timing.present = true;
    report_.timing.samples_ms = samples_ms;
    if (samples_ms.empty()) {
      report_.timing.avg_ms = 0.0;
      report_.timing.min_ms = 0.0;
      report_.timing.max_ms = 0.0;
      return;
    }
    double sum = 0.0;
    double min_ms = 1e100;
    double max_ms = -1e100;
    for (size_t i = 0; i < samples_ms.size(); ++i) {
      const double v = samples_ms[i];
      sum += v;
      if (v < min_ms) min_ms = v;
      if (v > max_ms) max_ms = v;
    }
    report_.timing.avg_ms = sum / (double)samples_ms.size();
    report_.timing.min_ms = min_ms;
    report_.timing.max_ms = max_ms;
  }

  void SetSkipped(const char* reason) {
    report_.skipped = true;
    report_.skip_reason = reason ? reason : "";
  }

  int Pass() {
    finalized_ = true;
    report_.status = "PASS";
    report_.exit_code = 0;
    PrintfStdout("PASS: %s", report_.test_name.c_str());
    return 0;
  }

  int Fail(const char* fmt, ...) {
    finalized_ = true;
    report_.status = "FAIL";
    report_.exit_code = 1;
    va_list ap;
    va_start(ap, fmt);
    report_.failure = FormatStringV(fmt, ap);
    va_end(ap);
    return aerogpu_test::Fail(report_.test_name.c_str(), "%s", report_.failure.c_str());
  }

  int FailHresult(const char* what, HRESULT hr) {
    return Fail("%s failed with %s", what ? what : "<null>", HresultToString(hr).c_str());
  }

 private:
  void WriteIfEnabled() {
    if (!enabled_) {
      return;
    }
    // If the test returned without explicitly calling Pass()/Fail(), keep the default FAIL
    // status, but ensure the report is still emitted.
    if (!finalized_ && report_.status.empty()) {
      report_.status = "FAIL";
    }

    std::string json = BuildTestReportJson(report_);
    json.push_back('\n');
    std::string err;
    if (!WriteFileStringW(json_path_, json, &err)) {
      // Reporting must not change the test outcome.
      PrintfStdout("INFO: %s: failed to write JSON report to %ls: %s",
                   report_.test_name.c_str(),
                   json_path_.c_str(),
                   err.c_str());
    }
  }

  bool enabled_;
  bool finalized_;
  std::wstring json_path_;
  TestReport report_;
};

// Reporter-aware variants of common failure helpers.
//
// Many tests predate `TestReporter` and use helpers in aerogpu_test_common.h that call
// aerogpu_test::Fail() directly. When those helpers are used from a `--json`-enabled test,
// the printed FAIL line is correct but the JSON report ends up with `"failure": null`
// because the reporter was never finalized. These wrappers preserve the original stdout
// diagnostics while correctly populating the JSON failure message.
static inline int RequireAeroGpuUmdLoaded(TestReporter* reporter,
                                          const char* test_name,
                                          const wchar_t* expected_module_base_name,
                                          const char* api_label,
                                          const char* reg_key_hint) {
  const char* tn = test_name ? test_name : "<unknown>";
  const char* api = api_label ? api_label : "<unknown>";
  const char* reg = reg_key_hint ? reg_key_hint : "<unknown>";
  const wchar_t* expected = expected_module_base_name ? expected_module_base_name : L"<null>";

  std::wstring path;
  std::string err;
  if (GetLoadedModulePathByBaseName(expected, &path, &err)) {
    if (!path.empty()) {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD (%s%s): %ls",
                   tn,
                   api,
                   GetProcessBitnessString(),
                   GetWow64SuffixString(),
                   path.c_str());
    } else if (!err.empty()) {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD module %ls (%s%s; path unavailable: %s)",
                   tn,
                   api,
                   expected,
                   GetProcessBitnessString(),
                   GetWow64SuffixString(),
                   err.c_str());
    } else {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD module %ls (%s%s; path unavailable)",
                   tn,
                   api,
                   expected,
                   GetProcessBitnessString(),
                   GetWow64SuffixString());
    }
    return 0;
  }

  DumpLoadedAeroGpuUmdModules(tn);
  if (reporter) {
    return reporter->Fail(
        "expected AeroGPU %s UMD DLL %ls to be loaded in-process (process=%s%s), but it was not. "
        "Likely causes: incorrect INF registry keys (%s), incorrect UMD exports/decoration (stdcall), "
        "or missing DLL in System32/SysWOW64.",
        api,
        expected,
        GetProcessBitnessString(),
        GetWow64SuffixString(),
        reg);
  }
  return Fail(
      tn,
      "expected AeroGPU %s UMD DLL %ls to be loaded in-process (process=%s%s), but it was not. "
      "Likely causes: incorrect INF registry keys (%s), incorrect UMD exports/decoration (stdcall), "
      "or missing DLL in System32/SysWOW64.",
      api,
      expected,
      GetProcessBitnessString(),
      GetWow64SuffixString(),
      reg);
}

static inline int RequireAeroGpuD3D9UmdLoaded(TestReporter* reporter, const char* test_name) {
  return RequireAeroGpuUmdLoaded(reporter,
                                 test_name,
                                 ExpectedAeroGpuD3D9UmdModuleBaseName(),
                                 "D3D9",
                                 "InstalledDisplayDrivers/InstalledDisplayDriversWow");
}

static inline int RequireAeroGpuD3D10UmdLoaded(TestReporter* reporter, const char* test_name) {
  return RequireAeroGpuUmdLoaded(reporter,
                                 test_name,
                                 ExpectedAeroGpuD3D10UmdModuleBaseName(),
                                 "D3D10/11",
                                 "UserModeDriverName/UserModeDriverNameWow");
}

}  // namespace aerogpu_test
