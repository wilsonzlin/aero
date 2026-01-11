#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <windows.h>
#include <d3d9.h>

#include <string>
#include <vector>

static void PrintUsage() {
  aerogpu_test::PrintfStdout(
      "Usage: aerogpu_test_runner.exe [--bin-dir=DIR] [--manifest=PATH] [--timeout-ms=NNNN] [--no-timeout] "
      "[--json[=PATH]] [test flags...]");
  aerogpu_test::PrintfStdout("");
  aerogpu_test::PrintfStdout("Runs the AeroGPU Win7 validation suite and aggregates results.");
  aerogpu_test::PrintfStdout("");
  aerogpu_test::PrintfStdout("Runner flags:");
  aerogpu_test::PrintfStdout("  --bin-dir=DIR         Directory containing the test executables.");
  aerogpu_test::PrintfStdout("                        Default: directory of aerogpu_test_runner.exe");
  aerogpu_test::PrintfStdout("  --manifest=PATH       Suite manifest file (tests_manifest.txt).");
  aerogpu_test::PrintfStdout("                        Default: ..\\tests_manifest.txt next to the bin directory, if present.");
  aerogpu_test::PrintfStdout("  --timeout-ms=NNNN     Per-test wall-clock timeout. Default: 30000 or AEROGPU_TEST_TIMEOUT_MS.");
  aerogpu_test::PrintfStdout("  --no-timeout          Disable timeouts.");
  aerogpu_test::PrintfStdout("  --json[=PATH]         Write a machine-readable JSON suite report.");
  aerogpu_test::PrintfStdout("                        Default path: next to aerogpu_test_runner.exe (report.json)");
  aerogpu_test::PrintfStdout("");
  aerogpu_test::PrintfStdout("All other flags are forwarded to each test (e.g. --dump, --hidden, --require-vid=...).");
}

static bool FileExistsW(const std::wstring& path) {
  DWORD attr = GetFileAttributesW(path.c_str());
  if (attr == INVALID_FILE_ATTRIBUTES) {
    return false;
  }
  return (attr & FILE_ATTRIBUTE_DIRECTORY) == 0;
}

static bool DirExistsW(const std::wstring& path) {
  DWORD attr = GetFileAttributesW(path.c_str());
  if (attr == INVALID_FILE_ATTRIBUTES) {
    return false;
  }
  return (attr & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

static std::wstring DirName(const std::wstring& path) {
  size_t pos = path.find_last_of(L"\\/");
  if (pos == std::wstring::npos) {
    return std::wstring();
  }
  return path.substr(0, pos + 1);
}

static std::string TrimAsciiWhitespace(const std::string& s) {
  size_t start = 0;
  while (start < s.size()) {
    const char c = s[start];
    if (c != ' ' && c != '\t' && c != '\r' && c != '\n') {
      break;
    }
    start++;
  }
  size_t end = s.size();
  while (end > start) {
    const char c = s[end - 1];
    if (c != ' ' && c != '\t' && c != '\r' && c != '\n') {
      break;
    }
    end--;
  }
  return s.substr(start, end - start);
}

static aerogpu_test::TestReportAdapterInfo QueryDefaultAdapterInfo() {
  aerogpu_test::TestReportAdapterInfo info;
  aerogpu_test::ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr) || !d3d) {
    return info;
  }
  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (FAILED(hr)) {
    return info;
  }
  info.present = true;
  info.description_utf8 = ident.Description;
  info.vendor_id = (uint32_t)ident.VendorId;
  info.device_id = (uint32_t)ident.DeviceId;
  return info;
}

static std::wstring QuoteArgForCreateProcess(const std::wstring& arg) {
  if (arg.empty()) {
    return L"\"\"";
  }

  bool needs_quotes = false;
  for (size_t i = 0; i < arg.size(); ++i) {
    if (arg[i] == L' ' || arg[i] == L'\t') {
      needs_quotes = true;
      break;
    }
  }
  if (!needs_quotes) {
    return arg;
  }

  std::wstring out;
  out.push_back(L'"');
  size_t num_backslashes = 0;
  for (size_t i = 0; i < arg.size(); ++i) {
    const wchar_t c = arg[i];
    if (c == L'\\') {
      num_backslashes++;
      out.push_back(L'\\');
      continue;
    }
    if (c == L'"') {
      out.append(num_backslashes, L'\\');
      num_backslashes = 0;
      out.push_back(L'\\');
      out.push_back(L'"');
      continue;
    }
    num_backslashes = 0;
    out.push_back(c);
  }
  out.append(num_backslashes, L'\\');
  out.push_back(L'"');
  return out;
}

