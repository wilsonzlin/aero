#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d10.h>
#include <d3d10_1.h>
#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

enum ProbeApi {
  kProbeApiD3D11 = 0,
  kProbeApiD3D10,
  kProbeApiD3D10_1,
};

static const char* ProbeApiName(ProbeApi api) {
  switch (api) {
    case kProbeApiD3D11:
      return "d3d11";
    case kProbeApiD3D10:
      return "d3d10";
    case kProbeApiD3D10_1:
      return "d3d10_1";
    default:
      return "unknown";
  }
}

static bool ParseProbeApi(const std::string& s, ProbeApi* out_api) {
  if (!out_api) {
    return false;
  }
  if (s.empty()) {
    return false;
  }
  if (lstrcmpiA(s.c_str(), "d3d11") == 0) {
    *out_api = kProbeApiD3D11;
    return true;
  }
  if (lstrcmpiA(s.c_str(), "d3d10") == 0) {
    *out_api = kProbeApiD3D10;
    return true;
  }
  if (lstrcmpiA(s.c_str(), "d3d10_1") == 0 || lstrcmpiA(s.c_str(), "d3d10.1") == 0 ||
      lstrcmpiA(s.c_str(), "d3d10-1") == 0) {
    *out_api = kProbeApiD3D10_1;
    return true;
  }
  return false;
}

static const char* SwapEffectName(DXGI_SWAP_EFFECT e) {
  switch (e) {
    case DXGI_SWAP_EFFECT_DISCARD:
      return "discard";
    case DXGI_SWAP_EFFECT_SEQUENTIAL:
      return "sequential";
    default:
      return "unknown";
  }
}

static bool ParseSwapEffect(const std::string& s, DXGI_SWAP_EFFECT* out) {
  if (!out) {
    return false;
  }
  if (s.empty()) {
    return false;
  }
  if (lstrcmpiA(s.c_str(), "discard") == 0) {
    *out = DXGI_SWAP_EFFECT_DISCARD;
    return true;
  }
  if (lstrcmpiA(s.c_str(), "sequential") == 0) {
    *out = DXGI_SWAP_EFFECT_SEQUENTIAL;
    return true;
  }
  return false;
}

static const char* FormatName(DXGI_FORMAT fmt) {
  switch (fmt) {
    case DXGI_FORMAT_B8G8R8A8_UNORM:
      return "b8g8r8a8_unorm";
    case DXGI_FORMAT_B8G8R8X8_UNORM:
      return "b8g8r8x8_unorm";
    case DXGI_FORMAT_R8G8B8A8_UNORM:
      return "r8g8b8a8_unorm";
    default:
      return "unknown";
  }
}

static bool ParseFormat(const std::string& s, DXGI_FORMAT* out_fmt) {
  if (!out_fmt) {
    return false;
  }
  if (s.empty()) {
    return false;
  }
  if (lstrcmpiA(s.c_str(), "b8g8r8a8") == 0 || lstrcmpiA(s.c_str(), "b8g8r8a8_unorm") == 0) {
    *out_fmt = DXGI_FORMAT_B8G8R8A8_UNORM;
    return true;
  }
  if (lstrcmpiA(s.c_str(), "b8g8r8x8") == 0 || lstrcmpiA(s.c_str(), "b8g8r8x8_unorm") == 0) {
    *out_fmt = DXGI_FORMAT_B8G8R8X8_UNORM;
    return true;
  }
  if (lstrcmpiA(s.c_str(), "r8g8b8a8") == 0 || lstrcmpiA(s.c_str(), "r8g8b8a8_unorm") == 0) {
    *out_fmt = DXGI_FORMAT_R8G8B8A8_UNORM;
    return true;
  }
  return false;
}

