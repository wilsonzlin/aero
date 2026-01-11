#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float uv[2];
};

struct Params {
  float tint[4];
};

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

static int RunD3D11Texture(int argc, char** argv) {
  const char* kTestName = "d3d11_texture";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu]",
        kTestName);
    return 0;
  }
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D11Texture",
                                              L"AeroGPU D3D11 Texture",
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
    return aerogpu_test::FailHresult(kTestName, "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
  }

  ComPtr<ID3D11Texture2D> backbuffer;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)backbuffer.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDXGISwapChain::GetBuffer", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(backbuffer.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView", hr);
  }

  ID3D11RenderTargetView* rtvs[] = {rtv.get()};
  context->OMSetRenderTargets(1, rtvs, NULL);

  D3D11_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = (FLOAT)kWidth;
  vp.Height = (FLOAT)kHeight;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  context->RSSetViewports(1, &vp);

  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring vs_path = aerogpu_test::JoinPath(dir, L"d3d11_texture_vs.cso");
  const std::wstring ps_path = aerogpu_test::JoinPath(dir, L"d3d11_texture_ps.cso");

  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string file_err;
  if (!aerogpu_test::ReadFileBytes(vs_path, &vs_bytes, &file_err)) {
    return aerogpu_test::Fail(kTestName, "failed to read %ls: %s", vs_path.c_str(), file_err.c_str());
  }
  if (!aerogpu_test::ReadFileBytes(ps_path, &ps_bytes, &file_err)) {
    return aerogpu_test::Fail(kTestName, "failed to read %ls: %s", ps_path.c_str(), file_err.c_str());
  }

  ComPtr<ID3D11VertexShader> vs;
  hr = device->CreateVertexShader(&vs_bytes[0], vs_bytes.size(), NULL, vs.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateVertexShader", hr);
  }

  ComPtr<ID3D11PixelShader> ps;
  hr = device->CreatePixelShader(&ps_bytes[0], ps_bytes.size(), NULL, ps.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreatePixelShader", hr);
  }

  D3D11_INPUT_ELEMENT_DESC il[] = {
      {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
      {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(il,
                                 ARRAYSIZE(il),
                                 &vs_bytes[0],
                                 vs_bytes.size(),
                                 input_layout.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateInputLayout", hr);
  }

  context->IASetInputLayout(input_layout.get());
  context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

  Vertex verts[3];
  verts[0].pos[0] = -1.0f;
  verts[0].pos[1] = -1.0f;
  verts[1].pos[0] = 0.0f;
  verts[1].pos[1] = 1.0f;
  verts[2].pos[0] = 1.0f;
  verts[2].pos[1] = -1.0f;
  for (int i = 0; i < 3; ++i) {
    verts[i].uv[0] = 0.25f;
    verts[i].uv[1] = 0.25f;
  }

  D3D11_BUFFER_DESC bd;
  ZeroMemory(&bd, sizeof(bd));
  bd.ByteWidth = sizeof(verts);
  bd.Usage = D3D11_USAGE_DEFAULT;
  bd.BindFlags = D3D11_BIND_VERTEX_BUFFER;

  D3D11_SUBRESOURCE_DATA init_vb;
  ZeroMemory(&init_vb, sizeof(init_vb));
  init_vb.pSysMem = verts;

  ComPtr<ID3D11Buffer> vb;
  hr = device->CreateBuffer(&bd, &init_vb, vb.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(vertex)", hr);
  }

  UINT stride = sizeof(Vertex);
  UINT offset = 0;
  ID3D11Buffer* vbs[] = {vb.get()};
  context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

  uint32_t texel_bgra[4];
  texel_bgra[0] = 0xFF0000FFu;  // top-left: blue
  texel_bgra[1] = 0xFF00FF00u;  // top-right: green
  texel_bgra[2] = 0xFFFF0000u;  // bottom-left: red
  texel_bgra[3] = 0xFFFFFFFFu;  // bottom-right: white

  D3D11_TEXTURE2D_DESC td;
  ZeroMemory(&td, sizeof(td));
  td.Width = 2;
  td.Height = 2;
  td.MipLevels = 1;
  td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  td.SampleDesc.Count = 1;
  td.SampleDesc.Quality = 0;
  td.Usage = D3D11_USAGE_DEFAULT;
  td.BindFlags = D3D11_BIND_SHADER_RESOURCE;

  D3D11_SUBRESOURCE_DATA init_tex;
  ZeroMemory(&init_tex, sizeof(init_tex));
  init_tex.pSysMem = texel_bgra;
  init_tex.SysMemPitch = 2 * 4;

  ComPtr<ID3D11Texture2D> tex;
  hr = device->CreateTexture2D(&td, &init_tex, tex.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D", hr);
  }

  ComPtr<ID3D11ShaderResourceView> srv;
  hr = device->CreateShaderResourceView(tex.get(), NULL, srv.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateShaderResourceView", hr);
  }

  D3D11_SAMPLER_DESC sd;
  ZeroMemory(&sd, sizeof(sd));
  sd.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
  sd.AddressU = D3D11_TEXTURE_ADDRESS_CLAMP;
  sd.AddressV = D3D11_TEXTURE_ADDRESS_CLAMP;
  sd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
  sd.MaxLOD = D3D11_FLOAT32_MAX;

  ComPtr<ID3D11SamplerState> sampler;
  hr = device->CreateSamplerState(&sd, sampler.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateSamplerState", hr);
  }

  Params params;
  params.tint[0] = 1.0f;
  params.tint[1] = 1.0f;
  params.tint[2] = 1.0f;
  params.tint[3] = 1.0f;

  D3D11_BUFFER_DESC cbd;
  ZeroMemory(&cbd, sizeof(cbd));
  cbd.ByteWidth = sizeof(params);
  cbd.Usage = D3D11_USAGE_DEFAULT;
  cbd.BindFlags = D3D11_BIND_CONSTANT_BUFFER;

  D3D11_SUBRESOURCE_DATA init_cb;
  ZeroMemory(&init_cb, sizeof(init_cb));
  init_cb.pSysMem = &params;

  ComPtr<ID3D11Buffer> cb;
  hr = device->CreateBuffer(&cbd, &init_cb, cb.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(constant)", hr);
  }

  context->VSSetShader(vs.get(), NULL, 0);
  context->PSSetShader(ps.get(), NULL, 0);

  ID3D11ShaderResourceView* srvs[] = {srv.get()};
  context->VSSetShaderResources(0, 1, srvs);
  context->PSSetShaderResources(0, 1, srvs);

  ID3D11SamplerState* samplers[] = {sampler.get()};
  context->VSSetSamplers(0, 1, samplers);
  context->PSSetSamplers(0, 1, samplers);

  ID3D11Buffer* cbs[] = {cb.get()};
  context->VSSetConstantBuffers(0, 1, cbs);
  context->PSSetConstantBuffers(0, 1, cbs);

  const FLOAT clear_rgba[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  context->ClearRenderTargetView(rtv.get(), clear_rgba);
  context->Draw(3, 0);

  D3D11_TEXTURE2D_DESC bb_desc;
  backbuffer->GetDesc(&bb_desc);

  D3D11_TEXTURE2D_DESC st_desc = bb_desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  st_desc.Usage = D3D11_USAGE_STAGING;

  ComPtr<ID3D11Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(staging)", hr);
  }

  context->CopyResource(staging.get(), backbuffer.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging)", hr, device.get());
  }

  const int cx = (int)bb_desc.Width / 2;
  const int cy = (int)bb_desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);
  const uint32_t expected = 0xFF0000FFu;
  const uint32_t expected_corner = 0xFFFF0000u;

  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_texture.bmp"),
                                      (int)bb_desc.Width,
                                      (int)bb_desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    }
  }

  context->Unmap(staging.get(), 0);

  hr = swapchain->Present(0, 0);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "IDXGISwapChain::Present", hr, device.get());
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
  int rc = RunD3D11Texture(argc, argv);
  Sleep(30);
  return rc;
}

