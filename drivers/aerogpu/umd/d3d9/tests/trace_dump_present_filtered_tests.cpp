#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;

  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_present_filtered_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "2");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  // Filter out Present/PresentEx, but allow ValidateDevice. This ensures that
  // dump-on-present still fires while verifying the force-record path does not
  // bypass the filter.
  set_env("AEROGPU_D3D9_TRACE_FILTER", "ValidateDevice");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // Filtered in: should appear in the dump.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceValidateDevice, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }

  // Filtered out: should not appear in the dump even though it triggers the dump.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0xaaa, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(1);
  }

  // Trigger the dump. PresentEx is still filtered out.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DevicePresentEx, 0xbbb, 0, 0, 0);
    trace.ret(S_OK);
    trace.maybe_dump_on_present(2);
  }

  std::fflush(stderr);

  const std::string output = slurp_file(out_path);
  if (output.find("dump reason=present_count") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump reason present_count (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::ValidateDevice") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected dump to include Device::ValidateDevice (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("Device::PresentEx") != std::string::npos) {
    std::fprintf(stdout, "FAIL: did not expect filtered-out PresentEx to appear in dump (log=%s)\n", out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}

