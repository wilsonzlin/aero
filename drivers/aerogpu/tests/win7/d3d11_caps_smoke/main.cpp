#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static int CheckFormat(aerogpu_test::TestReporter* reporter,
                       const char* test_name,
                       ID3D11Device* device,
                       DXGI_FORMAT fmt,
                       UINT required_bits,
                       const char* fmt_name) {
  if (!reporter || !test_name || !device || !fmt_name) {
    if (reporter) {
      return reporter->Fail("CheckFormat: invalid args");
    }
    return aerogpu_test::Fail(test_name ? test_name : "d3d11_caps_smoke", "CheckFormat: invalid args");
  }
  UINT support = 0;
  HRESULT hr = device->CheckFormatSupport(fmt, &support);
  if (FAILED(hr)) {
    return reporter->FailHresult("ID3D11Device::CheckFormatSupport", hr);
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
  const char* kTestName = "d3d11_caps_smoke";
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
    return reporter.FailHresult("D3D11CreateDevice(HARDWARE)", hr);
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr) && dxgi_device) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else if (adapter) {
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
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }

    // Verify that the expected D3D11 UMD entrypoint is present.
    if (!GetModuleHandleW(L"d3d11.dll")) {
      return reporter.Fail("d3d11.dll is not loaded");
    }
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_11 = GetProcAddress(umd, "OpenAdapter11");
    if (!open_adapter_11) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_11 = GetProcAddress(umd, "_OpenAdapter11@4");
    }
    if (!open_adapter_11) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter11 (D3D11 entrypoint)");
    }
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  if (chosen_level != D3D_FEATURE_LEVEL_10_0) {
    return reporter.Fail("expected FL10_0 only (got 0x%04X)", (unsigned)chosen_level);
  }

  D3D11_FEATURE_DATA_THREADING threading;
  ZeroMemory(&threading, sizeof(threading));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_THREADING, &threading, sizeof(threading));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(THREADING)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: threading: concurrent_creates=%u command_lists=%u",
                             kTestName,
                             (unsigned)threading.DriverConcurrentCreates,
                             (unsigned)threading.DriverCommandLists);
  if (threading.DriverConcurrentCreates || threading.DriverCommandLists) {
    return reporter.Fail("unexpected threading caps: concurrent_creates=%u command_lists=%u",
                         (unsigned)threading.DriverConcurrentCreates,
                         (unsigned)threading.DriverCommandLists);
  }

  D3D11_FEATURE_DATA_DOUBLES doubles;
  ZeroMemory(&doubles, sizeof(doubles));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_DOUBLES, &doubles, sizeof(doubles));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(DOUBLES)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: doubles: fp64_shader_ops=%u",
                             kTestName,
                             (unsigned)doubles.DoublePrecisionFloatShaderOps);

  D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS hw10x;
  ZeroMemory(&hw10x, sizeof(hw10x));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D10_X_HARDWARE_OPTIONS, &hw10x, sizeof(hw10x));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(D3D10_X_HARDWARE_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout(
      "INFO: %s: d3d10_x_hw_options: cs_plus_raw_structured_via_4x=%u",
      kTestName,
      (unsigned)hw10x.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x);
  if (!hw10x.ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x) {
    return reporter.Fail("missing compute capability (expected TRUE now that CS + UAV buffers + Dispatch are implemented)");
  }

  D3D11_FEATURE_DATA_D3D11_OPTIONS options;
  ZeroMemory(&options, sizeof(options));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D11_OPTIONS, &options, sizeof(options));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(D3D11_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: d3d11_options: logic_op=%u uav_only_forced_sample_count=%u",
                             kTestName,
                             (unsigned)options.OutputMergerLogicOp,
                             (unsigned)options.UAVOnlyRenderingForcedSampleCount);
  if (options.OutputMergerLogicOp) {
    return reporter.Fail("unexpected OutputMergerLogicOp (expected FALSE)");
  }

  D3D11_FEATURE_DATA_ARCHITECTURE_INFO arch;
  ZeroMemory(&arch, sizeof(arch));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_ARCHITECTURE_INFO, &arch, sizeof(arch));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(ARCHITECTURE_INFO)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: architecture: tile_based_deferred=%u",
                             kTestName,
                             (unsigned)arch.TileBasedDeferredRenderer);

  D3D11_FEATURE_DATA_D3D9_OPTIONS d3d9;
  ZeroMemory(&d3d9, sizeof(d3d9));
  hr = device->CheckFeatureSupport(D3D11_FEATURE_D3D9_OPTIONS, &d3d9, sizeof(d3d9));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(D3D9_OPTIONS)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: d3d9_options: full_non_pow2=%u",
                             kTestName,
                             (unsigned)d3d9.FullNonPow2TextureSupport);

  D3D11_FEATURE_DATA_FORMAT_SUPPORT2 fmt2;
  ZeroMemory(&fmt2, sizeof(fmt2));
  fmt2.InFormat = DXGI_FORMAT_B8G8R8A8_UNORM;
  hr = device->CheckFeatureSupport(D3D11_FEATURE_FORMAT_SUPPORT2, &fmt2, sizeof(fmt2));
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckFeatureSupport(FORMAT_SUPPORT2)", hr);
  }
  aerogpu_test::PrintfStdout(
      "INFO: %s: format_support2(B8G8R8A8)=0x%08lX",
      kTestName,
      (unsigned long)fmt2.OutFormatSupport2);
  if (fmt2.OutFormatSupport2 != 0) {
    return reporter.Fail("unexpected FormatSupport2 bits (expected 0, got 0x%08lX)",
                         (unsigned long)fmt2.OutFormatSupport2);
  }

  UINT quality_levels = 0;
  hr = device->CheckMultisampleQualityLevels(DXGI_FORMAT_B8G8R8A8_UNORM, 1, &quality_levels);
  if (FAILED(hr)) {
    return reporter.FailHresult("CheckMultisampleQualityLevels(B8G8R8A8, 1x)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: msaa quality levels (B8G8R8A8, 1x) = %lu",
                             kTestName,
                             (unsigned long)quality_levels);
  if (quality_levels < 1) {
    return reporter.Fail("expected at least 1 quality level for 1x sample count");
  }

  // Format support checks used by the D3D11 runtime during device creation and by common apps.
  int rc = 0;
  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8A8_UNORM,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                        D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R8G8B8A8_UNORM,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                        D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_R8G8B8A8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_D24_UNORM_S8_UINT,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D24_UNORM_S8_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_D32_FLOAT,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_DEPTH_STENCIL,
                   "DXGI_FORMAT_D32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_B8G8R8X8_UNORM,
                   D3D11_FORMAT_SUPPORT_TEXTURE2D | D3D11_FORMAT_SUPPORT_RENDER_TARGET |
                       D3D11_FORMAT_SUPPORT_SHADER_SAMPLE | D3D11_FORMAT_SUPPORT_DISPLAY,
                   "DXGI_FORMAT_B8G8R8X8_UNORM");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R16_UINT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R16_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32_UINT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_INDEX_BUFFER,
                   "DXGI_FORMAT_R32_UINT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32_FLOAT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32_FLOAT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
                   "DXGI_FORMAT_R32G32B32_FLOAT");
  if (rc) return rc;

  rc = CheckFormat(&reporter,
                   kTestName,
                   device.get(),
                   DXGI_FORMAT_R32G32B32A32_FLOAT,
                   D3D11_FORMAT_SUPPORT_BUFFER | D3D11_FORMAT_SUPPORT_IA_VERTEX_BUFFER,
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
