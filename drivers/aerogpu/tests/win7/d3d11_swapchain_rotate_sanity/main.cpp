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

static int RunD3D11SwapchainRotateSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_swapchain_rotate_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
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

  const int kWidth = 128;
  const int kHeight = 128;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D11SwapchainRotateSanity",
                                              L"AeroGPU D3D11 Swapchain Rotate Sanity",
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

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return aerogpu_test::FailHresult(kTestName,
                                         "IDXGIDevice::GetAdapter (required for --require-vid/--require-did)",
                                         hr_adapter);
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
        kTestName,
        "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)",
        hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  ComPtr<ID3D11Texture2D> buffer0;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)buffer0.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer(0)", hr);
  }

  ComPtr<ID3D11Texture2D> buffer1;
  hr = swapchain->GetBuffer(1, __uuidof(ID3D11Texture2D), (void**)buffer1.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer(1)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv0;
  hr = device->CreateRenderTargetView(buffer0.get(), NULL, rtv0.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView(buffer0)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv1;
  hr = device->CreateRenderTargetView(buffer1.get(), NULL, rtv1.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView(buffer1)", hr);
  }

  D3D11_TEXTURE2D_DESC bb_desc;
  buffer0->GetDesc(&bb_desc);

  D3D11_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = (FLOAT)bb_desc.Width;
  vp.Height = (FLOAT)bb_desc.Height;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  context->RSSetViewports(1, &vp);

  const FLOAT red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  const FLOAT green[4] = {0.0f, 1.0f, 0.0f, 1.0f};

  ID3D11RenderTargetView* rtvs0[] = {rtv0.get()};
  context->OMSetRenderTargets(1, rtvs0, NULL);
  context->ClearRenderTargetView(rtv0.get(), red);

  ID3D11RenderTargetView* rtvs1[] = {rtv1.get()};
  context->OMSetRenderTargets(1, rtvs1, NULL);
  context->ClearRenderTargetView(rtv1.get(), green);

  D3D11_TEXTURE2D_DESC st_desc = bb_desc;
  st_desc.Usage = D3D11_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;

  ComPtr<ID3D11Texture2D> staging0;
  hr = device->CreateTexture2D(&st_desc, NULL, staging0.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(staging0)", hr);
  }

  ComPtr<ID3D11Texture2D> staging1;
  hr = device->CreateTexture2D(&st_desc, NULL, staging1.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(staging1)", hr);
  }

  // Validate the pre-present contents to make swapchain-rotation failures clearer. If these don't
  // match, the failure is in rendering/readback rather than RotateResourceIdentities.
  context->CopyResource(staging0.get(), buffer0.get());
  context->CopyResource(staging1.get(), buffer1.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging0.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging0, pre-present)", hr, device.get());
  }
  const uint32_t before0 =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, (int)bb_desc.Width / 2, (int)bb_desc.Height / 2);
  context->Unmap(staging0.get(), 0);

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging1.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging1, pre-present)", hr, device.get());
  }
  const uint32_t before1 =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, (int)bb_desc.Width / 2, (int)bb_desc.Height / 2);
  context->Unmap(staging1.get(), 0);

  const uint32_t expected_before0 = 0xFFFF0000u;
  const uint32_t expected_before1 = 0xFF00FF00u;
  if ((before0 & 0x00FFFFFFu) != (expected_before0 & 0x00FFFFFFu) ||
      (before1 & 0x00FFFFFFu) != (expected_before1 & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(kTestName,
                              "pre-present buffer contents mismatch: buffer0=0x%08lX buffer1=0x%08lX (expected buffer0~0x%08lX buffer1~0x%08lX)",
                              (unsigned long)before0,
                              (unsigned long)before1,
                              (unsigned long)expected_before0,
                              (unsigned long)expected_before1);
  }

  hr = swapchain->Present(0, 0);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "IDXGISwapChain::Present", hr, device.get());
  }

  context->CopyResource(staging0.get(), buffer0.get());
  context->CopyResource(staging1.get(), buffer1.get());
  context->Flush();

  const std::wstring dir = aerogpu_test::GetModuleDir();

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging0.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging0)", hr, device.get());
  }
  const uint32_t after0 =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, (int)bb_desc.Width / 2, (int)bb_desc.Height / 2);
  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_swapchain_rotate_sanity_buffer0.bmp"),
                                      (int)bb_desc.Width,
                                      (int)bb_desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump for buffer0 failed: %s", kTestName, err.c_str());
    }
  }
  context->Unmap(staging0.get(), 0);

  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging1.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging1)", hr, device.get());
  }
  const uint32_t after1 =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, (int)bb_desc.Width / 2, (int)bb_desc.Height / 2);
  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_swapchain_rotate_sanity_buffer1.bmp"),
                                      (int)bb_desc.Width,
                                      (int)bb_desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump for buffer1 failed: %s", kTestName, err.c_str());
    }
  }
  context->Unmap(staging1.get(), 0);

  const uint32_t expected0 = 0xFF00FF00u;
  const uint32_t expected1 = 0xFFFF0000u;

  if ((after0 & 0x00FFFFFFu) != (expected0 & 0x00FFFFFFu) ||
      (after1 & 0x00FFFFFFu) != (expected1 & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(kTestName,
                              "swapchain buffer identity mismatch after Present: before(buffer0=0x%08lX buffer1=0x%08lX) after(buffer0=0x%08lX buffer1=0x%08lX) (expected after buffer0~0x%08lX buffer1~0x%08lX)",
                              (unsigned long)before0,
                              (unsigned long)before1,
                              (unsigned long)after0,
                              (unsigned long)after1,
                              (unsigned long)expected0,
                              (unsigned long)expected1);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D11SwapchainRotateSanity(argc, argv);
  Sleep(30);
  return rc;
}
