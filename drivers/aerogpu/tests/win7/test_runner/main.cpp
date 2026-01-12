#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <windows.h>
#include <d3d9.h>

#include <string>
#include <vector>

static void PrintUsage() {
  aerogpu_test::PrintfStdout(
      "Usage: aerogpu_test_runner.exe [--bin-dir=DIR] [--manifest=PATH] [--timeout-ms=NNNN] [--no-timeout] "
      "[--json[=PATH]] [--log-dir=DIR] [--dbgctl=PATH] [--dbgctl-timeout-ms=NNNN] [test flags...]");
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
  aerogpu_test::PrintfStdout("                        Also writes per-test <test>.json files next to the suite report.");
  aerogpu_test::PrintfStdout(
      "  --log-dir=DIR         If set, redirect each test's stdout/stderr to <test>.stdout.txt / <test>.stderr.txt in DIR.");
  aerogpu_test::PrintfStdout("  --dbgctl=PATH         Optional path to aerogpu_dbgctl.exe; if set, run '--status' after test failures/timeouts.");
  aerogpu_test::PrintfStdout("  --dbgctl-timeout-ms=NNNN  Timeout for the dbgctl process itself (wrapper kill). Default: 5000.");
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
  info.description_utf8 = aerogpu_test::NarrowToUtf8FallbackAcp(ident.Description);
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

struct ProcessOutputFiles {
  std::wstring stdout_path;
  std::wstring stderr_path;
};

static bool IsAbsolutePathW(const std::wstring& path) {
  if (path.size() >= 2 && path[1] == L':') {
    return true;
  }
  if (path.size() >= 2 && path[0] == L'\\' && path[1] == L'\\') {
    return true;
  }
  if (!path.empty() && (path[0] == L'\\' || path[0] == L'/')) {
    return true;
  }
  return false;
}

static bool EnsureDirExistsRecursive(const std::wstring& path, std::string* err) {
  if (path.empty()) {
    return true;
  }

  // Trim trailing separators.
  std::wstring dir = path;
  while (!dir.empty()) {
    wchar_t last = dir[dir.size() - 1];
    if (last != L'\\' && last != L'/') {
      break;
    }
    dir.resize(dir.size() - 1);
  }
  if (dir.empty()) {
    return true;
  }

  DWORD attr = GetFileAttributesW(dir.c_str());
  if (attr != INVALID_FILE_ATTRIBUTES) {
    if ((attr & FILE_ATTRIBUTE_DIRECTORY) != 0) {
      return true;
    }
    if (err) {
      *err = "path exists but is not a directory";
    }
    return false;
  }

  // Create parent first (if any).
  size_t slash = dir.find_last_of(L"\\/");
  if (slash != std::wstring::npos) {
    const std::wstring parent = dir.substr(0, slash);
    if (!parent.empty() && !EnsureDirExistsRecursive(parent, err)) {
      return false;
    }
  }

  if (!CreateDirectoryW(dir.c_str(), NULL)) {
    DWORD e = GetLastError();
    if (e != ERROR_ALREADY_EXISTS) {
      if (err) {
        *err = "CreateDirectory failed: " + aerogpu_test::Win32ErrorToString(e);
      }
      return false;
    }
  }

  return true;
}

static HANDLE CreateInheritableFileForWriteW(const std::wstring& path, std::string* err) {
  SECURITY_ATTRIBUTES sa;
  ZeroMemory(&sa, sizeof(sa));
  sa.nLength = sizeof(sa);
  sa.bInheritHandle = TRUE;

  HANDLE h = CreateFileW(path.c_str(),
                         GENERIC_WRITE,
                         FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                         &sa,
                         CREATE_ALWAYS,
                         FILE_ATTRIBUTE_NORMAL,
                         NULL);
  if (h == INVALID_HANDLE_VALUE) {
    if (err) {
      *err = "CreateFileW failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return INVALID_HANDLE_VALUE;
  }
  return h;
}

static RunResult RunProcessWithTimeoutW(const std::wstring& exe_path,
                                       const std::vector<std::wstring>& args,
                                       DWORD timeout_ms,
                                       bool enforce_timeout,
                                       const ProcessOutputFiles* output_files) {
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

  HANDLE stdout_file = INVALID_HANDLE_VALUE;
  HANDLE stderr_file = INVALID_HANDLE_VALUE;
  if (output_files && (!output_files->stdout_path.empty() || !output_files->stderr_path.empty())) {
    const std::wstring& stdout_path = output_files->stdout_path;
    const std::wstring& stderr_path = output_files->stderr_path;

    if (!stdout_path.empty()) {
      stdout_file = CreateInheritableFileForWriteW(stdout_path, &out.err);
      if (stdout_file == INVALID_HANDLE_VALUE) {
        return out;
      }
    }
    if (!stderr_path.empty() && stderr_path == stdout_path) {
      stderr_file = stdout_file;
    } else if (!stderr_path.empty()) {
      stderr_file = CreateInheritableFileForWriteW(stderr_path, &out.err);
      if (stderr_file == INVALID_HANDLE_VALUE) {
        if (stdout_file != INVALID_HANDLE_VALUE) {
          CloseHandle(stdout_file);
        }
        return out;
      }
    }

    si.dwFlags |= STARTF_USESTDHANDLES;
    si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    si.hStdOutput = (stdout_file != INVALID_HANDLE_VALUE) ? stdout_file : GetStdHandle(STD_OUTPUT_HANDLE);
    si.hStdError = (stderr_file != INVALID_HANDLE_VALUE) ? stderr_file : GetStdHandle(STD_ERROR_HANDLE);
  }

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  // Only opt into inheriting handles when we explicitly configured stdio redirection. This keeps the
  // default behavior closer to the old runner (and avoids leaking unrelated inheritable handles
  // into child processes in environments where the runner is embedded).
  const BOOL inherit_handles = (si.dwFlags & STARTF_USESTDHANDLES) ? TRUE : FALSE;
  BOOL ok = CreateProcessW(exe_path.c_str(),
                           &cmdline_buf[0],
                           NULL,
                           NULL,
                           inherit_handles,
                           0,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (stdout_file != INVALID_HANDLE_VALUE) {
    CloseHandle(stdout_file);
  }
  if (stderr_file != INVALID_HANDLE_VALUE && stderr_file != stdout_file) {
    CloseHandle(stderr_file);
  }

  if (!ok) {
    out.err = "CreateProcess failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    return out;
  }

  // Best-effort job object to ensure child process trees are cleaned up on timeout.
  // Some tests spawn helper processes; without this, a timeout can leave orphans running and
  // interfere with subsequent tests.
  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (job) {
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
    ZeroMemory(&info, sizeof(info));
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info)) ||
        !AssignProcessToJobObject(job, pi.hProcess)) {
      CloseHandle(job);
      job = NULL;
    }
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
    if (job) {
      TerminateJobObject(job, 124);
    } else {
      TerminateProcess(pi.hProcess, 124);
    }
    WaitForSingleObject(pi.hProcess, 5000);
    out.exit_code = 124;
  } else if (wait != WAIT_OBJECT_0) {
    out.err = "WaitForSingleObject failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    if (job) {
      TerminateJobObject(job, 1);
    } else {
      TerminateProcess(pi.hProcess, 1);
    }
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

  if (job) {
    CloseHandle(job);
  }
  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  return out;
}

