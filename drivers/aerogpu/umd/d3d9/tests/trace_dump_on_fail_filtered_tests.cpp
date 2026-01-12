#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_on_fail_filtered_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }
 
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_FILTER", "ValidateDevice");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "1");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
 
  aerogpu::d3d9_trace_init_from_env();
 
  // Filtered out: should not dump and should not be recorded.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(E_INVALIDARG);
  }
 
  // Filtered in: should dump on fail.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceValidateDevice, 0x222, 0, 0, 0);
    trace.ret(E_INVALIDARG);
  }
 
  std::fflush(stderr);
 
  const std::string output = slurp_file(out_path);
  if (output.find("dump reason=Device::ValidateDevice") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason Device::ValidateDevice (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=Device::CreateResource") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect dump reason Device::CreateResource (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::CreateResource") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out entry to appear in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected failing ValidateDevice call arg a0=0x222 (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("hr=0x80070057") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected hr=0x80070057 (E_INVALIDARG) (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}
