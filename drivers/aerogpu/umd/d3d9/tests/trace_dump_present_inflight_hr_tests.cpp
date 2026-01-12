#include "aerogpu_trace.h"
#include "trace_test_utils.h"

#include <cstring>

int main() {
  using namespace aerogpu_d3d9_trace_test;

  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_present_inflight_hr_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  // Trigger dump on the first present count.
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  // Also enable dump-on-fail; the present-count dump should win (dump is one-shot).
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // Simulate dump-on-present firing while the present call is still on the
  // stack (before the trace scope ends). The dump should still report the
  // correct HRESULT for the in-flight call.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0x111, 0, 0, 0);
    trace.ret(E_INVALIDARG);
    trace.maybe_dump_on_present(1);
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
  if (output.find("dump reason=present_count") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason present_count (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=Device::PresentEx") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect dump-on-fail to emit an additional dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::PresentEx") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected Device::PresentEx in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("hr=0x80070057") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected hr=0x80070057 (E_INVALIDARG) in dump (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("hr=0x7fffffff") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect pending hr=0x7fffffff in dump (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}
