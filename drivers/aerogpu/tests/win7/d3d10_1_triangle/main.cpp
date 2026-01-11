#include "..\\common\\aerogpu_test_common.h"

#include <d3d10_1.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float color[4];
};

static int FailD3D10WithRemovedReason(const char* test_name,
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
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static int RunD3D101Triangle(int argc, char** argv) {
  const char* kTestName = "d3d10_1_triangle";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D101Triangle",
                                              L"AeroGPU D3D10.1 Triangle",
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
  scd.BufferCount = 1;
  scd.OutputWindow = hwnd;
  scd.Windowed = TRUE;
  scd.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
  scd.Flags = 0;

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
    return aerogpu_test::FailHresult(kTestName, "D3D10CreateDeviceAndSwapChain1(HARDWARE)", hr);
  }

  // This test is specifically intended to exercise the D3D10.1 runtime path (d3d10_1.dll).
  if (!GetModuleHandleW(L"d3d10_1.dll")) {
    return aerogpu_test::Fail(kTestName, "d3d10_1.dll is not loaded");
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
        kTestName, "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  ComPtr<ID3D10Texture2D> backbuffer;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D10Texture2D), (void**)backbuffer.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer", hr);
  }

  ComPtr<ID3D10RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(backbuffer.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView", hr);
  }

  ID3D10RenderTargetView* rtvs[] = {rtv.get()};
  device->OMSetRenderTargets(1, rtvs, NULL);

  D3D10_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = kWidth;
  vp.Height = kHeight;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  device->RSSetViewports(1, &vp);

  // Load precompiled shaders generated by build_vs2010.cmd.
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring vs_path = aerogpu_test::JoinPath(dir, L"d3d10_1_triangle_vs.cso");
  const std::wstring ps_path = aerogpu_test::JoinPath(dir, L"d3d10_1_triangle_ps.cso");

  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string file_err;
  if (!aerogpu_test::ReadFileBytes(vs_path, &vs_bytes, &file_err)) {
    return aerogpu_test::Fail(kTestName, "failed to read %ls: %s", vs_path.c_str(), file_err.c_str());
  }
  if (!aerogpu_test::ReadFileBytes(ps_path, &ps_bytes, &file_err)) {
    return aerogpu_test::Fail(kTestName, "failed to read %ls: %s", ps_path.c_str(), file_err.c_str());
  }

  ComPtr<ID3D10VertexShader> vs;
  hr = device->CreateVertexShader(&vs_bytes[0], vs_bytes.size(), vs.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateVertexShader", hr);
  }

  ComPtr<ID3D10PixelShader> ps;
  hr = device->CreatePixelShader(&ps_bytes[0], ps_bytes.size(), ps.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreatePixelShader", hr);
  }

  D3D10_INPUT_ELEMENT_DESC il[] = {
      {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D10_INPUT_PER_VERTEX_DATA, 0},
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D10_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D10InputLayout> input_layout;
  hr = device->CreateInputLayout(il,
                                 ARRAYSIZE(il),
                                 &vs_bytes[0],
                                 vs_bytes.size(),
                                 input_layout.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateInputLayout", hr);
  }

  device->IASetInputLayout(input_layout.get());
  device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

  Vertex verts[3];
  // A large triangle that covers the backbuffer center (0,0 in NDC).
  verts[0].pos[0] = -1.0f;
  verts[0].pos[1] = -1.0f;
  verts[1].pos[0] = 0.0f;
  verts[1].pos[1] = 1.0f;
  verts[2].pos[0] = 1.0f;
  verts[2].pos[1] = -1.0f;
  for (int i = 0; i < 3; ++i) {
    verts[i].color[0] = 0.0f;
    verts[i].color[1] = 1.0f;
    verts[i].color[2] = 0.0f;
    verts[i].color[3] = 1.0f;
  }

  D3D10_BUFFER_DESC bd;
  ZeroMemory(&bd, sizeof(bd));
  bd.ByteWidth = sizeof(verts);
  bd.Usage = D3D10_USAGE_DEFAULT;
  bd.BindFlags = D3D10_BIND_VERTEX_BUFFER;
  bd.CPUAccessFlags = 0;
  bd.MiscFlags = 0;

  D3D10_SUBRESOURCE_DATA init;
  ZeroMemory(&init, sizeof(init));
  init.pSysMem = verts;

  ComPtr<ID3D10Buffer> vb;
  hr = device->CreateBuffer(&bd, &init, vb.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(vertex)", hr);
  }

  UINT stride = sizeof(Vertex);
  UINT offset = 0;
  ID3D10Buffer* vbs[] = {vb.get()};
  device->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

  device->VSSetShader(vs.get());
  device->PSSetShader(ps.get());

  const FLOAT clear_rgba[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  device->ClearRenderTargetView(rtv.get(), clear_rgba);
  device->Draw(3, 0);

  // Read back pixels before present.
  D3D10_TEXTURE2D_DESC bb_desc;
  backbuffer->GetDesc(&bb_desc);

  D3D10_TEXTURE2D_DESC st_desc = bb_desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;
  st_desc.Usage = D3D10_USAGE_STAGING;

  ComPtr<ID3D10Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(staging)", hr);
  }

  device->CopyResource(staging.get(), backbuffer.get());
  device->Flush();

  D3D10_MAPPED_TEXTURE2D map;
  ZeroMemory(&map, sizeof(map));
  hr = staging->Map(0, D3D10_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(kTestName, "Map(staging)", hr, device.get());
  }

  const int cx = (int)bb_desc.Width / 2;
  const int cy = (int)bb_desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);
  const uint32_t expected = 0xFF00FF00u;
  const uint32_t expected_corner = 0xFFFF0000u;

  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d10_1_triangle.bmp"),
                                      (int)bb_desc.Width,
                                      (int)bb_desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    }
  }

  staging->Unmap(0);

  hr = swapchain->Present(0, 0);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(kTestName, "IDXGISwapChain::Present", hr, device.get());
  }

  if ((center & 0x00FFFFFFu) != (expected & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(kTestName,
                              "pixel mismatch: center=0x%08lX corner(5,5)=0x%08lX",
                              (unsigned long)center,
                              (unsigned long)corner);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D101Triangle(argc, argv);
  Sleep(30);
  return rc;
}