static bool DumpDbgctlStatusSnapshotBestEffort(const std::wstring& dbgctl_path,
                                               const std::wstring& out_dir,
                                               const std::string& test_name,
                                               DWORD dbgctl_timeout_ms,
                                               std::wstring* out_path,
                                               std::string* err) {
  if (out_path) {
    out_path->clear();
  }
  if (err) {
    err->clear();
  }
  if (dbgctl_path.empty() || out_dir.empty() || test_name.empty() || dbgctl_timeout_ms == 0) {
    return false;
  }

  std::string mk_err;
  if (!EnsureDirExistsRecursive(out_dir, &mk_err)) {
    if (err) {
      *err = mk_err;
    }
    return false;
  }

  const std::wstring leaf = aerogpu_test::Utf8ToWideFallbackAcp("dbgctl_" + test_name + "_status.txt");
  const std::wstring snapshot_path = aerogpu_test::JoinPath(out_dir, leaf.c_str());

  ProcessOutputFiles out_files;
  out_files.stdout_path = snapshot_path;
  out_files.stderr_path = snapshot_path;  // combined

  std::vector<std::wstring> args;
  args.push_back(L"--status");
  args.push_back(L"--timeout-ms");
  args.push_back(aerogpu_test::Utf8ToWideFallbackAcp(
      aerogpu_test::FormatString("%lu", (unsigned long)dbgctl_timeout_ms)));

  RunResult rr = RunProcessWithTimeoutW(dbgctl_path, args, dbgctl_timeout_ms, true, &out_files);
  if (!rr.started) {
    if (err) {
      *err = rr.err;
    }
    return false;
  }

  if (out_path) {
    *out_path = snapshot_path;
  }
  return true;
}