struct RunResult {
  bool started;
  bool timed_out;
  DWORD exit_code;
  std::string err;

  RunResult() : started(false), timed_out(false), exit_code(1) {}
};

static RunResult RunProcessWithTimeoutW(const std::wstring& exe_path,
                                       const std::vector<std::wstring>& args,
                                       DWORD timeout_ms,
                                       bool enforce_timeout) {
  RunResult out;

  // Build a CreateProcess-compatible command line that round-trips correctly.
  std::wstring cmdline = QuoteArgForCreateProcess(exe_path);
  for (size_t i = 0; i < args.size(); ++i) {
    cmdline.push_back(L' ');
    cmdline.append(QuoteArgForCreateProcess(args[i]));
  }

  std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
  cmdline_buf.push_back(0);

  STARTUPINFOW si;
  ZeroMemory(&si, sizeof(si));
  si.cb = sizeof(si);

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  BOOL ok = CreateProcessW(exe_path.c_str(),
                           &cmdline_buf[0],
                           NULL,
                           NULL,
                           TRUE,
                           0,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (!ok) {
    out.err = "CreateProcess failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    return out;
  }

  out.started = true;

  DWORD wait = WAIT_OBJECT_0;
  if (enforce_timeout) {
    wait = WaitForSingleObject(pi.hProcess, timeout_ms);
  } else {
    wait = WaitForSingleObject(pi.hProcess, INFINITE);
  }

  if (wait == WAIT_TIMEOUT) {
    out.timed_out = true;
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    out.exit_code = 124;
  } else if (wait != WAIT_OBJECT_0) {
    out.err = "WaitForSingleObject failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    out.exit_code = 1;
  } else {
    DWORD exit_code = 1;
    if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
      out.err = "GetExitCodeProcess failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
      exit_code = 1;
    }
    out.exit_code = exit_code;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  return out;
}

static bool ParseRunnerArgs(int argc,
                            char** argv,
                            std::wstring* out_bin_dir,
                            std::wstring* out_manifest_path,
                            DWORD* out_timeout_ms,
                            bool* out_enforce_timeout,
                            bool* out_emit_json,
                            std::wstring* out_json_path,
                            std::vector<std::wstring>* out_forwarded_args) {
  if (!out_bin_dir || !out_manifest_path || !out_timeout_ms || !out_enforce_timeout || !out_emit_json ||
      !out_json_path || !out_forwarded_args) {
    return false;
  }

  *out_bin_dir = aerogpu_test::GetModuleDir();
  out_manifest_path->clear();

  // Default timeout: env var or 30000ms.
  DWORD timeout_ms = 30000;
  char env_buf[64];
  DWORD env_len = GetEnvironmentVariableA("AEROGPU_TEST_TIMEOUT_MS", env_buf, sizeof(env_buf));
  if (env_len > 0 && env_len < sizeof(env_buf)) {
    std::string env_s(env_buf, env_buf + env_len);
    uint32_t parsed = 0;
    std::string parse_err;
    if (aerogpu_test::ParseUint32(env_s, &parsed, &parse_err) && parsed > 0) {
      timeout_ms = (DWORD)parsed;
    }
  }
  *out_timeout_ms = timeout_ms;
  *out_enforce_timeout = true;

  *out_emit_json = false;
  out_json_path->clear();

  const char* const kTimeoutPrefix = "--timeout-ms=";
  const char* const kBinDirPrefix = "--bin-dir=";
  const char* const kManifestPrefix = "--manifest=";
  const char* const kJsonPrefix = "--json=";

  for (int i = 1; i < argc; ++i) {
    const char* arg = argv[i];
    if (!arg) {
      continue;
    }
    if (lstrcmpiA(arg, "--no-timeout") == 0) {
      *out_enforce_timeout = false;
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kTimeoutPrefix)) {
      const std::string val(arg + strlen(kTimeoutPrefix));
      uint32_t parsed = 0;
      std::string parse_err;
      if (!aerogpu_test::ParseUint32(val, &parsed, &parse_err) || parsed == 0) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: invalid --timeout-ms: %s", parse_err.c_str());
        return false;
      }
      *out_timeout_ms = (DWORD)parsed;
      continue;
    }
    if (lstrcmpiA(arg, "--timeout-ms") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --timeout-ms missing value");
        return false;
      }
      const std::string val(argv[++i]);
      uint32_t parsed = 0;
      std::string parse_err;
      if (!aerogpu_test::ParseUint32(val, &parsed, &parse_err) || parsed == 0) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: invalid --timeout-ms: %s", parse_err.c_str());
        return false;
      }
      *out_timeout_ms = (DWORD)parsed;
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kBinDirPrefix)) {
      const std::string val(arg + strlen(kBinDirPrefix));
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --bin-dir missing value");
        return false;
      }
      *out_bin_dir = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }
    if (lstrcmpiA(arg, "--bin-dir") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --bin-dir missing value");
        return false;
      }
      const std::string val(argv[++i]);
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --bin-dir missing value");
        return false;
      }
      *out_bin_dir = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kManifestPrefix)) {
      const std::string val(arg + strlen(kManifestPrefix));
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --manifest missing value");
        return false;
      }
      *out_manifest_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }
    if (lstrcmpiA(arg, "--manifest") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --manifest missing value");
        return false;
      }
      const std::string val(argv[++i]);
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --manifest missing value");
        return false;
      }
      *out_manifest_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kJsonPrefix)) {
      *out_emit_json = true;
      const std::string val(arg + strlen(kJsonPrefix));
      if (val.empty()) {
        *out_json_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"report.json");
      } else {
        *out_json_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      }
      continue;
    }
    if (lstrcmpiA(arg, "--json") == 0) {
      *out_emit_json = true;
      // Optional value: only consume the next argv if it doesn't look like another flag.
      if (i + 1 < argc && argv[i + 1] && argv[i + 1][0] != '-') {
        const std::string val(argv[++i]);
        *out_json_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      } else {
        *out_json_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"report.json");
      }
      continue;
    }

    // Forward everything else to the tests.
    out_forwarded_args->push_back(aerogpu_test::Utf8ToWideFallbackAcp(std::string(arg)));
  }

  if (out_manifest_path->empty()) {
    // Default: look for tests_manifest.txt adjacent to the bin directory (the typical in-tree
    // layout is win7/tests_manifest.txt with binaries in win7/bin/).
    const std::wstring default_manifest_parent =
        aerogpu_test::JoinPath(*out_bin_dir, L"..\\tests_manifest.txt");
    if (FileExistsW(default_manifest_parent)) {
      *out_manifest_path = default_manifest_parent;
    } else {
      // Also support running from a "bin-only" folder where tests_manifest.txt is placed next to
      // aerogpu_test_runner.exe.
      const std::wstring default_manifest_bin =
          aerogpu_test::JoinPath(*out_bin_dir, L"tests_manifest.txt");
      if (FileExistsW(default_manifest_bin)) {
        *out_manifest_path = default_manifest_bin;
      }
    }
  }

  return true;
}

