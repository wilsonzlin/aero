#include "aerogpu_trace.h"
#include "trace_test_utils.h"

#include <cstring>

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_present_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }
 
  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "2");
  // Also enable dump-on-detach; the present-count dump should win (dump is one-shot).
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");
 
  aerogpu::d3d9_trace_init_from_env();
 
  // First present: should not dump yet.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0x111, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(1);
  }

  // Second present: should dump at the configured count.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0x222, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(2);
  }

  // Ensure dump-on-detach does not produce a second dump after dump-on-present.
  aerogpu::d3d9_trace_on_process_detach();

  const std::string output = slurp_file_after_closing_stderr(out_path);
  int dump_count = 0;
  for (size_t pos = 0; (pos = output.find("dump reason=", pos)) != std::string::npos; ++dump_count) {
    pos += std::strlen("dump reason=");
  }
  if (dump_count != 1) {
    std::fprintf(stdout, "FAIL: expected exactly one dump reason line (count=%d log=%s)\n", dump_count, out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=present_count") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason present_count at count=2 (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::PresentEx") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected trace dump to include Device::PresentEx record (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("a0=0x222") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected present record a0=0x222 (log=%s)\n", out_path.c_str());
    return 1;
  }
 
  std::remove(out_path.c_str());
  return 0;
}
