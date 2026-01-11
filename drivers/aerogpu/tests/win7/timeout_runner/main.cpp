#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <windows.h>

static void PrintUsage() {
  printf("Usage: aerogpu_timeout_runner.exe <timeout_ms> <command> [args...]\n");
  printf("\n");
  printf("Runs a child process with a wall-clock timeout.\n");
  printf("If the child exceeds the timeout, it is terminated and a non-zero exit code is returned.\n");
  printf("\n");
  printf("JSON reporting:\n");
  printf("If the child command line includes --json[=PATH], this wrapper deletes any stale JSON\n");
  printf("output up front and writes a fallback JSON report on timeout/crash/missing output.\n");
}

static std::string QuoteArgForCreateProcess(const char* arg) {
  // From MSDN/CRT rules:
  // - Wrap in quotes if needed (spaces/tabs).
  // - Escape embedded quotes and backslashes preceding them.
  if (!arg) {
    return "\"\"";
  }

  bool needs_quotes = false;
  for (const char* p = arg; *p; ++p) {
    if (*p == ' ' || *p == '\t') {
      needs_quotes = true;
      break;
    }
  }
  if (!needs_quotes) {
    return std::string(arg);
  }

  std::string out;
  out.push_back('"');
  size_t num_backslashes = 0;
  for (const char* p = arg; *p; ++p) {
    if (*p == '\\') {
      num_backslashes++;
      out.push_back('\\');
      continue;
    }
    if (*p == '"') {
      // Escape all backslashes, then escape the quote.
      out.append(num_backslashes, '\\');
      num_backslashes = 0;
      out.push_back('\\');
      out.push_back('"');
      continue;
    }
    num_backslashes = 0;
    out.push_back(*p);
  }
  // Escape trailing backslashes.
  out.append(num_backslashes, '\\');
  out.push_back('"');
  return out;
}

