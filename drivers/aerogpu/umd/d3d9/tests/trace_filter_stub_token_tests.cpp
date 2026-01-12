#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_filter_stub_token_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  // Filter on "stub" to include only entrypoints whose trace names contain the
  // "(stub)" marker.
  set_env("AEROGPU_D3D9_TRACE_FILTER", "stub");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // This call should be filtered out (not stub-tagged).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetCursorProperties, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }

  // This call should be filtered out (not stub-tagged).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x222, 0, 0, 0);
    trace.ret(S_OK);
  }

  // This call should be recorded (stub-tagged).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceProcessVertices, 0x333, 0, 0, 0);
    trace.ret(S_OK);
  }

  aerogpu::d3d9_trace_on_process_detach();
  std::fflush(stderr);

  const std::string output = slurp_file(out_path);
  if (output.find("Device::ProcessVertices (stub)") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected stub-tagged entry to be recorded (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::SetCursorProperties") != std::string::npos) {
    std::fprintf(stdout,
                 "FAIL: did not expect non-stub no-op entry to be recorded under filter=stub (log=%s)\n",
                 out_path.c_str());
    return 1;
  }
  if (output.find("Device::CreateResource") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect non-stub entry to be recorded under filter=stub (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}

