#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;

  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_present_force_record_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  // Use unique mode so the second PresentEx call is suppressed unless the dump
  // trigger force-records it.
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "2");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0x111, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(1);
  }

  // Second call to the same entrypoint should be suppressed in TRACE_MODE=unique,
  // but the dump-on-present trigger should force-record it so we can see the
  // call that actually caused the dump.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0x222, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(2);
  }

  std::fflush(stderr);

  const std::string output = slurp_file(out_path);
  if (output.find("dump reason=present_count") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason present_count (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x111") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected first PresentEx call a0=0x111 in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") == std::string::npos) {
    std::fprintf(stdout,
                 "FAIL: expected triggering PresentEx call a0=0x222 in dump (force-recorded) (log=%s)\n",
                 out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}

