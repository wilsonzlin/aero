#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float color[4];
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

static int RunD3D11RSOMStateSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_rs_om_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu]",
        kTestName);
    return 0;
  }

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
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

  D3D_FEATURE_LEVEL feature_levels[] = {
      D3D_FEATURE_LEVEL_11_0,
      D3D_FEATURE_LEVEL_10_1,
      D3D_FEATURE_LEVEL_10_0,
      D3D_FEATURE_LEVEL_9_3,
      D3D_FEATURE_LEVEL_9_2,
      D3D_FEATURE_LEVEL_9_1,
  };

  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;

  HRESULT hr = D3D11CreateDevice(NULL,
                                 D3D_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 feature_levels,
                                 ARRAYSIZE(feature_levels),
                                 D3D11_SDK_VERSION,
                                 device.put(),
                                 &chosen_level,
                                 context.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "D3D11CreateDevice(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  if (chosen_level < D3D_FEATURE_LEVEL_10_0) {
    return aerogpu_test::Fail(kTestName, "feature level 0x%04X is below required FL10_0", (unsigned)chosen_level);
  }

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
            !(ad.VendorId == 0x1414 && allow_microsoft) && !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
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

  // Load precompiled shaders generated by build_vs2010.cmd.
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring vs_path = aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_vs.cso");
  const std::wstring ps_path = aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_ps.cso");

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
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateInputLayout", hr);
  }

  const int kWidth = 64;
  const int kHeight = 64;

  D3D11_TEXTURE2D_DESC tex_desc;
  ZeroMemory(&tex_desc, sizeof(tex_desc));
  tex_desc.Width = kWidth;
  tex_desc.Height = kHeight;
  tex_desc.MipLevels = 1;
  tex_desc.ArraySize = 1;
  tex_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  tex_desc.SampleDesc.Count = 1;
  tex_desc.SampleDesc.Quality = 0;
  tex_desc.Usage = D3D11_USAGE_DEFAULT;
  tex_desc.BindFlags = D3D11_BIND_RENDER_TARGET;
  tex_desc.CPUAccessFlags = 0;
  tex_desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> rt_tex;
  hr = device->CreateTexture2D(&tex_desc, NULL, rt_tex.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(render target)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(rt_tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetView", hr);
  }

  // Create a readback staging texture.
  D3D11_TEXTURE2D_DESC st_desc = tex_desc;
  st_desc.Usage = D3D11_USAGE_STAGING;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;

  ComPtr<ID3D11Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture2D(staging)", hr);
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

  context->IASetInputLayout(input_layout.get());
  context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  context->VSSetShader(vs.get(), NULL, 0);
  context->PSSetShader(ps.get(), NULL, 0);

  // Fullscreen triangle (covers entire render target).
  Vertex fs_verts[3];
  fs_verts[0].pos[0] = -1.0f;
  fs_verts[0].pos[1] = -1.0f;
  fs_verts[1].pos[0] = -1.0f;
  fs_verts[1].pos[1] = 3.0f;
  fs_verts[2].pos[0] = 3.0f;
  fs_verts[2].pos[1] = -1.0f;
  for (int i = 0; i < 3; ++i) {
    fs_verts[i].color[0] = 0.0f;
    fs_verts[i].color[1] = 1.0f;
    fs_verts[i].color[2] = 0.0f;
    fs_verts[i].color[3] = 0.5f;
  }

  // CCW centered triangle (will be culled when FrontCounterClockwise==FALSE and CullMode==BACK).
  Vertex cull_verts[3];
  cull_verts[0].pos[0] = -0.5f;
  cull_verts[0].pos[1] = -0.5f;
  cull_verts[1].pos[0] = 0.5f;
  cull_verts[1].pos[1] = -0.5f;
  cull_verts[2].pos[0] = 0.0f;
  cull_verts[2].pos[1] = 0.5f;
  for (int i = 0; i < 3; ++i) {
    cull_verts[i].color[0] = 0.0f;
    cull_verts[i].color[1] = 1.0f;
    cull_verts[i].color[2] = 0.0f;
    cull_verts[i].color[3] = 0.5f;
  }

  D3D11_BUFFER_DESC vb_desc;
  ZeroMemory(&vb_desc, sizeof(vb_desc));
  vb_desc.Usage = D3D11_USAGE_DEFAULT;
  vb_desc.BindFlags = D3D11_BIND_VERTEX_BUFFER;

  ComPtr<ID3D11Buffer> vb_fs;
  vb_desc.ByteWidth = sizeof(fs_verts);
  D3D11_SUBRESOURCE_DATA vb_init;
  ZeroMemory(&vb_init, sizeof(vb_init));
  vb_init.pSysMem = fs_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_fs.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(vb_fs)", hr);
  }

  ComPtr<ID3D11Buffer> vb_cull;
  vb_desc.ByteWidth = sizeof(cull_verts);
  vb_init.pSysMem = cull_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_cull.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(vb_cull)", hr);
  }

  // Rasterizer state: scissor enabled, no culling.
  D3D11_RASTERIZER_DESC rs_desc_scissor;
  ZeroMemory(&rs_desc_scissor, sizeof(rs_desc_scissor));
  rs_desc_scissor.FillMode = D3D11_FILL_SOLID;
  rs_desc_scissor.CullMode = D3D11_CULL_NONE;
  rs_desc_scissor.FrontCounterClockwise = FALSE;
  rs_desc_scissor.DepthClipEnable = TRUE;
  rs_desc_scissor.ScissorEnable = TRUE;

  ComPtr<ID3D11RasterizerState> rs_scissor;
  hr = device->CreateRasterizerState(&rs_desc_scissor, rs_scissor.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRasterizerState(scissor)", hr);
  }

  // Rasterizer state: cull backfaces, FrontCounterClockwise=FALSE (CW is front).
  D3D11_RASTERIZER_DESC rs_desc_cull;
  ZeroMemory(&rs_desc_cull, sizeof(rs_desc_cull));
  rs_desc_cull.FillMode = D3D11_FILL_SOLID;
  rs_desc_cull.CullMode = D3D11_CULL_BACK;
  rs_desc_cull.FrontCounterClockwise = FALSE;
  rs_desc_cull.DepthClipEnable = TRUE;

  ComPtr<ID3D11RasterizerState> rs_cull_front_cw;
  hr = device->CreateRasterizerState(&rs_desc_cull, rs_cull_front_cw.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRasterizerState(cull front=CW)", hr);
  }

  // Rasterizer state: cull backfaces, FrontCounterClockwise=TRUE (CCW is front).
  rs_desc_cull.FrontCounterClockwise = TRUE;

  ComPtr<ID3D11RasterizerState> rs_cull_front_ccw;
  hr = device->CreateRasterizerState(&rs_desc_cull, rs_cull_front_ccw.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRasterizerState(cull front=CCW)", hr);
  }

  // Rasterizer state: no culling (used for blend).
  D3D11_RASTERIZER_DESC rs_desc_no_cull;
  ZeroMemory(&rs_desc_no_cull, sizeof(rs_desc_no_cull));
  rs_desc_no_cull.FillMode = D3D11_FILL_SOLID;
  rs_desc_no_cull.CullMode = D3D11_CULL_NONE;
  rs_desc_no_cull.FrontCounterClockwise = FALSE;
  rs_desc_no_cull.DepthClipEnable = TRUE;

  ComPtr<ID3D11RasterizerState> rs_no_cull;
  hr = device->CreateRasterizerState(&rs_desc_no_cull, rs_no_cull.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRasterizerState(no cull)", hr);
  }

  // Blend state: standard alpha blending.
  D3D11_BLEND_DESC blend_desc;
  ZeroMemory(&blend_desc, sizeof(blend_desc));
  blend_desc.AlphaToCoverageEnable = FALSE;
  blend_desc.IndependentBlendEnable = FALSE;
  blend_desc.RenderTarget[0].BlendEnable = TRUE;
  blend_desc.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA;
  blend_desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA;
  blend_desc.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
  blend_desc.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE;
  blend_desc.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO;
  blend_desc.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
  blend_desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;

  ComPtr<ID3D11BlendState> alpha_blend;
  hr = device->CreateBlendState(&blend_desc, alpha_blend.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBlendState(alpha)", hr);
  }

  const FLOAT clear_red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  const FLOAT blend_factor[4] = {0.0f, 0.0f, 0.0f, 0.0f};
  const D3D11_RECT full_rect = {0, 0, kWidth, kHeight};

  // Subtest 1: Scissor (left half should turn green, right half must remain red).
  {
    context->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    context->RSSetState(rs_scissor.get());
    context->RSSetScissorRects(1, &full_rect);

    const D3D11_RECT scissor = {0, 0, kWidth / 2, kHeight};
    context->RSSetScissorRects(1, &scissor);

    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->CopyResource(staging.get(), rt_tex.get());
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(kTestName, "Map(staging) [scissor]", hr, device.get());
    }

    const uint32_t inside = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, kHeight / 2);
    const uint32_t outside =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth - 5, kHeight / 2);

    if (dump) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_scissor.bmp"),
                                        kWidth,
                                        kHeight,
                                        map.pData,
                                        (int)map.RowPitch,
                                        &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: scissor BMP dump failed: %s", kTestName, err.c_str());
      }
    }

    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0xFF00FF00u;
    const uint32_t expected_red = 0xFFFF0000u;
    if ((inside & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (outside & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      return aerogpu_test::Fail(kTestName,
                                "scissor failed: inside(5,%d)=0x%08lX expected ~0x%08lX, "
                                "outside(%d,%d)=0x%08lX expected ~0x%08lX",
                                kHeight / 2,
                                (unsigned long)inside,
                                (unsigned long)expected_green,
                                kWidth - 5,
                                kHeight / 2,
                                (unsigned long)outside,
                                (unsigned long)expected_red);
    }
  }

  // Subtest 2: Cull mode + FrontCounterClockwise toggling.
  {
    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs[] = {vb_cull.get()};
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
    context->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    context->RSSetScissorRects(1, &full_rect);

    // First: FrontCounterClockwise=FALSE, CCW triangle should be culled (center remains red).
    context->RSSetState(rs_cull_front_cw.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->CopyResource(staging.get(), rt_tex.get());
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(kTestName, "Map(staging) [cull culled]", hr, device.get());
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t center_culled = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(
              aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_culled.bmp"),
              kWidth,
              kHeight,
              map.pData,
              (int)map.RowPitch,
              &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(culled) BMP dump failed: %s", kTestName, err.c_str());
      }
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_red = 0xFFFF0000u;
    if ((center_culled & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      return aerogpu_test::Fail(kTestName,
                                "cull failed (expected culled): center(%d,%d)=0x%08lX expected ~0x%08lX",
                                cx,
                                cy,
                                (unsigned long)center_culled,
                                (unsigned long)expected_red);
    }

    // Second: FrontCounterClockwise=TRUE, same CCW triangle should render (center becomes green).
    context->RSSetState(rs_cull_front_ccw.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->CopyResource(staging.get(), rt_tex.get());
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(kTestName, "Map(staging) [cull drawn]", hr, device.get());
    }

    const uint32_t center_drawn = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(
              aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_drawn.bmp"),
              kWidth,
              kHeight,
              map.pData,
              (int)map.RowPitch,
              &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(drawn) BMP dump failed: %s", kTestName, err.c_str());
      }
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0xFF00FF00u;
    if ((center_drawn & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      return aerogpu_test::Fail(kTestName,
                                "cull failed (expected visible): center(%d,%d)=0x%08lX expected ~0x%08lX",
                                cx,
                                cy,
                                (unsigned long)center_drawn,
                                (unsigned long)expected_green);
    }
  }

  // Subtest 3: Blend (green with alpha=0.5 over red should yield ~yellow).
  {
    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
    context->RSSetState(rs_no_cull.get());
    context->RSSetScissorRects(1, &full_rect);

    context->OMSetBlendState(alpha_blend.get(), blend_factor, 0xFFFFFFFFu);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->CopyResource(staging.get(), rt_tex.get());
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(kTestName, "Map(staging) [blend]", hr, device.get());
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);

    if (dump) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_blend.bmp"),
                                        kWidth,
                                        kHeight,
                                        map.pData,
                                        (int)map.RowPitch,
                                        &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: blend BMP dump failed: %s", kTestName, err.c_str());
      }
    }

    context->Unmap(staging.get(), 0);

    const uint8_t b = (uint8_t)(center & 0xFFu);
    const uint8_t g = (uint8_t)((center >> 8) & 0xFFu);
    const uint8_t r = (uint8_t)((center >> 16) & 0xFFu);

    const uint8_t exp_r = 0x80;
    const uint8_t exp_g = 0x80;
    const uint8_t exp_b = 0x00;
    const uint8_t tol = 2;

    if ((r < exp_r - tol || r > exp_r + tol) || (g < exp_g - tol || g > exp_g + tol) ||
        (b < exp_b - tol || b > exp_b + tol)) {
      return aerogpu_test::Fail(kTestName,
                                "blend failed: center(%d,%d)=0x%08lX (r=%u g=%u b=%u) expected ~"
                                "(r=%u g=%u b=%u) tol=%u",
                                cx,
                                cy,
                                (unsigned long)center,
                                (unsigned)r,
                                (unsigned)g,
                                (unsigned)b,
                                (unsigned)exp_r,
                                (unsigned)exp_g,
                                (unsigned)exp_b,
                                (unsigned)tol);
    }
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11RSOMStateSanity(argc, argv);
}

