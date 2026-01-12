#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_filter_unknown_token_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  // Unknown token should result in filter_on=1 and filter_count=0, and filter out
  // all entrypoints.
  set_env("AEROGPU_D3D9_TRACE_FILTER", "does_not_exist");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // Calls should be filtered out (not recorded).
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceValidateDevice, 0x222, 0, 0, 0);
    trace.ret(S_OK);
  }

  aerogpu::d3d9_trace_on_process_detach();

  const std::string output = slurp_file_after_closing_stderr(out_path);
  if (output.find("dump reason=DLL_PROCESS_DETACH") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason DLL_PROCESS_DETACH (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("filter_on=1") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected filter_on=1 (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("filter_count=0") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected filter_count=0 for unknown filter token (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("entries=0") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected entries=0 under unknown-token filter (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::CreateResource") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out entry to be recorded (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::ValidateDevice") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out entry to be recorded (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}

