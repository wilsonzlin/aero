#include "..\\common\\aerogpu_test_common.h"

#include <d3d10.h>

using aerogpu_test::ComPtr;

static int CheckFormat(const char* test_name,
                       ID3D10Device* device,
                       DXGI_FORMAT fmt,
                       UINT required_bits,
                       const char* fmt_name) {
  UINT support = 0;
  HRESULT hr = device->CheckFormatSupport(fmt, &support);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "ID3D10Device::CheckFormatSupport", hr);
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
  const char* kTestName = "d3d10_caps_smoke";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe", kTestName);
    return 0;
  }

  ComPtr<ID3D10Device> device;
  HRESULT hr = D3D10CreateDevice(NULL,
                                 D3D10_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D10_CREATE_DEVICE_BGRA_SUPPORT,
                                 D3D10_SDK_VERSION,
                                 device.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "D3D10CreateDevice(HARDWARE)", hr);
  }

  int rc = 0;
  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8A8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8X8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8X8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R8G8B8A8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_R8G8B8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_D24_UNORM_S8_UINT,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D24_UNORM_S8_UINT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_D32_FLOAT,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R16_UINT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R16_UINT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R32_UINT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R32_UINT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32B32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32A32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32B32A32_FLOAT");
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