static bool ParseRunnerArgs(int argc,
                            char** argv,
                            std::wstring* out_bin_dir,
                            std::wstring* out_manifest_path,
                            DWORD* out_timeout_ms,
                            bool* out_enforce_timeout,
                            bool* out_emit_json,
                            std::wstring* out_json_path,
                            std::wstring* out_log_dir,
                            std::wstring* out_dbgctl_path,
                            DWORD* out_dbgctl_timeout_ms,
                            std::vector<std::wstring>* out_forwarded_args) {
  if (!out_bin_dir || !out_manifest_path || !out_timeout_ms || !out_enforce_timeout || !out_emit_json ||
      !out_json_path || !out_log_dir || !out_dbgctl_path || !out_dbgctl_timeout_ms || !out_forwarded_args) {
    return false;
  }

  *out_bin_dir = aerogpu_test::GetModuleDir();
  out_manifest_path->clear();
  out_log_dir->clear();
  out_dbgctl_path->clear();
  *out_dbgctl_timeout_ms = 5000;

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
  const char* const kLogDirPrefix = "--log-dir=";
  const char* const kDbgctlPrefix = "--dbgctl=";
  const char* const kDbgctlTimeoutPrefix = "--dbgctl-timeout-ms=";

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

    if (aerogpu_test::StrIStartsWith(arg, kLogDirPrefix)) {
      const std::string val(arg + strlen(kLogDirPrefix));
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --log-dir missing value");
        return false;
      }
      *out_log_dir = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }
    if (lstrcmpiA(arg, "--log-dir") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --log-dir missing value");
        return false;
      }
      const std::string val(argv[++i]);
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --log-dir missing value");
        return false;
      }
      *out_log_dir = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kDbgctlPrefix)) {
      const std::string val(arg + strlen(kDbgctlPrefix));
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --dbgctl missing value");
        return false;
      }
      *out_dbgctl_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }
    if (lstrcmpiA(arg, "--dbgctl") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --dbgctl missing value");
        return false;
      }
      const std::string val(argv[++i]);
      if (val.empty()) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --dbgctl missing value");
        return false;
      }
      *out_dbgctl_path = aerogpu_test::Utf8ToWideFallbackAcp(val);
      continue;
    }

    if (aerogpu_test::StrIStartsWith(arg, kDbgctlTimeoutPrefix)) {
      const std::string val(arg + strlen(kDbgctlTimeoutPrefix));
      uint32_t parsed = 0;
      std::string parse_err;
      if (!aerogpu_test::ParseUint32(val, &parsed, &parse_err) || parsed == 0) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: invalid --dbgctl-timeout-ms: %s", parse_err.c_str());
        return false;
      }
      *out_dbgctl_timeout_ms = (DWORD)parsed;
      continue;
    }
    if (lstrcmpiA(arg, "--dbgctl-timeout-ms") == 0) {
      if (i + 1 >= argc || !argv[i + 1]) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: --dbgctl-timeout-ms missing value");
        return false;
      }
      const std::string val(argv[++i]);
      uint32_t parsed = 0;
      std::string parse_err;
      if (!aerogpu_test::ParseUint32(val, &parsed, &parse_err) || parsed == 0) {
        aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: invalid --dbgctl-timeout-ms: %s", parse_err.c_str());
        return false;
      }
      *out_dbgctl_timeout_ms = (DWORD)parsed;
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

static bool LooksLikeTestReportJsonObject(const std::string& obj) {
  if (obj.size() < 2) {
    return false;
  }
  if (obj[0] != '{' || obj[obj.size() - 1] != '}') {
    return false;
  }
  // Very small sanity checks to avoid embedding truncated/corrupted output into the suite JSON.
  // We intentionally do not attempt to fully parse JSON here (no dependency and no STL iostreams).
  if (obj.find("\"schema_version\":") == std::string::npos) {
    return false;
  }
  if (obj.find("\"test_name\":") == std::string::npos) {
    return false;
  }
  if (obj.find("\"status\":") == std::string::npos) {
    return false;
  }
  if (obj.find("\"exit_code\":") == std::string::npos) {
    return false;
  }
  return true;
}

