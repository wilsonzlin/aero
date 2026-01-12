#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
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
 
  // The ProcessVertices DDI is stubbed, but it should be filtered out here (no dump).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceProcessVertices, 0xabc, 0, 0, 0);
    trace.ret(S_OK);
  }
 
  std::fflush(stderr);
 
  const std::string output = slurp_file(out_path);
  if (output.find("dump reason=") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect dump to trigger for filtered-out stub (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::ProcessVertices") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out entry to appear (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}
