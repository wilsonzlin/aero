#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_trace.h"
#include "aerogpu_d3d9_test_entrypoints.h"
#include "trace_test_utils.h"

namespace aerogpu {

namespace {

int failf(const char* fmt, const std::string& log_path, const std::string& output) {
  std::fprintf(stdout, "FAIL: ");
  std::fprintf(stdout, fmt, log_path.c_str());
  std::fprintf(stdout, "\n");
  // Include output to make CI failures actionable.
  std::fprintf(stdout, "---- trace output ----\n%s\n----------------------\n", output.c_str());
  return 1;
}

} // namespace
} // namespace aerogpu

int main() {
  using namespace aerogpu_d3d9_trace_test;

  const std::string out_path =
      make_unique_log_path("aerogpu_d3d9_trace_process_vertices_single_record_tests");
  if (!std::freopen(out_path.c_str(), "w", stderr)) {
    return fail("freopen(stderr) failed");
  }

  set_env("AEROGPU_D3D9_TRACE", "1");
  set_env("AEROGPU_D3D9_TRACE_MODE", "all");
  set_env("AEROGPU_D3D9_TRACE_MAX", "64");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH", "1");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_ON_STUB", "0");
  set_env("AEROGPU_D3D9_TRACE_DUMP_PRESENT", "0");
  // Restrict to ProcessVertices so unrelated trace noise can't make this flaky.
  set_env("AEROGPU_D3D9_TRACE_FILTER", "ProcessVertices");
  // On Windows, the trace defaults to OutputDebugStringA; enable stderr echo so
  // we can capture output portably.
  set_env("AEROGPU_D3D9_TRACE_STDERR", "1");

  aerogpu::d3d9_trace_init_from_env();

  // Call the real DDI entrypoint once. Historically, ProcessVertices was traced
  // twice per call (the public entrypoint and an internal helper both emitted
  // `D3d9TraceCall`), which caused TRACE_MODE=all to dump two entries.
  {
    aerogpu::Adapter adapter;
    aerogpu::Device dev(&adapter);

    aerogpu::Resource src;
    src.kind = aerogpu::ResourceKind::Buffer;
    src.size_bytes = 16;
    src.storage.resize(src.size_bytes);
    std::memset(src.storage.data(), 0xAA, src.storage.size());

    aerogpu::Resource dst;
    dst.kind = aerogpu::ResourceKind::Buffer;
    dst.size_bytes = 16;
    dst.storage.resize(dst.size_bytes);
    std::memset(dst.storage.data(), 0xCD, dst.storage.size());

    D3DDDI_HDEVICE hDevice{};
    hDevice.pDrvPrivate = &dev;

    D3DDDI_HRESOURCE hSrc{};
    hSrc.pDrvPrivate = &src;
    const HRESULT ss_hr = aerogpu::device_set_stream_source(
        hDevice, /*stream=*/0, hSrc, /*offset_bytes=*/0, /*stride_bytes=*/16);
    if (FAILED(ss_hr)) {
      const std::string output = slurp_file_after_closing_stderr(out_path);
      std::remove(out_path.c_str());
      return aerogpu::failf("expected SetStreamSource to succeed (log=%s)", out_path, output);
    }

    D3DDDIARG_PROCESSVERTICES pv{};
    pv.SrcStartIndex = 0;
    pv.DestIndex = 0;
    pv.VertexCount = 1;
    pv.hDestBuffer.pDrvPrivate = &dst;
    pv.Flags = 0;
    // Ensure the call succeeds without requiring any vertex decl setup.
    pv.DestStride = 0;

    const HRESULT hr = aerogpu::device_process_vertices(hDevice, &pv);
    if (FAILED(hr)) {
      const std::string output = slurp_file_after_closing_stderr(out_path);
      std::remove(out_path.c_str());
      return aerogpu::failf("expected ProcessVertices to succeed (log=%s)", out_path, output);
    }
  }

  aerogpu::d3d9_trace_on_process_detach();

  const std::string output = slurp_file_after_closing_stderr(out_path);
  if (output.find("dump reason=DLL_PROCESS_DETACH") == std::string::npos) {
    std::remove(out_path.c_str());
    return aerogpu::failf("expected dump reason DLL_PROCESS_DETACH (log=%s)", out_path, output);
  }
  if (output.find("mode=all") == std::string::npos) {
    std::remove(out_path.c_str());
    return aerogpu::failf("expected mode=all (log=%s)", out_path, output);
  }
  if (output.find("entries=1") == std::string::npos) {
    std::remove(out_path.c_str());
    return aerogpu::failf("expected entries=1 (no double-tracing) (log=%s)", out_path, output);
  }
  if (output.find("Device::ProcessVertices") == std::string::npos) {
    std::remove(out_path.c_str());
    return aerogpu::failf("expected Device::ProcessVertices entry (log=%s)", out_path, output);
  }

  // Ensure ProcessVertices appears only once in the dump (no duplicate trace
  // record emitted by internal helpers).
  const size_t first = output.find("Device::ProcessVertices");
  const size_t second =
      (first == std::string::npos) ? std::string::npos : output.find("Device::ProcessVertices", first + 1);
  if (second != std::string::npos) {
    std::remove(out_path.c_str());
    return aerogpu::failf("did not expect a second Device::ProcessVertices entry (log=%s)", out_path, output);
  }

  std::remove(out_path.c_str());
  return 0;
}
