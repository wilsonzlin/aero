#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

static int FailD3D11WithRemovedReason(const char* test_name,
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

static void DumpSharedHandleInfo(const char* test_name, const char* label, ID3D11Texture2D* tex) {
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

static int RunDxgiSwapchainProbe(int argc, char** argv) {
  const char* kTestName = "dxgi_swapchain_probe";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--frames=N] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu]",
        kTestName);
    return 0;
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

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
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_DXGISwapchainProbe",
                                              L"AeroGPU DXGI Swapchain Probe",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  DXGI_SWAP_CHAIN_DESC scd;
  ZeroMemory(&scd, sizeof(scd));
  scd.BufferDesc.Width = kWidth;
  scd.BufferDesc.Height = kHeight;
  scd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  scd.BufferDesc.RefreshRate.Numerator = 60;
  scd.BufferDesc.RefreshRate.Denominator = 1;
  scd.SampleDesc.Count = 1;
  scd.SampleDesc.Quality = 0;
  scd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
  scd.BufferCount = 2;
  scd.OutputWindow = hwnd;
  scd.Windowed = TRUE;
  scd.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
  scd.Flags = 0;

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
    return aerogpu_test::FailHresult(kTestName, "D3D11CreateDeviceAndSwapChain(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

  // Optional adapter sanity checks (same policy as other tests in this suite).
  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return aerogpu_test::FailHresult(
            kTestName, "IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return aerogpu_test::FailHresult(
              kTestName, "IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc);
        }
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                                   kTestName,
                                   ad.Description,
                                   (unsigned)ad.VendorId,
                                   (unsigned)ad.DeviceId);
        if (!allow_microsoft && ad.VendorId == 0x1414) {
          return aerogpu_test::Fail(kTestName,
                                    "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                    "Install AeroGPU driver or pass --allow-microsoft.",
                                    (unsigned)ad.VendorId,
                                    (unsigned)ad.DeviceId);
        }
        if (has_require_vid && ad.VendorId != require_vid) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.VendorId,
                                    (unsigned)require_vid);
        }
        if (has_require_did && ad.DeviceId != require_did) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                    (unsigned)ad.DeviceId,
                                    (unsigned)require_did);
        }
        if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
            !(ad.VendorId == 0x1414 && allow_microsoft) &&
            !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
          return aerogpu_test::Fail(kTestName,
                                    "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu "
                                    "or use --require-vid/--require-did)",
                                    ad.Description);
        }
      }
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(
        kTestName, "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  ComPtr<ID3D11Texture2D> backbuffer0;
  ComPtr<ID3D11Texture2D> backbuffer1;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)backbuffer0.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer(0)", hr);
  }
  hr = swapchain->GetBuffer(1, __uuidof(ID3D11Texture2D), (void**)backbuffer1.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer(1)", hr);
  }

  D3D11_TEXTURE2D_DESC bb0_desc;
  D3D11_TEXTURE2D_DESC bb1_desc;
  backbuffer0->GetDesc(&bb0_desc);
  backbuffer1->GetDesc(&bb1_desc);
  PrintTexDesc(kTestName, "backbuffer[0]", bb0_desc);
  PrintTexDesc(kTestName, "backbuffer[1]", bb1_desc);
  DumpSharedHandleInfo(kTestName, "backbuffer[0]", backbuffer0.get());
  DumpSharedHandleInfo(kTestName, "backbuffer[1]", backbuffer1.get());

  ComPtr<ID3D11RenderTargetView> rtv0;
  ComPtr<ID3D11RenderTargetView> rtv1;
  hr = device->CreateRenderTargetView(backbuffer0.get(), NULL, rtv0.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView(backbuffer[0])", hr);
  }
  hr = device->CreateRenderTargetView(backbuffer1.get(), NULL, rtv1.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView(backbuffer[1])", hr);
  }

  D3D11_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = (FLOAT)kWidth;
  vp.Height = (FLOAT)kHeight;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  context->RSSetViewports(1, &vp);

  for (uint32_t frame = 0; frame < frames; ++frame) {
    ID3D11RenderTargetView* rtv = (frame & 1) ? rtv1.get() : rtv0.get();
    context->OMSetRenderTargets(1, &rtv, NULL);

    const FLOAT clear_rgba[4] = {(frame & 1) ? 0.0f : 1.0f, (frame & 1) ? 1.0f : 0.0f, 0.0f, 1.0f};
    context->ClearRenderTargetView(rtv, clear_rgba);

    hr = swapchain->Present(1, 0);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(kTestName, "IDXGISwapChain::Present(1,0)", hr, device.get());
    }
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunDxgiSwapchainProbe(argc, argv);
  Sleep(30);
  return rc;
}

