#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_mode_default_unique_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  // Exercise whitespace trimming in env_bool parsing.
  set_env("AEROGPU_D3D9_TRACE", " 1 ");
  // Default is "unique"; ensure we don't regress to TRACE_MODE=all.
  set_env("AEROGPU_D3D9_TRACE_MODE", nullptr);
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", " 1 ");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", " 1 ");

  aerogpu::d3d9_trace_init_from_env();

  // Second call to the same entrypoint should be suppressed in unique mode.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceCreateResource, 0x222, 0, 0, 0);
    trace.ret(S_OK);
  }

  aerogpu::d3d9_trace_on_process_detach();

  const std::string output = slurp_file_after_closing_stderr(out_path);
  if (output.find("dump reason=DLL_PROCESS_DETACH") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason DLL_PROCESS_DETACH (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("mode=unique") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected mode=unique by default (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("entries=1") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected entries=1 in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x111") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected a0=0x111 to be recorded (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect second call a0=0x222 in unique-mode default (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}