static void WriteTestReportJsonBestEffort(const std::wstring& path, const aerogpu_test::TestReport& report) {
  if (path.empty()) {
    return;
  }
  std::string json = aerogpu_test::BuildTestReportJson(report);
  json.push_back('\n');
  std::string err;
  if (!aerogpu_test::WriteFileStringW(path, json, &err)) {
    // Reporting should not change the test outcome.
    aerogpu_test::PrintfStdout("INFO: aerogpu_test_runner: failed to write per-test JSON report to %ls: %s",
                               path.c_str(),
                               err.c_str());
  }
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
  std::wstring log_dir;
  std::wstring dbgctl_path;
  DWORD dbgctl_timeout_ms = 5000;
  std::vector<std::wstring> forwarded_args;
  if (!ParseRunnerArgs(argc,
                       argv,
                       &bin_dir,
                       &manifest_path,
                       &timeout_ms,
                       &enforce_timeout,
                       &emit_json,
                       &json_path,
                       &log_dir,
                       &dbgctl_path,
                       &dbgctl_timeout_ms,
                       &forwarded_args)) {
    return 1;
  }

  const aerogpu_test::TestReportAdapterInfo suite_adapter = QueryDefaultAdapterInfo();

  if (!log_dir.empty()) {
    if (!IsAbsolutePathW(log_dir)) {
      log_dir = aerogpu_test::JoinPath(bin_dir, log_dir.c_str());
    }
    std::string err;
    if (!EnsureDirExistsRecursive(log_dir, &err)) {
      aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: failed to create log dir %ls: %s",
                                 log_dir.c_str(),
                                 err.c_str());
      return 1;
    }
    aerogpu_test::PrintfStdout("INFO: capturing per-test stdout/stderr to %ls", log_dir.c_str());
  }

  if (!dbgctl_path.empty()) {
    if (!IsAbsolutePathW(dbgctl_path)) {
      dbgctl_path = aerogpu_test::JoinPath(bin_dir, dbgctl_path.c_str());
    }
    if (!FileExistsW(dbgctl_path)) {
      aerogpu_test::PrintfStdout("FAIL: aerogpu_test_runner: dbgctl binary not found: %ls",
                                 dbgctl_path.c_str());
      return 1;
    }
  }

  // Legacy fallback list (for older checkouts without tests_manifest.txt).
  const char* const kFallbackTests[] = {
      "device_state_sanity",
      "d3d9ex_dwm_probe",
      "d3d9ex_event_query",
      "d3d9ex_dwm_ddi_sanity",
      "d3d9ex_getters_sanity",
      "d3d9ex_submit_fence_stress",
      "fence_state_sanity",
      "ring_state_sanity",
      "vblank_wait_sanity",
      "vblank_wait",
      "wait_vblank_pacing",
      "vblank_wait_pacing",
      "vblank_state_sanity",
      "get_scanline_sanity",
      "scanout_state_sanity",
      "dump_createalloc_sanity",
      "umd_private_sanity",
      "transfer_feature_sanity",
      "d3d9_raster_status_sanity",
      "d3d9_raster_status_pacing",
      "d3d9_validate_device_sanity",
      "d3d9_get_state_roundtrip",
      "dwm_flush_pacing",
      "d3d9ex_triangle",
      "d3d9ex_stateblock_sanity",
      "d3d9ex_scissor_sanity",
      "d3d9ex_draw_indexed_primitive_up",
      "d3d9ex_multiframe_triangle",
      "d3d9ex_vb_dirty_range",
      "d3d9ex_stretchrect",
      "d3d9ex_query_latency",
      "d3d9ex_shared_surface",
      "d3d9ex_shared_surface_ipc",
      "d3d9ex_alloc_id_persistence",
      "d3d9ex_shared_surface_wow64",
      "d3d9ex_shared_surface_many_producers",
      "d3d9ex_shared_allocations",
      "d3d9ex_shared_surface_stress",
      "d3d10_triangle",
      "d3d10_map_do_not_wait",
      "d3d10_shared_surface_ipc",
      "d3d10_1_triangle",
      "d3d10_1_map_do_not_wait",
      "d3d10_1_shared_surface_ipc",
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
      "d3d11_shared_surface_ipc",
      "d3d11_texture_sampling_sanity",
      "d3d11_texture_mips_array_sanity",
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
    std::wstring per_test_json_path;
    if (emit_json) {
      const std::wstring json_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".json");
      if (!report_dir.empty()) {
        per_test_json_path = aerogpu_test::JoinPath(report_dir, json_leaf.c_str());
      } else {
        per_test_json_path = json_leaf;
      }

      // Avoid consuming stale output from a previous run if the test crashes or otherwise fails to
      // write a report this time.
      DeleteFileW(per_test_json_path.c_str());
    }

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
          WriteTestReportJsonBestEffort(per_test_json_path, fallback);
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
        WriteTestReportJsonBestEffort(per_test_json_path, fallback);
      }
      continue;
    }

    std::vector<std::wstring> args = forwarded_args;
    if (emit_json) {
      args.push_back(L"--json=" + per_test_json_path);
    }

    ProcessOutputFiles out_files;
    const ProcessOutputFiles* out_files_ptr = NULL;
    if (!log_dir.empty()) {
      out_files_ptr = &out_files;
      const std::wstring stdout_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".stdout.txt");
      const std::wstring stderr_leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".stderr.txt");
      out_files.stdout_path = aerogpu_test::JoinPath(log_dir, stdout_leaf.c_str());
      out_files.stderr_path = aerogpu_test::JoinPath(log_dir, stderr_leaf.c_str());
    }

    RunResult rr = RunProcessWithTimeoutW(exe_path, args, timeout_ms, enforce_timeout, out_files_ptr);
    if (!rr.started) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (failed to start: %s)", test_name.c_str(), rr.err.c_str());
      fallback.status = "FAIL";
      fallback.exit_code = 1;
      fallback.failure = rr.err;
      if (emit_json) {
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
        WriteTestReportJsonBestEffort(per_test_json_path, fallback);
      }
      continue;
    }

    if (rr.timed_out) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (timed out after %lu ms)", test_name.c_str(), (unsigned long)timeout_ms);
      fallback.status = "FAIL";
      fallback.exit_code = (int)rr.exit_code;
      fallback.failure = aerogpu_test::FormatString("timed out after %lu ms", (unsigned long)timeout_ms);

      if (!dbgctl_path.empty()) {
        const std::wstring out_dir = !log_dir.empty() ? log_dir : (!report_dir.empty() ? report_dir : bin_dir);
        std::wstring snapshot_path;
        std::string snapshot_err;
        if (DumpDbgctlStatusSnapshotBestEffort(
                dbgctl_path, out_dir, test_name, dbgctl_timeout_ms, &snapshot_path, &snapshot_err)) {
          aerogpu_test::PrintfStdout("INFO: wrote dbgctl status snapshot: %ls", snapshot_path.c_str());
        } else if (!snapshot_err.empty()) {
          aerogpu_test::PrintfStdout("INFO: dbgctl snapshot failed: %s", snapshot_err.c_str());
        }
      }

      if (emit_json) {
        test_json_objects.push_back(aerogpu_test::BuildTestReportJson(fallback));
        WriteTestReportJsonBestEffort(per_test_json_path, fallback);
      }
      continue;
    }

    if (rr.exit_code != 0) {
      failures++;
      aerogpu_test::PrintfStdout("FAIL: %s (exit_code=%lu)", test_name.c_str(), (unsigned long)rr.exit_code);
      if (!dbgctl_path.empty()) {
        const std::wstring out_dir = !log_dir.empty() ? log_dir : (!report_dir.empty() ? report_dir : bin_dir);
        std::wstring snapshot_path;
        std::string snapshot_err;
        if (DumpDbgctlStatusSnapshotBestEffort(
                dbgctl_path, out_dir, test_name, dbgctl_timeout_ms, &snapshot_path, &snapshot_err)) {
          aerogpu_test::PrintfStdout("INFO: wrote dbgctl status snapshot: %ls", snapshot_path.c_str());
        } else if (!snapshot_err.empty()) {
          aerogpu_test::PrintfStdout("INFO: dbgctl snapshot failed: %s", snapshot_err.c_str());
        }
      }
    } else {
      aerogpu_test::PrintfStdout("PASS: %s", test_name.c_str());
    }

    if (emit_json) {
      std::string obj = ReadJsonFileOrEmpty(per_test_json_path);
      if (!obj.empty() && !LooksLikeTestReportJsonObject(obj)) {
        aerogpu_test::PrintfStdout("INFO: %s: invalid per-test JSON output; using fallback report",
                                   test_name.c_str());
        obj.clear();
      }
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
        WriteTestReportJsonBestEffort(per_test_json_path, fallback);
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
