#include "aerogpu_trace.h"
#include "trace_test_utils.h"

int main() {
  using namespace aerogpu_d3d9_trace_test;
  const std::string out_path = make_unique_log_path("aerogpu_d3d9_trace_dump_on_stub_noop_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "unique");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  set_env("AEROGPU_D3D9_TRACE_FILTER", nullptr);
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // `Device::SetCursorProperties` is a real D3D9 UMD entrypoint. It should NOT
  // be tagged as "(stub)" in trace output, so it should not trigger
  // AEROGPU_D3D9_TRACE_DUMP_ON_STUB.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetCursorProperties, 0xabc, 0, 0, 0);
    trace.ret(S_OK);
  }

  // Exercise other non-stub DDIs as well (some are bring-up no-ops, others are
  // real implementations); none should be stub-tagged.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetCursorPosition, 0xdef, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceShowCursor, 0x123, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetDialogBoxMode, 0x456, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceSetConvolutionMonoKernel, 0x789, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceGenerateMipSubLevels, 0xabc, 0, 0, 0);
    trace.ret(S_OK);
  }

  // Trace IDs for real D3D9 UMD entrypoints should not carry the "(stub)"
  // marker, so they do not trigger AEROGPU_D3D9_TRACE_DUMP_ON_STUB. Stub-tag
  // behavior is exercised via the trace-only TraceTestStub entrypoint.
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceDrawRectPatch, 0x111, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceDrawTriPatch, 0x222, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceDeletePatch, 0x333, 0, 0, 0);
    trace.ret(S_OK);
  }
  {
    aerogpu::D3d9TraceCall trace(aerogpu::D3d9TraceFunc::DeviceProcessVertices, 0x444, 0, 0, 0);
    trace.ret(S_OK);
  }

  const std::string output = slurp_file_after_closing_stderr(out_path);
  if (output.find("aerogpu-d3d9-trace: enabled") == std::string::npos) {
    std::fprintf(stdout, "FAIL: expected trace init line (log=%s)\n", out_path.c_str());
    return 1;
  }
  if (output.find("dump reason=") != std::string::npos) {
    std::fprintf(stdout,
                 "FAIL: did not expect dump-on-stub to trigger for a non-stub DDI (log=%s)\n",
                 out_path.c_str());
    return 1;
  }

  std::remove(out_path.c_str());
  return 0;
}
