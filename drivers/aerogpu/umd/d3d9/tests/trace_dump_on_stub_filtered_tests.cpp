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
 
int main() {
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_on_stub_filtered_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }
 
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_FILTER", "ValidateDevice");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
 
  aerogpu::d3d9_trace_init_from_env();
 
  // The cursor DDI is stubbed, but it should be filtered out here (no dump).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetCursorProperties, 0xabc, 0, 0, 0);
    trace.ret(S_OK);
  }
 
  std::fflush(stderr);
 
  const std::string output = slurp_file(out_path);
  if (output.find("dump reason=") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect dump to trigger for filtered-out stub (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::SetCursorProperties") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out entry to appear (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}