static int FailD3D11WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                       const char* test_name,
                                       const char* what,
                                       HRESULT hr,
                                       ID3D11Device* device) {
  if (device) {
    HRESULT reason = device->GetDeviceRemovedReason();
    if (FAILED(reason)) {
      aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                                 test_name,
                                 aerogpu_test::HresultToString(reason).c_str());
    }
  }
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static int FailD3D10WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                      const char* test_name,
                                      const char* what,
                                      HRESULT hr,
                                      ID3D10Device* device) {
  if (device) {
    HRESULT reason = device->GetDeviceRemovedReason();
    if (FAILED(reason)) {
      aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                                 test_name,
                                 aerogpu_test::HresultToString(reason).c_str());
    }
  }
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static const char* D3D11UsageName(D3D11_USAGE u) {
  switch (u) {
    case D3D11_USAGE_DEFAULT:
      return "DEFAULT";
    case D3D11_USAGE_IMMUTABLE:
      return "IMMUTABLE";
    case D3D11_USAGE_DYNAMIC:
      return "DYNAMIC";
    case D3D11_USAGE_STAGING:
      return "STAGING";
    default:
      return "UNKNOWN";
  }
}

static const char* D3D10UsageName(D3D10_USAGE u) {
  switch (u) {
    case D3D10_USAGE_DEFAULT:
      return "DEFAULT";
    case D3D10_USAGE_IMMUTABLE:
      return "IMMUTABLE";
    case D3D10_USAGE_DYNAMIC:
      return "DYNAMIC";
    case D3D10_USAGE_STAGING:
      return "STAGING";
    default:
      return "UNKNOWN";
  }
}

static void PrintTexDesc(const char* test_name, const char* label, const D3D11_TEXTURE2D_DESC& d) {
  aerogpu_test::PrintfStdout(
      "INFO: %s: %s: %ux%u fmt=%u mips=%u array=%u sample=(%u,%u) usage=%s(%u) bind=0x%08X cpu=0x%08X misc=0x%08X",
      test_name,
      label,
      (unsigned)d.Width,
      (unsigned)d.Height,
      (unsigned)d.Format,
      (unsigned)d.MipLevels,
      (unsigned)d.ArraySize,
      (unsigned)d.SampleDesc.Count,
      (unsigned)d.SampleDesc.Quality,
      D3D11UsageName(d.Usage),
      (unsigned)d.Usage,
      (unsigned)d.BindFlags,
      (unsigned)d.CPUAccessFlags,
      (unsigned)d.MiscFlags);
}

static void PrintTexDesc(const char* test_name, const char* label, const D3D10_TEXTURE2D_DESC& d) {
  aerogpu_test::PrintfStdout(
      "INFO: %s: %s: %ux%u fmt=%u mips=%u array=%u sample=(%u,%u) usage=%s(%u) bind=0x%08X cpu=0x%08X misc=0x%08X",
      test_name,
      label,
      (unsigned)d.Width,
      (unsigned)d.Height,
      (unsigned)d.Format,
      (unsigned)d.MipLevels,
      (unsigned)d.ArraySize,
      (unsigned)d.SampleDesc.Count,
      (unsigned)d.SampleDesc.Quality,
      D3D10UsageName(d.Usage),
      (unsigned)d.Usage,
      (unsigned)d.BindFlags,
      (unsigned)d.CPUAccessFlags,
      (unsigned)d.MiscFlags);
}

static void DumpSharedHandleInfo(const char* test_name, const char* label, IUnknown* tex) {
  if (!tex) {
    return;
  }

  ComPtr<IDXGIResource> res;
  HRESULT hr = tex->QueryInterface(__uuidof(IDXGIResource), (void**)res.put());
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: %s: QueryInterface(IDXGIResource) failed: %s",
                               test_name,
                               label,
                               aerogpu_test::HresultToString(hr).c_str());
    return;
  }

  HANDLE h = NULL;
  hr = res->GetSharedHandle(&h);
  aerogpu_test::PrintfStdout("INFO: %s: %s: IDXGIResource::GetSharedHandle -> %s handle=%p",
                              test_name,
                              label,
                             aerogpu_test::HresultToString(hr).c_str(),
                              h);
}

