#include "aerogpu_trace.h"
#include "trace_test_utils.h"

#include <cstring>

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_on_fail_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }
 
  // Exercise dump-on-fail in TRACE_MODE=unique. The second call to the same
  // entrypoint would normally be suppressed, so this ensures the dump-on-fail
  // path force-records the failing call.
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "1");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");
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

  // Subsequent failing calls should not trigger additional dumps (dump is one-shot).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x333, 0, 0, 0);
    trace.ret(E_INVALIDARG);
  }
 
  std::fflush(stderr);

  const std::string output = slurp_file(out_path);
  int dump_count = 0;
  for (size_t pos = 0; (pos = output.find("dump reason=", pos)) != std::string::npos; ++dump_count) {
    pos += std::strlen("dump reason=");
  }
  if (dump_count != 1) {
    std::fprintf(stdout, "FAIL: expected exactly one dump reason line (count=%d log=%s)\n", dump_count, out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=Device::CreateResource") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason Device::CreateResource (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected failing call arg a0=0x222 (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("hr=0x80070057") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected hr=0x80070057 (E_INVALIDARG) (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}