static std::string BasenameWithoutExtA(const char* path) {
  std::string s(path ? path : "");
  size_t pos = s.find_last_of("\\/");
  std::string leaf = (pos == std::string::npos) ? s : s.substr(pos + 1);
  size_t dot = leaf.find_last_of('.');
  if (dot != std::string::npos) {
    leaf = leaf.substr(0, dot);
  }
  return leaf;
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

static bool LooksLikeTestReportJsonObject(const std::string& obj) {
  if (obj.size() < 2) {
    return false;
  }
  if (obj[0] != '{' || obj[obj.size() - 1] != '}') {
    return false;
  }
  // Very small sanity checks to avoid treating truncated/corrupted output as a valid report.
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

static std::wstring DirNameW(const std::wstring& path) {
  size_t pos = path.find_last_of(L"\\/");
  if (pos == std::wstring::npos) {
    return std::wstring();
  }
  return path.substr(0, pos + 1);
}

static std::wstring GetFullPathFallbackW(const std::wstring& path) {
  if (path.empty()) {
    return path;
  }
  wchar_t buf[MAX_PATH];
  DWORD len = GetFullPathNameW(path.c_str(), ARRAYSIZE(buf), buf, NULL);
  if (!len || len >= ARRAYSIZE(buf)) {
    return path;
  }
  return std::wstring(buf, buf + len);
}

static bool ParseChildJsonPath(int argc,
                               char** argv,
                               const std::wstring& child_exe_path_w,
                               std::wstring* out_json_path) {
  if (!out_json_path) {
    return false;
  }
  out_json_path->clear();

  bool emit_json = false;
  const char* json_value = NULL;
  const char* kJsonPrefix = "--json=";
  for (int i = 3; i < argc; ++i) {
    const char* arg = argv[i];
    if (!arg) {
      continue;
    }
    if (aerogpu_test::StrIStartsWith(arg, kJsonPrefix)) {
      emit_json = true;
      json_value = arg + strlen(kJsonPrefix);
      break;
    }
    if (lstrcmpiA(arg, "--json") == 0) {
      emit_json = true;
      if (i + 1 < argc && argv[i + 1] && argv[i + 1][0] != '-') {
        json_value = argv[i + 1];
      }
      break;
    }
  }

  // If --json wasn't supplied, do nothing.
  if (!emit_json) {
    return false;
  }

  // If a path was supplied explicitly, use it.
  if (json_value && *json_value) {
    *out_json_path = aerogpu_test::Utf8ToWideFallbackAcp(std::string(json_value));
    return true;
  }

  // Default path matches TestReporter behavior for the common case where the test name matches the
  // executable base name.
  const std::wstring exe_full = GetFullPathFallbackW(child_exe_path_w);
  std::wstring dir = DirNameW(exe_full);
  if (dir.empty()) {
    dir = L".\\";
  }
  const std::string test_name = BasenameWithoutExtA(aerogpu_test::WideToUtf8(exe_full).c_str());
  const std::wstring leaf = aerogpu_test::Utf8ToWideFallbackAcp(test_name + ".json");
  *out_json_path = aerogpu_test::JoinPath(dir, leaf.c_str());
  return true;
}

static void WriteFallbackJsonIfEnabled(const std::wstring& json_path_w,
                                       const std::string& test_name,
                                       DWORD exit_code,
                                       const std::string& failure) {
  if (json_path_w.empty() || test_name.empty()) {
    return;
  }
  aerogpu_test::TestReport report;
  report.test_name = test_name;
  report.status = (exit_code == 0) ? "PASS" : "FAIL";
  report.exit_code = (int)exit_code;
  report.failure = (exit_code == 0) ? std::string() : failure;

  std::string json = aerogpu_test::BuildTestReportJson(report);
  json.push_back('\n');
  std::string err;
  if (!aerogpu_test::WriteFileStringW(json_path_w, json, &err)) {
    // Don't change the wrapper outcome if we can't write JSON.
    printf("INFO: timeout_runner: failed to write JSON report to %ls: %s\n",
           json_path_w.c_str(),
           err.c_str());
  }
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (argc < 3 || aerogpu_test::HasHelpArg(argc, argv)) {
    PrintUsage();
    return (argc < 3) ? 1 : 0;
  }

  std::string timeout_str(argv[1] ? argv[1] : "");
  DWORD timeout_ms = 0;
  std::string parse_err;
  if (!aerogpu_test::ParseUint32(timeout_str, (uint32_t*)&timeout_ms, &parse_err) || timeout_ms == 0) {
    printf("FAIL: timeout_runner: invalid timeout_ms: %s\n", parse_err.c_str());
    return 1;
  }

  const std::string test_name = BasenameWithoutExtA(argv[2]);
  const std::wstring child_exe_path_w = aerogpu_test::Utf8ToWideFallbackAcp(std::string(argv[2] ? argv[2] : ""));
  std::wstring json_path_w;
  const bool emit_json = ParseChildJsonPath(argc, argv, child_exe_path_w, &json_path_w);
  if (emit_json && !json_path_w.empty()) {
    // Avoid leaving behind stale output if the child crashes or times out before writing a report.
    DeleteFileW(json_path_w.c_str());
  }

  // Build a command line string from argv[2..] that round-trips correctly through CreateProcess.
  std::string cmdline;
  for (int i = 2; i < argc; ++i) {
    if (i != 2) {
      cmdline.push_back(' ');
    }
    cmdline += QuoteArgForCreateProcess(argv[i]);
  }
  // CreateProcess requires a writable buffer.
  std::vector<char> cmdline_buf(cmdline.begin(), cmdline.end());
  cmdline_buf.push_back(0);

  STARTUPINFOA si;
  ZeroMemory(&si, sizeof(si));
  si.cb = sizeof(si);

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  BOOL ok = CreateProcessA(argv[2],
                           &cmdline_buf[0],
                           NULL,
                           NULL,
                           FALSE,
                           0,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (!ok) {
    DWORD err = GetLastError();
    const std::string msg = "CreateProcess failed: " + aerogpu_test::Win32ErrorToString(err);
    printf("FAIL: timeout_runner: %s\n", msg.c_str());
    if (emit_json) {
      WriteFallbackJsonIfEnabled(json_path_w, test_name, 1, msg);
    }
    return 1;
  }

  // Best-effort job object so a timed out test can't leave behind orphaned helper processes.
  // Some tests spawn child processes; JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE ensures the whole tree
  // is cleaned up when we terminate the job.
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

  DWORD wait = WaitForSingleObject(pi.hProcess, timeout_ms);
  if (wait == WAIT_TIMEOUT) {
    printf("FAIL: timeout_runner: process timed out after %lu ms: %s\n",
           (unsigned long)timeout_ms,
           argv[2]);
    if (job) {
      TerminateJobObject(job, 124);
    } else {
      TerminateProcess(pi.hProcess, 124);
    }
    WaitForSingleObject(pi.hProcess, 5000);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (emit_json) {
      WriteFallbackJsonIfEnabled(
          json_path_w,
          test_name,
          124,
          aerogpu_test::FormatString("timed out after %lu ms", (unsigned long)timeout_ms));
    }
    return 124;
  }
  if (wait != WAIT_OBJECT_0) {
    DWORD err = GetLastError();
    const std::string msg = "WaitForSingleObject failed: " + aerogpu_test::Win32ErrorToString(err);
    printf("FAIL: timeout_runner: %s\n", msg.c_str());
    if (job) {
      TerminateJobObject(job, 1);
    } else {
      TerminateProcess(pi.hProcess, 1);
    }
    WaitForSingleObject(pi.hProcess, 5000);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (emit_json) {
      WriteFallbackJsonIfEnabled(json_path_w, test_name, 1, msg);
    }
    return 1;
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    DWORD err = GetLastError();
    const std::string msg = "GetExitCodeProcess failed: " + aerogpu_test::Win32ErrorToString(err);
    printf("FAIL: timeout_runner: %s\n", msg.c_str());
    exit_code = 1;
    if (emit_json) {
      WriteFallbackJsonIfEnabled(json_path_w, test_name, 1, msg);
    }
  }

  if (job) {
    CloseHandle(job);
  }
  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (emit_json && !json_path_w.empty()) {
    bool have_json = false;
    DWORD attr = GetFileAttributesW(json_path_w.c_str());
    if (attr != INVALID_FILE_ATTRIBUTES && (attr & FILE_ATTRIBUTE_DIRECTORY) == 0) {
      std::vector<unsigned char> bytes;
      std::string read_err;
      if (aerogpu_test::ReadFileBytes(json_path_w, &bytes, &read_err)) {
        std::string obj = TrimAsciiWhitespace(std::string(bytes.begin(), bytes.end()));
        if (!obj.empty() && LooksLikeTestReportJsonObject(obj)) {
          have_json = true;
        }
      }
      if (!have_json) {
        printf("INFO: timeout_runner: invalid JSON report from child; writing fallback: %ls\n",
               json_path_w.c_str());
        DeleteFileW(json_path_w.c_str());
      }
    }
    if (!have_json) {
      const std::string msg =
          (exit_code == 0)
              ? std::string()
              : aerogpu_test::FormatString("exit_code=%lu", (unsigned long)exit_code);
      WriteFallbackJsonIfEnabled(json_path_w, test_name, exit_code, msg);
    }
  }
  return (int)exit_code;
}
