#include "..\\common\\aerogpu_test_common.h"

#include <windows.h>

static void PrintUsage() {
  printf("Usage: aerogpu_timeout_runner.exe <timeout_ms> <command> [args...]\n");
  printf("\n");
  printf("Runs a child process with a wall-clock timeout.\n");
  printf("If the child exceeds the timeout, it is terminated and a non-zero exit code is returned.\n");
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
                           TRUE,
                           0,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (!ok) {
    DWORD err = GetLastError();
    printf("FAIL: timeout_runner: CreateProcess failed: %s\n",
           aerogpu_test::Win32ErrorToString(err).c_str());
    return 1;
  }

  DWORD wait = WaitForSingleObject(pi.hProcess, timeout_ms);
  if (wait == WAIT_TIMEOUT) {
    printf("FAIL: timeout_runner: process timed out after %lu ms: %s\n",
           (unsigned long)timeout_ms,
           argv[2]);
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return 124;
  }
  if (wait != WAIT_OBJECT_0) {
    DWORD err = GetLastError();
    printf("FAIL: timeout_runner: WaitForSingleObject failed: %s\n",
           aerogpu_test::Win32ErrorToString(err).c_str());
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return 1;
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    DWORD err = GetLastError();
    printf("FAIL: timeout_runner: GetExitCodeProcess failed: %s\n",
           aerogpu_test::Win32ErrorToString(err).c_str());
    exit_code = 1;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  return (int)exit_code;
}