static int CheckAdapterPolicy(const char* test_name,
                              aerogpu_test::TestReporter* reporter,
                              IUnknown* device,
                              bool allow_microsoft,
                              bool allow_non_aerogpu,
                              bool has_require_vid,
                              uint32_t require_vid,
                              bool has_require_did,
                              uint32_t require_did) {
  if (!device) {
    return 0;
  }

  // Optional adapter sanity checks (same policy as other tests in this suite).
  ComPtr<IDXGIDevice> dxgi_device;
  HRESULT hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        if (reporter) {
          return reporter->FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)",
                                       hr_adapter);
        }
        return aerogpu_test::FailHresult(
            test_name, "IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          if (reporter) {
            return reporter->FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
          }
          return aerogpu_test::FailHresult(
              test_name, "IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
        }
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                                   test_name,
                                   ad.Description,
                                   (unsigned)ad.VendorId,
                                   (unsigned)ad.DeviceId);
        if (reporter) {
          reporter->SetAdapterInfoW(ad.Description, ad.VendorId, ad.DeviceId);
        }
        if (!allow_microsoft && ad.VendorId == 0x1414) {
          if (reporter) {
            return reporter->Fail(
                "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass "
                "--allow-microsoft.",
                (unsigned)ad.VendorId,
                (unsigned)ad.DeviceId);
          }
          return aerogpu_test::Fail(test_name,
                                    "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                    "Install AeroGPU driver or pass --allow-microsoft.",
                                    (unsigned)ad.VendorId,
                                    (unsigned)ad.DeviceId);
        }
        if (has_require_vid && ad.VendorId != require_vid) {
          if (reporter) {
            return reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                                  (unsigned)ad.VendorId,
                                  (unsigned)require_vid);
          }
          return aerogpu_test::Fail(test_name,
                                    "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.VendorId,
                                    (unsigned)require_vid);
        }
        if (has_require_did && ad.DeviceId != require_did) {
          if (reporter) {
            return reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                                  (unsigned)ad.DeviceId,
                                  (unsigned)require_did);
          }
          return aerogpu_test::Fail(test_name,
                                    "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.DeviceId,
                                    (unsigned)require_did);
        }
        if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
            !(ad.VendorId == 0x1414 && allow_microsoft) &&
            !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
          if (reporter) {
            return reporter->Fail(
                "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu or use --require-vid/--require-did)",
                ad.Description);
          }
          return aerogpu_test::Fail(test_name,
                                    "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu "
                                    "or use --require-vid/--require-did)",
                                    ad.Description);
        }
      }
    }
  } else if (has_require_vid || has_require_did) {
    if (reporter) {
      return reporter->FailHresult("QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
    }
    return aerogpu_test::FailHresult(
        test_name, "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  return 0;
}

static int RunDxgiSwapchainProbe(int argc, char** argv) {
  const char* kTestName = "dxgi_swapchain_probe";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--api=d3d11|d3d10|d3d10_1] [--width=N] [--height=N] [--buffers=1|2] "
        "[--swap-effect=discard|sequential] [--format=b8g8r8a8_unorm|r8g8b8a8_unorm|87] "
        "[--buffer-usage=0x####] [--swapchain-flags=0x####] [--hidden] [--frames=N] [--json[=PATH]] "
        "[--require-vid=0x####] "
        "[--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  ProbeApi api = kProbeApiD3D11;
  std::string api_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--api", &api_str)) {
    if (api_str.empty()) {
      return reporter.Fail("--api requires a value (d3d11|d3d10|d3d10_1)");
    }
    if (!ParseProbeApi(api_str, &api)) {
      return reporter.Fail("invalid --api value: %s (expected d3d11|d3d10|d3d10_1)", api_str.c_str());
    }
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");

  uint32_t frames = 2;
  aerogpu_test::GetArgUint32(argc, argv, "--frames", &frames);
  if (frames == 0) {
    frames = 1;
  }
  if (frames > 120) {
    frames = 120;
  }

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

  uint32_t width = 256;
  uint32_t height = 256;
  aerogpu_test::GetArgUint32(argc, argv, "--width", &width);
  aerogpu_test::GetArgUint32(argc, argv, "--height", &height);
  if (width == 0) {
    width = 1;
  }
  if (height == 0) {
    height = 1;
  }

  uint32_t buffers = 2;
  aerogpu_test::GetArgUint32(argc, argv, "--buffers", &buffers);
  if (buffers < 1 || buffers > 2) {
    return reporter.Fail("invalid --buffers value: %u (expected 1 or 2)", (unsigned)buffers);
  }

  DXGI_SWAP_EFFECT swap_effect = DXGI_SWAP_EFFECT_DISCARD;
  std::string swap_effect_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--swap-effect", &swap_effect_str)) {
    if (swap_effect_str.empty()) {
      return reporter.Fail("--swap-effect requires a value (discard|sequential)");
    }
    if (!ParseSwapEffect(swap_effect_str, &swap_effect)) {
      return reporter.Fail("invalid --swap-effect value: %s (expected discard|sequential)",
                           swap_effect_str.c_str());
    }
  }

  DXGI_FORMAT format = DXGI_FORMAT_B8G8R8A8_UNORM;
  std::string format_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--format", &format_str)) {
    if (format_str.empty()) {
      return reporter.Fail("--format requires a value (e.g. b8g8r8a8_unorm or 87)");
    }
    DXGI_FORMAT parsed = DXGI_FORMAT_UNKNOWN;
    if (ParseFormat(format_str, &parsed)) {
      format = parsed;
    } else {
      uint32_t v = 0;
      std::string err;
      if (!aerogpu_test::ParseUint32(format_str, &v, &err)) {
        return reporter.Fail("invalid --format: %s", err.c_str());
      }
      format = (DXGI_FORMAT)v;
    }
  }

  uint32_t buffer_usage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
  std::string buffer_usage_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--buffer-usage", &buffer_usage_str)) {
    uint32_t v = 0;
    std::string err;
    if (!aerogpu_test::ParseUint32(buffer_usage_str, &v, &err)) {
      return reporter.Fail("invalid --buffer-usage: %s", err.c_str());
    }
    buffer_usage = v;
  }

  uint32_t swapchain_flags = 0;
  std::string swapchain_flags_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--swapchain-flags", &swapchain_flags_str)) {
    uint32_t v = 0;
    std::string err;
    if (!aerogpu_test::ParseUint32(swapchain_flags_str, &v, &err)) {
      return reporter.Fail("invalid --swapchain-flags: %s", err.c_str());
    }
    swapchain_flags = v;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_DXGISwapchainProbe",
                                              L"AeroGPU DXGI Swapchain Probe",
                                              (int)width,
                                              (int)height,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  DXGI_SWAP_CHAIN_DESC scd;
  ZeroMemory(&scd, sizeof(scd));
  scd.BufferDesc.Width = width;
  scd.BufferDesc.Height = height;
  scd.BufferDesc.Format = format;
  scd.BufferDesc.RefreshRate.Numerator = 60;
  scd.BufferDesc.RefreshRate.Denominator = 1;
  scd.SampleDesc.Count = 1;
  scd.SampleDesc.Quality = 0;
  scd.BufferUsage = buffer_usage;
  scd.BufferCount = buffers;
  scd.OutputWindow = hwnd;
  scd.Windowed = TRUE;
  scd.SwapEffect = swap_effect;
  scd.Flags = swapchain_flags;

  aerogpu_test::PrintfStdout("INFO: %s: api=%s size=%ux%u buffers=%u swap_effect=%s fmt=%s(%u) usage=0x%08X flags=0x%08X",
                             kTestName,
                             ProbeApiName(api),
                             (unsigned)width,
                             (unsigned)height,
                             (unsigned)buffers,
                             SwapEffectName(swap_effect),
                             FormatName(format),
                             (unsigned)format,
                             (unsigned)buffer_usage,
                             (unsigned)swapchain_flags);

  if (api == kProbeApiD3D10) {
    ComPtr<ID3D10Device> device;
    ComPtr<IDXGISwapChain> swapchain;
    const UINT flags = D3D10_CREATE_DEVICE_BGRA_SUPPORT;
    HRESULT hr = D3D10CreateDeviceAndSwapChain(NULL,
                                               D3D10_DRIVER_TYPE_HARDWARE,
                                               NULL,
                                               flags,
                                               D3D10_SDK_VERSION,
                                               &scd,
                                               swapchain.put(),
                                               device.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("D3D10CreateDeviceAndSwapChain(HARDWARE)", hr);
    }

    // Sanity check: this mode should load the D3D10 runtime path (d3d10.dll).
    if (!GetModuleHandleW(L"d3d10.dll")) {
      return reporter.Fail("d3d10.dll is not loaded");
    }

    int adapter_rc = CheckAdapterPolicy(kTestName,
                                        &reporter,
                                        device.get(),
                                        allow_microsoft,
                                        allow_non_aerogpu,
                                        has_require_vid,
                                        require_vid,
                                        has_require_did,
                                        require_did);
    if (adapter_rc != 0) {
      return adapter_rc;
    }

    if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
      int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
      if (umd_rc != 0) {
        return umd_rc;
      }
    }

    ComPtr<ID3D10Texture2D> backbuffer0;
    ComPtr<ID3D10Texture2D> backbuffer1;
    hr = swapchain->GetBuffer(0, __uuidof(ID3D10Texture2D), (void**)backbuffer0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("IDXGISwapChain::GetBuffer(0)", hr);
    }
    if (buffers > 1) {
      hr = swapchain->GetBuffer(1, __uuidof(ID3D10Texture2D), (void**)backbuffer1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("IDXGISwapChain::GetBuffer(1)", hr);
      }
    }

    D3D10_TEXTURE2D_DESC bb0_desc;
    backbuffer0->GetDesc(&bb0_desc);
    PrintTexDesc(kTestName, "backbuffer[0]", bb0_desc);
    DumpSharedHandleInfo(kTestName, "backbuffer[0]", backbuffer0.get());
    if (buffers > 1) {
      D3D10_TEXTURE2D_DESC bb1_desc;
      backbuffer1->GetDesc(&bb1_desc);
      PrintTexDesc(kTestName, "backbuffer[1]", bb1_desc);
      DumpSharedHandleInfo(kTestName, "backbuffer[1]", backbuffer1.get());
    }

    ComPtr<ID3D10RenderTargetView> rtv0;
    ComPtr<ID3D10RenderTargetView> rtv1;
    hr = device->CreateRenderTargetView(backbuffer0.get(), NULL, rtv0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRenderTargetView(backbuffer[0])", hr);
    }
    if (buffers > 1) {
      hr = device->CreateRenderTargetView(backbuffer1.get(), NULL, rtv1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("CreateRenderTargetView(backbuffer[1])", hr);
      }
    }

    D3D10_VIEWPORT vp;
    vp.TopLeftX = 0;
    vp.TopLeftY = 0;
    vp.Width = (UINT)width;
    vp.Height = (UINT)height;
    vp.MinDepth = 0.0f;
    vp.MaxDepth = 1.0f;
    device->RSSetViewports(1, &vp);

    for (uint32_t frame = 0; frame < frames; ++frame) {
      ID3D10RenderTargetView* rtv = (buffers > 1 && (frame & 1)) ? rtv1.get() : rtv0.get();
      ID3D10RenderTargetView* rtvs[] = {rtv};
      device->OMSetRenderTargets(1, rtvs, NULL);

      const FLOAT clear_rgba[4] = {(frame & 1) ? 0.0f : 1.0f, (frame & 1) ? 1.0f : 0.0f, 0.0f, 1.0f};
      device->ClearRenderTargetView(rtv, clear_rgba);

      hr = swapchain->Present(1, 0);
      if (FAILED(hr)) {
        return FailD3D10WithRemovedReason(&reporter, kTestName, "IDXGISwapChain::Present(1,0)", hr, device.get());
      }
    }
  } else if (api == kProbeApiD3D10_1) {
    ComPtr<ID3D10Device1> device;
    ComPtr<IDXGISwapChain> swapchain;

    // Ensure BGRA swap chains (DXGI_FORMAT_B8G8R8A8_UNORM) can be used as render targets.
    const UINT flags = D3D10_CREATE_DEVICE_BGRA_SUPPORT;
    D3D10_FEATURE_LEVEL1 feature_levels[] = {D3D10_FEATURE_LEVEL1_10_1, D3D10_FEATURE_LEVEL1_10_0};

    D3D10_FEATURE_LEVEL1 chosen_level = (D3D10_FEATURE_LEVEL1)0;
    HRESULT hr = E_FAIL;
    for (size_t i = 0; i < ARRAYSIZE(feature_levels); ++i) {
      chosen_level = feature_levels[i];
      hr = D3D10CreateDeviceAndSwapChain1(NULL,
                                          D3D10_DRIVER_TYPE_HARDWARE,
                                          NULL,
                                          flags,
                                          chosen_level,
                                          D3D10_SDK_VERSION,
                                          &scd,
                                          swapchain.put(),
                                          device.put());
      if (SUCCEEDED(hr)) {
        break;
      }
    }
    if (FAILED(hr)) {
      return reporter.FailHresult("D3D10CreateDeviceAndSwapChain1(HARDWARE)", hr);
    }

    // Sanity check: this mode should load the D3D10.1 runtime path (d3d10_1.dll).
    if (!GetModuleHandleW(L"d3d10_1.dll")) {
      return reporter.Fail("d3d10_1.dll is not loaded");
    }

    aerogpu_test::PrintfStdout("INFO: %s: d3d10_1 feature level 0x%04X", kTestName, (unsigned)chosen_level);

    int adapter_rc = CheckAdapterPolicy(kTestName,
                                        &reporter,
                                        device.get(),
                                        allow_microsoft,
                                        allow_non_aerogpu,
                                        has_require_vid,
                                        require_vid,
                                        has_require_did,
                                        require_did);
    if (adapter_rc != 0) {
      return adapter_rc;
    }

    if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
      int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
      if (umd_rc != 0) {
        return umd_rc;
      }
    }

    ComPtr<ID3D10Texture2D> backbuffer0;
    ComPtr<ID3D10Texture2D> backbuffer1;
    hr = swapchain->GetBuffer(0, __uuidof(ID3D10Texture2D), (void**)backbuffer0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("IDXGISwapChain::GetBuffer(0)", hr);
    }
    if (buffers > 1) {
      hr = swapchain->GetBuffer(1, __uuidof(ID3D10Texture2D), (void**)backbuffer1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("IDXGISwapChain::GetBuffer(1)", hr);
      }
    }

    D3D10_TEXTURE2D_DESC bb0_desc;
    backbuffer0->GetDesc(&bb0_desc);
    PrintTexDesc(kTestName, "backbuffer[0]", bb0_desc);
    DumpSharedHandleInfo(kTestName, "backbuffer[0]", backbuffer0.get());
    if (buffers > 1) {
      D3D10_TEXTURE2D_DESC bb1_desc;
      backbuffer1->GetDesc(&bb1_desc);
      PrintTexDesc(kTestName, "backbuffer[1]", bb1_desc);
      DumpSharedHandleInfo(kTestName, "backbuffer[1]", backbuffer1.get());
    }

    ComPtr<ID3D10RenderTargetView> rtv0;
    ComPtr<ID3D10RenderTargetView> rtv1;
    hr = device->CreateRenderTargetView(backbuffer0.get(), NULL, rtv0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRenderTargetView(backbuffer[0])", hr);
    }
    if (buffers > 1) {
      hr = device->CreateRenderTargetView(backbuffer1.get(), NULL, rtv1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("CreateRenderTargetView(backbuffer[1])", hr);
      }
    }

    D3D10_VIEWPORT vp;
    vp.TopLeftX = 0;
    vp.TopLeftY = 0;
    vp.Width = (UINT)width;
    vp.Height = (UINT)height;
    vp.MinDepth = 0.0f;
    vp.MaxDepth = 1.0f;
    device->RSSetViewports(1, &vp);

    for (uint32_t frame = 0; frame < frames; ++frame) {
      ID3D10RenderTargetView* rtv = (buffers > 1 && (frame & 1)) ? rtv1.get() : rtv0.get();
      ID3D10RenderTargetView* rtvs[] = {rtv};
      device->OMSetRenderTargets(1, rtvs, NULL);

      const FLOAT clear_rgba[4] = {(frame & 1) ? 0.0f : 1.0f, (frame & 1) ? 1.0f : 0.0f, 0.0f, 1.0f};
      device->ClearRenderTargetView(rtv, clear_rgba);

      hr = swapchain->Present(1, 0);
      if (FAILED(hr)) {
        return FailD3D10WithRemovedReason(&reporter,
                                         kTestName,
                                         "IDXGISwapChain::Present(1,0)",
                                         hr,
                                         (ID3D10Device*)device.get());
      }
    }
  } else {
    D3D_FEATURE_LEVEL feature_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                          D3D_FEATURE_LEVEL_10_1,
                                          D3D_FEATURE_LEVEL_10_0,
                                          D3D_FEATURE_LEVEL_9_3,
                                          D3D_FEATURE_LEVEL_9_2,
                                          D3D_FEATURE_LEVEL_9_1};

    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<IDXGISwapChain> swapchain;
    D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;

    const UINT flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL,
                                               D3D_DRIVER_TYPE_HARDWARE,
                                               NULL,
                                               flags,
                                               feature_levels,
                                               ARRAYSIZE(feature_levels),
                                               D3D11_SDK_VERSION,
                                               &scd,
                                               swapchain.put(),
                                               device.put(),
                                               &chosen_level,
                                               context.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("D3D11CreateDeviceAndSwapChain(HARDWARE)", hr);
    }

    // Sanity check: this mode should load the D3D11 runtime path (d3d11.dll).
    if (!GetModuleHandleW(L"d3d11.dll")) {
      return reporter.Fail("d3d11.dll is not loaded");
    }

    aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

    int adapter_rc = CheckAdapterPolicy(kTestName,
                                        &reporter,
                                        device.get(),
                                        allow_microsoft,
                                        allow_non_aerogpu,
                                        has_require_vid,
                                        require_vid,
                                        has_require_did,
                                        require_did);
    if (adapter_rc != 0) {
      return adapter_rc;
    }

    if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
      int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
      if (umd_rc != 0) {
        return umd_rc;
      }
    }

    ComPtr<ID3D11Texture2D> backbuffer0;
    ComPtr<ID3D11Texture2D> backbuffer1;
    hr = swapchain->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)backbuffer0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("IDXGISwapChain::GetBuffer(0)", hr);
    }
    if (buffers > 1) {
      hr = swapchain->GetBuffer(1, __uuidof(ID3D11Texture2D), (void**)backbuffer1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("IDXGISwapChain::GetBuffer(1)", hr);
      }
    }

    D3D11_TEXTURE2D_DESC bb0_desc;
    backbuffer0->GetDesc(&bb0_desc);
    PrintTexDesc(kTestName, "backbuffer[0]", bb0_desc);
    DumpSharedHandleInfo(kTestName, "backbuffer[0]", backbuffer0.get());
    if (buffers > 1) {
      D3D11_TEXTURE2D_DESC bb1_desc;
      backbuffer1->GetDesc(&bb1_desc);
      PrintTexDesc(kTestName, "backbuffer[1]", bb1_desc);
      DumpSharedHandleInfo(kTestName, "backbuffer[1]", backbuffer1.get());
    }

    ComPtr<ID3D11RenderTargetView> rtv0;
    ComPtr<ID3D11RenderTargetView> rtv1;
    hr = device->CreateRenderTargetView(backbuffer0.get(), NULL, rtv0.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRenderTargetView(backbuffer[0])", hr);
    }
    if (buffers > 1) {
      hr = device->CreateRenderTargetView(backbuffer1.get(), NULL, rtv1.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("CreateRenderTargetView(backbuffer[1])", hr);
      }
    }

    D3D11_VIEWPORT vp;
    vp.TopLeftX = 0;
    vp.TopLeftY = 0;
    vp.Width = (FLOAT)width;
    vp.Height = (FLOAT)height;
    vp.MinDepth = 0.0f;
    vp.MaxDepth = 1.0f;
    context->RSSetViewports(1, &vp);

    for (uint32_t frame = 0; frame < frames; ++frame) {
      ID3D11RenderTargetView* rtv = (buffers > 1 && (frame & 1)) ? rtv1.get() : rtv0.get();
      context->OMSetRenderTargets(1, &rtv, NULL);

      const FLOAT clear_rgba[4] = {(frame & 1) ? 0.0f : 1.0f, (frame & 1) ? 1.0f : 0.0f, 0.0f, 1.0f};
      context->ClearRenderTargetView(rtv, clear_rgba);

      hr = swapchain->Present(1, 0);
      if (FAILED(hr)) {
        return FailD3D11WithRemovedReason(&reporter, kTestName, "IDXGISwapChain::Present(1,0)", hr, device.get());
      }
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunDxgiSwapchainProbe(argc, argv);
  Sleep(30);
  return rc;
}
