#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_env_bool_variants_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  // Exercise non-numeric env_bool inputs (yes/on/true).
  set_env("AEROGPU_D3D9_TRACE", "yes");
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "on");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "true");

  aerogpu::d3d9_trace_init_from_env();

  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }

  aerogpu::d3d9_trace_on_process_detach();

  const std::string output = slurp_file_after_closing_stderr(out_path);
  if (output.find("aerogpu-d3d9-trace: enabled") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected trace to be enabled via AEROGPU_D3D9_TRACE=yes (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("dump_on_detach=1") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump_on_detach=1 via AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=on (log=%s)\n",
                 out_path.c_str());
    return 1;
  }
  if (output.find("stderr_on=1") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected stderr_on=1 via AEROGPU_D3D9_TRACE_STDERR=true (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=DLL_PROCESS_DETACH") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason DLL_PROCESS_DETACH (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::CreateResource") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected recorded call in dump (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}

