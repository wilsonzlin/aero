#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <string>
 
#include "aerogpu_trace.h"

#if !defined(_WIN32)
  #include <unistd.h>
#endif
 
namespace {
 
void set_env(const char* name, const char* value) {
#if defined(_WIN32)
  // Use the Win32 API so the value is visible to GetEnvironmentVariableA.
  if (!name) {
    return;
  }
  if (value) {
    SetEnvironmentVariableA(name, value);
  } else {
    SetEnvironmentVariableA(name, nullptr);
  }
#else
  if (!name) {
    return;
  }
  if (value) {
    setenv(name, value, 1);
  } else {
    unsetenv(name);
  }
#endif
}
 
std::string slurp_file(const std::string& path) {
  std::ifstream in(path.c_str(), std::ios::in | std::ios::binary);
  std::stringstream ss;
  ss << in.rdbuf();
  return ss.str();
}

std::string make_unique_log_path(const char* stem) {
  if (!stem) {
    stem = "aerogpu_d3d9_trace_test";
  }
#if defined(_WIN32)
  const unsigned long pid = GetCurrentProcessId();
#else
  const unsigned long pid = static_cast<unsigned long>(getpid());
#endif
  char buf[256];
  std::snprintf(buf, sizeof(buf), "%s.%lu.log", stem, pid);
  return std::string(buf);
}

int fail(const char* msg) {
  std::fprintf(stdout, "FAIL: %s\n", msg ? msg : "(null)");
  return 1;
}
 
} // namespace
 
#if defined(_WIN32)
// The trace backend uses OutputDebugStringA on Windows, making it difficult to
// capture the output in a portable unit test. The functional paths are still
// exercised by guest-side tests; skip this host-only test on Windows.
int main() {
  return 0;
}
#else
int main() {
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_filter_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }
 
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_FILTER", "ValidateDevice");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
 
  aerogpu::d3d9_trace_init_from_env();
 
  // This call should be filtered out.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }
 
  // This call should be recorded.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceValidateDevice, 0x222, 0, 0, 0);
    trace.ret(S_OK);
  }
 
  aerogpu::d3d9_trace_on_process_detach();
  std::fflush(stderr);
 
  const std::string output = slurp_file(out_path);
  if (output.find("Device::ValidateDevice") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected output to contain Device::ValidateDevice (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::CreateResource") != std::string::npos) {
    std::fprintf(stdout, "FAIL: expected output to NOT contain Device::CreateResource (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("filter_on=1") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected output to contain filter_on=1 (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}
#endif