static std::string ReadJsonFileOrEmpty(const std::wstring& path) {
  std::vector<unsigned char> bytes;
  std::string err;
  if (!aerogpu_test::ReadFileBytes(path, &bytes, &err)) {
    return std::string();
  }
  return TrimAsciiWhitespace(std::string(bytes.begin(), bytes.end()));
}

static std::string BuildAdapterJsonObject(const aerogpu_test::TestReportAdapterInfo& info) {
  if (!info.present) {
    return std::string("null");
  }
  std::string out;
  out.reserve(256);
  out.push_back('{');
  out.append("\"description\":");
  aerogpu_test::JsonAppendEscaped(&out, info.description_utf8);
  out.append(",\"vid\":");
  aerogpu_test::JsonAppendEscaped(&out, aerogpu_test::FormatHexU16(info.vendor_id));
  out.append(",\"did\":");
  aerogpu_test::JsonAppendEscaped(&out, aerogpu_test::FormatHexU16(info.device_id));
  out.push_back('}');
  return out;
}

static bool EqualsIgnoreCaseAscii(const std::string& a, const char* b) {
  if (!b) {
    return false;
  }
  const size_t blen = strlen(b);
  if (a.size() != blen) {
    return false;
  }
  for (size_t i = 0; i < blen; ++i) {
    char ca = (char)tolower((unsigned char)a[i]);
    char cb = (char)tolower((unsigned char)b[i]);
    if (ca != cb) {
      return false;
    }
  }
  return true;
}

