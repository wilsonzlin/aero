#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_mode_all_records_duplicates_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  // Use uppercase to verify case-insensitive mode parsing.
  // Also include extra whitespace to verify trimming.
  set_env("AEROGPU_D3D9_TRACE_MODE", "  ALL  ");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // In TRACE_MODE=all, repeated calls to the same entrypoint should all be recorded.
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
  if (output.find("mode=all") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected mode=all (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("entries=2") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected entries=2 in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x111") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected first call a0=0x111 in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected second call a0=0x222 in dump (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}
