#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>

using aerogpu_test::ComPtr;

static int CheckFormat(const char* test_name,
                       ID3D11Device* device,
                       DXGI_FORMAT fmt,
                       UINT required_bits,
                       const char* fmt_name) {
  UINT support = 0;
  HRESULT hr = device->CheckFormatSupport(fmt, &support);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "ID3D11Device::CheckFormatSupport", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: format %s support=0x%08lX", test_name, fmt_name, (unsigned long)support);
  if ((support & required_bits) != required_bits) {
    return aerogpu_test::Fail(test_name,
                              "format %s missing required bits: have=0x%08lX need=0x%08lX",
                              fmt_name,
                              (unsigned long)support,
                              (unsigned long)required_bits);
  }
  return 0;
}

static int RunCapsSmoke(int argc, char** argv) {
  const char* kTestName = "d3d11_caps_smoke";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe", kTestName);
    return 0;
  }

  // Request higher feature levels first; the smoke test validates that the
  // driver advertises only FL10_0 today.
  D3D_FEATURE_LEVEL requested_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                          D3D_FEATURE_LEVEL_10_1,
                                          D3D_FEATURE_LEVEL_10_0};
  D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;

  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;

  HRESULT hr = D3D11CreateDevice(NULL,
                                 D3D_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 requested_levels,
                                 ARRAYSIZE(requested_levels),
                                 D3D11_SDK_VERSION,
                                 device.put(),
                                 &chosen_level,
                                 context.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "D3D11CreateDevice(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  if (chosen_level != D3D_FEATURE_LEVEL_10_0) {
    return aerogpu_test::Fail(kTestName, "expected FL10_0 only (got 0x%04X)", (unsigned)chosen_level);
  }

  D3D11_FEATURE_DATA_THREADING threading;
  ZeroMemory(&threading, sizeof(threading));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_THREADING, &threading, sizeof(threading));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CheckFeatureSupport(THREADING)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: threading: concurrent_creates=%u command_lists=%u",
                             kTestName,
                             (unsigned)threading.DriverConcurrentCreates,
                             (unsigned)threading.DriverCommandLists);

  D3D11_FEATURE_DATA_DOUBLES doubles;
  ZeroMemory(&doubles, sizeof(doubles));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_DOUBLES, &doubles, sizeof(doubles));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CheckFeatureSupport(DOUBLES)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: doubles: fp64_shader_ops=%u",
                             kTestName,
                             (unsigned)doubles.DoublePrecisionFloatShaderOps);

  D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS hw10x;
  ZeroMemory(&hw10x, sizeof(hw10x));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, &hw10x, sizeof(hw10x));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CheckFeatureSupport(D3D10_X_HARDWARE_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout(
      "INFO: %s: d3d10_x_hw_options: cs_plus_raw_structured_via_4x=%u",
      kTestName,
      (unsigned)hw10x.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x);
  if (hw10x.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x) {
    return aerogpu_test::Fail(kTestName,
                              "unexpected compute capability (expected FALSE until implemented)");
  }

  D3D11_FEATURE_DATA_D3D11_OPTIONS options;
  ZeroMemory(&options, sizeof(options));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS, &options, sizeof(options));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CheckFeatureSupport(D3D11_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: d3d11_options: logic_op=%u uav_only_forced_sample_count=%u",
                             kTestName,
                             (unsigned)options.OutputMergerLogicOp,
                             (unsigned)options.UAVOnlyRenderingForcedSampleCount);
  if (options.OutputMergerLogicOp) {
    return aerogpu_test::Fail(kTestName, "unexpected OutputMergerLogicOp (expected FALSE)");
  }

  // Format support checks used by the D3D11 runtime during device creation and by common apps.
  int rc = 0;
  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8A8_UNORM,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D11_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R8G8B8A8_UNORM,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D11_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_R8G8B8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_D24_UNORM_S8_UINT,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D24_UNORM_S8_UINT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_D32_FLOAT,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R16_UINT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R16_UINT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R32_UINT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R32_UINT");
  if (rc) return rc;

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunCapsSmoke(argc, argv);
  Sleep(30);
  return rc;
}
