#include <cassert>
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
 
} // namespace
 
#if defined(_WIN32)
int main() {
  // See note in trace_filter_tests.cpp: OutputDebugStringA isn't captured here.
  return 0;
}
#else
int main() {
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_on_fail_tests");
  assert(std::freopen(out_path.c_str(), "w", stderr) != nullptr);
 
  // Exercise dump-on-fail in TRACE_MODE=unique. The second call to the same
  // entrypoint would normally be suppressed, so this ensures the dump-on-fail
  // path force-records the failing call.
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
 
  aerogpu::d3d9_trace_init_from_env();
 
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }
 
  // Failing call to the same entrypoint should still trigger a dump and appear
  // in the trace even though TRACE_MODE=unique would normally suppress it.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x222, 0, 0, 0);
    trace.ret(E_INVALIDARG);
  }
 
  std::fflush(stderr);
 
  const std::string output = slurp_file(out_path);
  assert(output.find("dump reason=Device::CreateResource") != std::string::npos);
  assert(output.find("a0=0x222") != std::string::npos);
  assert(output.find("hr=0x80070057") != std::string::npos);
 
  std::remove(out_path.c_str());
  return 0;
}
#endif