static bool ReadTestsFromManifest(const std::wstring& manifest_path,
                                 std::vector<std::string>* out_tests,
                                 std::string* err) {
  if (!out_tests) {
    if (err) *err = "ReadTestsFromManifest: out_tests == NULL";
    return false;
  }
  out_tests->clear();
  if (err) {
    err->clear();
  }

  std::vector<unsigned char> bytes;
  std::string read_err;
  if (!aerogpu_test::ReadFileBytes(manifest_path, &bytes, &read_err)) {
    if (err) {
      *err = read_err;
    }
    return false;
  }

  std::string contents(bytes.begin(), bytes.end());
  size_t pos = 0;
  while (pos < contents.size()) {
    size_t end = contents.find('\n', pos);
    if (end == std::string::npos) {
      end = contents.size();
    }
    std::string line = contents.substr(pos, end - pos);
    pos = (end < contents.size()) ? (end + 1) : end;

    line = TrimAsciiWhitespace(line);
    if (line.empty()) {
      continue;
    }

    // Match `for /f "tokens=1"` behavior in run_all.cmd.
    size_t tok_end = 0;
    while (tok_end < line.size() && line[tok_end] != ' ' && line[tok_end] != '\t') {
      tok_end++;
    }
    std::string token = line.substr(0, tok_end);

    // Strip UTF-8 BOM if present on the first line.
    if (token.size() >= 3 && (unsigned char)token[0] == 0xEF && (unsigned char)token[1] == 0xBB &&
        (unsigned char)token[2] == 0xBF) {
      token = token.substr(3);
    }

    if (token.empty()) {
      continue;
    }
    if (token[0] == '#' || token[0] == ';') {
      continue;
    }
    if (token.size() >= 2 && token[0] == ':' && token[1] == ':') {
      continue;
    }
    if (EqualsIgnoreCaseAscii(token, "rem")) {
      continue;
    }

    out_tests->push_back(token);
  }

  return true;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    PrintUsage();
    return 0;
  }

  std::wstring bin_dir;
  std::wstring manifest_path;
  DWORD timeout_ms = 0;
  bool enforce_timeout = true;
  bool emit_json = false;
  std::wstring json_path;
  std::vector<std::wstring> forwarded_args;
  if (!ParseRunnerArgs(argc,
                       argv,
                       &bin_dir,
                       &manifest_path,
                       &timeout_ms,
                       &enforce_timeout,
                       &emit_json,
                       &json_path,
                       &forwarded_args)) {
    return 1;
  }

  const aerogpu_test::TestReportAdapterInfo suite_adapter = QueryDefaultAdapterInfo();

  // Legacy fallback list (for older checkouts without tests_manifest.txt).
  const char* const kFallbackTests[] = {
      "d3d9ex_dwm_probe",
      "d3d9ex_event_query",
      "d3d9ex_dwm_ddi_sanity",
      "d3d9ex_submit_fence_stress",
      "vblank_wait_sanity",
      "vblank_wait",
      "wait_vblank_pacing",
      "vblank_wait_pacing",
      "get_scanline_sanity",
      "d3d9_raster_status_sanity",
      "d3d9_raster_status_pacing",
      "dwm_flush_pacing",
      "d3d9ex_triangle",
      "d3d9ex_multiframe_triangle",
      "d3d9ex_stretchrect",
      "d3d9ex_query_latency",
      "d3d9ex_shared_surface",
      "d3d9ex_shared_surface_ipc",
      "d3d9ex_shared_surface_wow64",
      "d3d9ex_shared_surface_many_producers",
      "d3d9ex_shared_allocations",
      "d3d10_triangle",
      "d3d10_map_do_not_wait",
      "d3d10_1_triangle",
      "d3d10_1_map_do_not_wait",
      "d3d10_caps_smoke",
      "d3d11_triangle",
      "d3d11_map_do_not_wait",
      "d3d11_texture",
      "d3d11_caps_smoke",
      "d3d11_rs_om_state_sanity",
      "d3d11_geometry_shader_smoke",
      "dxgi_swapchain_probe",
      "d3d11_swapchain_rotate_sanity",
      "d3d11_map_dynamic_buffer_sanity",
      "d3d11_map_roundtrip",
      "d3d11_update_subresource_texture_sanity",
      "d3d11_texture_sampling_sanity",
      "d3d11_dynamic_constant_buffer_sanity",
      "d3d11_depth_test_sanity",
      "readback_sanity",
  };

  std::vector<std::string> tests;
  std::wstring suite_root_dir;
  bool allow_skipping_missing_tests = false;
  if (!manifest_path.empty()) {
    std::string manifest_err;
    if (!ReadTestsFromManifest(manifest_path, &tests, &manifest_err)) {
      aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: failed to read manifest %ls: %s",
                                 manifest_path.c_str(),
                                 manifest_err.c_str());
      return 1;
    }
    suite_root_dir = DirName(manifest_path);
    aerogpu_test::PrintfStdout("INFO: manifest=%ls (%u test(s))",
                               manifest_path.c_str(),
                               (unsigned)tests.size());

    // Only skip missing binaries when the manifest is part of a source checkout (i.e. when at least
    // one test source directory exists next to it). This matches run_all.cmd behavior in-tree, while
    // keeping "bin-only" distributions strict (missing binaries should fail).
    for (size_t i = 0; i < tests.size(); ++i) {
      const std::wstring leaf = aerogpu_test::Utf8ToWideFallbackAcp(tests[i]);
      const std::wstring test_dir = aerogpu_test::JoinPath(suite_root_dir, leaf.c_str());
      if (DirExistsW(test_dir)) {
        allow_skipping_missing_tests = true;
        break;
      }
    }
    if (!allow_skipping_missing_tests) {
      aerogpu_test::PrintfStdout(
          "INFO: aerogpu_test_runner: no test source directories found next to the manifest; "
          "missing binaries will be treated as failures");
    }
  } else {
    tests.reserve(ARRAYSIZE(kFallbackTests));
    for (size_t i = 0; i < ARRAYSIZE(kFallbackTests); ++i) {
      tests.push_back(std::string(kFallbackTests[i]));
    }
    aerogpu_test::PrintfStdout("INFO: manifest not found; using built-in test list (%u test(s))",
                               (unsigned)tests.size());
  }

  const std::wstring report_dir = emit_json ? DirName(json_path) : std::wstring();
  std::vector<std::string> test_json_objects;
  test_json_objects.reserve(tests.size());

  int failures = 0;

  if (enforce_timeout) {
    aerogpu_test::PrintfStdout("INFO: timeout=%lu ms", (unsigned long)timeout_ms);
  } else {
    aerogpu_test::PrintfStdout("INFO: timeout disabled");
  }

  for (size_t i = 0; i < tests.size(); ++i) {
    const std::string test_name(tests[i]);
    const std::wstring exe_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".exe");
    const std::wstring exe_path = aerogpu_test::JoinPath(bin_dir, exe_leaf.c_str());

    aerogpu_test::PrintfStdout("");
    aerogpu_test::PrintfStdout("=== Running %s ===", test_name.c_str());

    aerogpu_test::TestReport fallback;
    fallback.test_name = test_name;
    fallback.adapter = suite_adapter;

    if (!FileExistsW(exe_path)) {
      bool should_skip = false;
      if (allow_skipping_missing_tests && !suite_root_dir.empty()) {
        const std::wstring test_dir_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name);
        const std::wstring test_dir = aerogpu_test::JoinPath(suite_root_dir, test_dir_leaf.c_str());
        if (!DirExistsW(test_dir)) {
          should_skip = true;
        }
      }

      if (should_skip) {
        aerogpu_test::PrintfStdout("INFO: skipping %s (not present in this checkout)", test_name.c_str());
        fallback.status = "PASS";
        fallback.exit_code = 0;
        fallback.skipped = true;
        fallback.skip_reason = "not present in this checkout";
        if (emit_json) {
          test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
        }
        continue;
      }

      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (missing binary: %ls)", test_name.c_str(), exe_path.c_str());
      fallback.status = "FAIL";
      fallback.exit_code = 1;
      fallback.failure = "missing binary";
      if (emit_json) {
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
      }
      continue;
    }

    std::vector<std::wstring> args = forwarded_args;
    std::wstring per_test_json_path;
    if (emit_json) {
      const std::wstring json_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".json");
      if (!report_dir.empty()) {
        per_test_json_path = aerogpu_test::JoinPath(report_dir, json_leaf.c_str());
      } else {
        per_test_json_path = json_leaf;
      }
      args.push_back(L"--json=" + per_test_json_path);

      // Avoid consuming stale output from a previous run if the test crashes or otherwise fails to
      // write a report this time.
      DeleteFileW(per_test_json_path.c_str());
    }

    RunResult rr = RunProcessWithTimeoutW(exe_path, args, timeout_ms, enforce_timeout);
    if (!rr.started) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (failed to start: %s)", test_name.c_str(), rr.err.c_str());
      fallback.status = "FAIL";
      fallback.exit_code = 1;
      fallback.failure = rr.err;
      if (emit_json) {
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
      }
      continue;
    }

    if (rr.timed_out) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (timed out after %lu ms)", test_name.c_str(), (unsigned long)timeout_ms);
      fallback.status = "FAIL";
      fallback.exit_code = (int)rr.exit_code;
      fallback.failure = aerogpu_test::FormatString("timed out after %lu ms", (unsigned long)timeout_ms);
      if (emit_json) {
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
      }
      continue;
    }

    if (rr.exit_code != 0) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (exit_code=%lu)", test_name.c_str(), (unsigned long)rr.exit_code);
    } else {
      aerogpu_test::PrintfStdout("PASS: %s", test_name.c_str());
    }

    if (emit_json) {
      std::string obj = ReadJsonFileOrEmpty(per_test_json_path);
      if (!obj.empty()) {
        // Most tests include adapter info, but some low-level tests intentionally avoid instantiating
        // D3D/DXGI and therefore leave the adapter field null. Keep the suite report useful by
        // populating the adapter from the suite-level D3D9Ex query when available.
        if (suite_adapter.present) {
          const char* const kNeedle = "\"adapter\":null";
          size_t pos = obj.find(kNeedle);
          if (pos != std::string::npos) {
            const std::string replacement = std::string("\"adapter\":") + BuildAdapterJsonObject(suite_adapter);
            obj.replace(pos, strlen(kNeedle), replacement);
          }
        }
        test_json_objects.push_back(obj);
      } else {
        // Best-effort fallback if the child couldn't write its report.
        fallback.status = (rr.exit_code == 0) ? "PASS" : "FAIL";
        fallback.exit_code = (int)rr.exit_code;
        fallback.failure = (rr.exit_code == 0)
                               ? std::string()
                               : aerogpu_test::FormatString("exit_code=%lu", (unsigned long)rr.exit_code);
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
      }
    }
  }

  aerogpu_test::PrintfStdout("");
  if (failures == 0) {
    aerogpu_test::PrintfStdout("ALL TESTS PASSED");
  } else {
    aerogpu_test::PrintfStdout("%d TEST(S) FAILED", failures);
  }

  if (emit_json) {
    std::string suite_json;
    suite_json.reserve(2048);
    suite_json.push_back('{');
    suite_json.append("\"schema_version\":");
    suite_json.append(aerogpu_test::FormatString("%d", aerogpu_test::kAeroGpuTestReportSchemaVersion));
    suite_json.append(",\"suite_name\":");
    aerogpu_test::JsonAppendEscaped(&suite_json, "aerogpu_win7_validation");
    suite_json.append(",\"status\":");
    aerogpu_test::JsonAppendEscaped(&suite_json, (failures == 0) ? "PASS" : "FAIL");
    suite_json.append(",\"failures\":");
    suite_json.append(aerogpu_test::FormatString("%d", failures));
    suite_json.append(",\"tests\":[");
    for (size_t i = 0; i < test_json_objects.size(); ++i) {
      if (i) {
        suite_json.push_back(',');
      }
      suite_json.append(test_json_objects[i]);
    }
    suite_json.append("]}");
    suite_json.push_back('\n');

    std::string err;
    if (!aerogpu_test::WriteFileStringW(json_path, suite_json, &err)) {
      aerogpu_test::PrintfStdout("INFO: aerogpu_test_runner: failed to write JSON report to %ls: %s",
                                 json_path.c_str(),
                                 err.c_str());
    } else {
      aerogpu_test::PrintfStdout("INFO: wrote JSON report: %ls", json_path.c_str());
    }
  }

  return (failures == 0) ? 0 : 1;
}
