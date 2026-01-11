#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d10.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static int CheckFormat(aerogpu_test::TestReporter* reporter,
                       const char* test_name,
                       ID3D10Device* device,
                       DXGI_FORMAT fmt,
                       UINT required_bits,
                       const char* fmt_name) {
  if (!reporter || !test_name || !device || !fmt_name) {
    if (reporter) {
      return reporter->Fail("CheckFormat: invalid args");
    }
    return aerogpu_test::Fail(test_name ? test_name : "d3d10_caps_smoke", "CheckFormat: invalid args");
  }
  UINT support = 0;
  HRESULT hr = device->CheckFormatSupport(fmt, &support);
  if (FAILED(hr)) {
    return reporter->FailHresult("ID3D10Device::CheckFormatSupport", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: format %s support=0x%08lX", test_name, fmt_name, (unsigned long)support);
  if ((support & required_bits) != required_bits) {
    return reporter->Fail("format %s missing required bits: have=0x%08lX need=0x%08lX",
                          fmt_name,
                          (unsigned long)support,
                          (unsigned long)required_bits);
  }
  return 0;
}

static int RunCapsSmoke(int argc, char** argv) {
  const char* kTestName = "d3d10_caps_smoke";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return reporter.Fail("invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return reporter.Fail("invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  ComPtr<ID3D10Device> device;
  HRESULT hr = D3D10CreateDevice(NULL,
                                 D3D10_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                  D3D10_CREATE_DEVICE_BGRA_SUPPORT,
                                  D3D10_SDK_VERSION,
                                  device.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D10CreateDevice(HARDWARE)", hr);
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return reporter.FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
        }
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                                   kTestName,
                                   ad.Description,
                                   (unsigned)ad.VendorId,
                                   (unsigned)ad.DeviceId);
        reporter.SetAdapterInfoW(ad.Description, ad.VendorId, ad.DeviceId);
        if (!allow_microsoft && ad.VendorId == 0x1414) {
          return reporter.Fail(
              "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
              (unsigned)ad.VendorId,
              (unsigned)ad.DeviceId);
        }
        if (has_require_vid && ad.VendorId != require_vid) {
          return reporter.Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                               (unsigned)ad.VendorId,
                               (unsigned)require_vid);
        }
        if (has_require_did && ad.DeviceId != require_did) {
          return reporter.Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                               (unsigned)ad.DeviceId,
                               (unsigned)require_did);
        }
        if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
            !(ad.VendorId == 0x1414 && allow_microsoft) &&
            !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
          return reporter.Fail(
              "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu or use --require-vid/--require-did)",
              ad.Description);
        }
      }
    }
  } else if (has_require_vid || has_require_did) {
    return reporter.FailHresult("QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }

    // The D3D10 caps path should still go through OpenAdapter10.
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_10 = GetProcAddress(umd, "OpenAdapter10");
    if (!open_adapter_10) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_10 = GetProcAddress(umd, "_OpenAdapter10@4");
    }
    if (!open_adapter_10) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter10 (D3D10 entrypoint)");
    }
  }

  int rc = 0;
  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8A8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                        D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8X8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                        D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8X8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R8G8B8A8_UNORM,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_RENDER_TARGET |
                        D3D10_FORMAT_SUPPORT_SHADER_SAMPLE | D3D10_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_R8G8B8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_D24_UNORM_S8_UINT,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D24_UNORM_S8_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_D32_FLOAT,
                   D3D10_FORMAT_SUPPORT_TEXTURE2D | D3D10_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R16_UINT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R16_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32_UINT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R32_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32B32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32A32_FLOAT,
                   D3D10_FORMAT_SUPPORT_BUFFER | D3D10_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32B32A32_FLOAT");
  if (rc) return rc;

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunCapsSmoke(argc, argv);
  Sleep(30);
  return rc;
}
