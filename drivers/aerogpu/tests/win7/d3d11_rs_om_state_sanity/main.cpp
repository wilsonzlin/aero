#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float color[4];
};

static const char kStateHlsl[] = R"(struct VSIn {
  float2 pos : POSITION;
  float4 color : COLOR0;
};

struct VSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  o.color = input.color;
  return o;
}

VSOut vs_depth_clip_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, -0.5f, 1.0f);
  o.color = input.color;
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  return input.color;
}
)";

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

static void PrintDeviceRemovedReasonIfAny(const char* test_name, ID3D11Device* device) {
  if (!device) {
    return;
  }
  HRESULT reason = device->GetDeviceRemovedReason();
  if (reason != S_OK) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: device removed reason: %s", test_name, aerogpu_test::HresultToString(reason).c_str());
  }
}

static void DumpBytesToFile(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT byte_count) {
  if (!file_name || !data || byte_count == 0) {
    return;
  }
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring path = aerogpu_test::JoinPath(dir, file_name);
  HANDLE h =
      CreateFileW(path.c_str(), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    aerogpu_test::PrintfStdout("INFO: %s: dump CreateFileW(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    return;
  }
  DWORD written = 0;
  if (!WriteFile(h, data, byte_count, &written, NULL) || written != byte_count) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name,
                               (unsigned)byte_count,
                               path.c_str());
    if (reporter) {
      reporter->AddArtifactPathW(path);
    }
  }
  CloseHandle(h);
}

static void DumpTightBgra32(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT row_pitch,
                            int width,
                            int height) {
  if (!data || width <= 0 || height <= 0 || row_pitch < (UINT)(width * 4)) {
    return;
  }
  std::vector<uint8_t> tight((size_t)width * (size_t)height * 4u, 0);
  for (int y = 0; y < height; ++y) {
    const uint8_t* src_row = (const uint8_t*)data + (size_t)y * (size_t)row_pitch;
    memcpy(&tight[(size_t)y * (size_t)width * 4u], src_row, (size_t)width * 4u);
  }
  DumpBytesToFile(test_name, reporter, file_name, &tight[0], (UINT)tight.size());
}

static int RunD3D11RSOMStateSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_rs_om_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
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
    return reporter.FailHresult("D3D11CreateDevice(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  if (chosen_level < D3D_FEATURE_LEVEL_10_0) {
    const std::string skip_reason = aerogpu_test::FormatString(
        "feature level 0x%04X is below D3D_FEATURE_LEVEL_10_0 (0x%04X)",
        (unsigned)chosen_level,
        (unsigned)D3D_FEATURE_LEVEL_10_0);
    reporter.SetSkipped(skip_reason.c_str());
    aerogpu_test::PrintfStdout("SKIP: %s: %s", kTestName, skip_reason.c_str());
    return reporter.Pass();
  }

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return reporter.FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)",
                                    hr_adapter);
      }
    } else {
      DXGI_ADAPTER_DESC ad;
      ZeroMemory(&ad, sizeof(ad));
      HRESULT hr_desc = adapter->GetDesc(&ad);
      if (FAILED(hr_desc)) {
        if (has_require_vid || has_require_did) {
          return reporter.FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)",
                                      hr_desc);
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
            !(ad.VendorId == 0x1414 && allow_microsoft) && !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
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
  }

  // Compile shaders at runtime (no fxc.exe build-time dependency).
  const std::wstring dir = aerogpu_test::GetModuleDir();
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> vs_depth_clip_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(kStateHlsl,
                                           strlen(kStateHlsl),
                                           "d3d11_rs_om_state_sanity.hlsl",
                                           "vs_main",
                                           "vs_4_0",
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader (vs_main): %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(kStateHlsl,
                                           strlen(kStateHlsl),
                                           "d3d11_rs_om_state_sanity.hlsl",
                                           "vs_depth_clip_main",
                                           "vs_4_0",
                                           &vs_depth_clip_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader (vs_depth_clip_main): %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(kStateHlsl,
                                           strlen(kStateHlsl),
                                           "d3d11_rs_om_state_sanity.hlsl",
                                           "ps_main",
                                           "ps_4_0",
                                           &ps_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile pixel shader: %s", shader_err.c_str());
  }

  ComPtr<ID3D11VertexShader> vs;
  hr = device->CreateVertexShader(&vs_bytes[0], vs_bytes.size(), NULL, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }

  ComPtr<ID3D11VertexShader> vs_depth_clip;
  hr = device->CreateVertexShader(&vs_depth_clip_bytes[0], vs_depth_clip_bytes.size(), NULL, vs_depth_clip.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader(vs_depth_clip)", hr);
  }

  ComPtr<ID3D11PixelShader> ps;
  hr = device->CreatePixelShader(&ps_bytes[0], ps_bytes.size(), NULL, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  D3D11_INPUT_ELEMENT_DESC il[] = {
      {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateInputLayout", hr);
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
    return reporter.FailHresult("CreateTexture2D(render target)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(rt_tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView", hr);
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
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  const UINT min_row_pitch = (UINT)kWidth * 4;
  const auto ValidateStagingMap = [&](const char* label, const D3D11_MAPPED_SUBRESOURCE& map) -> int {
    if (!map.pData) {
      context->Unmap(staging.get(), 0);
      return reporter.Fail("%s returned NULL pData", label);
    }
    if (map.RowPitch < min_row_pitch) {
      context->Unmap(staging.get(), 0);
      return reporter.Fail("%s returned too-small RowPitch=%u (min=%u)",
                           label,
                           (unsigned)map.RowPitch,
                           (unsigned)min_row_pitch);
    }
    return 0;
  };
  const uint8_t kExpectedAlphaHalf = 0x80u;
  const uint8_t kAlphaTol = 2u;

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
    return reporter.FailHresult("CreateBuffer(vb_fs)", hr);
  }

  ComPtr<ID3D11Buffer> vb_cull;
  vb_desc.ByteWidth = sizeof(cull_verts);
  vb_init.pSysMem = cull_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_cull.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_cull)", hr);
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
    return reporter.FailHresult("CreateRasterizerState(scissor)", hr);
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
    return reporter.FailHresult("CreateRasterizerState(cull front=CW)", hr);
  }

  // Rasterizer state: cull backfaces, FrontCounterClockwise=TRUE (CCW is front).
  rs_desc_cull.FrontCounterClockwise = TRUE;

  ComPtr<ID3D11RasterizerState> rs_cull_front_ccw;
  hr = device->CreateRasterizerState(&rs_desc_cull, rs_cull_front_ccw.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRasterizerState(cull front=CCW)", hr);
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
    return reporter.FailHresult("CreateRasterizerState(no cull)", hr);
  }

  // Rasterizer state: no culling, depth clip disabled (used for depth clip test).
  D3D11_RASTERIZER_DESC rs_desc_no_depth_clip = rs_desc_no_cull;
  rs_desc_no_depth_clip.DepthClipEnable = FALSE;

  ComPtr<ID3D11RasterizerState> rs_no_cull_no_depth_clip;
  hr = device->CreateRasterizerState(&rs_desc_no_depth_clip, rs_no_cull_no_depth_clip.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRasterizerState(no depth clip)", hr);
  }

  // Rasterizer state: scissor enabled, no culling, depth clip disabled (used for ClearState test).
  D3D11_RASTERIZER_DESC rs_desc_scissor_no_depth_clip = rs_desc_scissor;
  rs_desc_scissor_no_depth_clip.DepthClipEnable = FALSE;

  ComPtr<ID3D11RasterizerState> rs_scissor_no_depth_clip;
  hr = device->CreateRasterizerState(&rs_desc_scissor_no_depth_clip, rs_scissor_no_depth_clip.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRasterizerState(scissor no depth clip)", hr);
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
    return reporter.FailHresult("CreateBlendState(alpha)", hr);
  }

  // Blend state: blending disabled, but with a non-default color write mask (green channel only).
  // This validates that the blend state object is honored even when BlendEnable=FALSE.
  D3D11_BLEND_DESC green_mask_desc = blend_desc;
  green_mask_desc.RenderTarget[0].BlendEnable = FALSE;
  green_mask_desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_GREEN;

  ComPtr<ID3D11BlendState> green_write_mask;
  hr = device->CreateBlendState(&green_mask_desc, green_write_mask.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBlendState(write mask)", hr);
  }

  // Blend state: uses constant blend factor (SrcBlend=BLEND_FACTOR, DestBlend=INV_BLEND_FACTOR).
  // This validates that the blend-factor parameter to OMSetBlendState is honored.
  D3D11_BLEND_DESC factor_desc = blend_desc;
  factor_desc.RenderTarget[0].SrcBlend = D3D11_BLEND_BLEND_FACTOR;
  factor_desc.RenderTarget[0].DestBlend = D3D11_BLEND_INV_BLEND_FACTOR;

  ComPtr<ID3D11BlendState> blend_factor_state;
  hr = device->CreateBlendState(&factor_desc, blend_factor_state.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBlendState(blend factor)", hr);
  }

  const FLOAT clear_red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  const FLOAT blend_factor[4] = {0.0f, 0.0f, 0.0f, 0.0f};
  const D3D11_RECT full_rect = {0, 0, kWidth, kHeight};
  int map_rc = 0;

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

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [scissor]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [scissor]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t inside = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, kHeight / 2);
    const uint32_t outside =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth - 5, kHeight / 2);

    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_scissor.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: scissor BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_scissor.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }

    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0xFF00FF00u;
    const uint32_t expected_red = 0xFFFF0000u;
    const uint32_t expected_green_rgb = expected_green & 0x00FFFFFFu;
    const uint32_t expected_red_rgb = expected_red & 0x00FFFFFFu;
    const uint8_t inside_a = (uint8_t)((inside >> 24) & 0xFFu);
    const uint8_t outside_a = (uint8_t)((outside >> 24) & 0xFFu);
    if ((inside & 0x00FFFFFFu) != expected_green_rgb ||
        (inside_a < kExpectedAlphaHalf - kAlphaTol || inside_a > kExpectedAlphaHalf + kAlphaTol) ||
        (outside & 0x00FFFFFFu) != expected_red_rgb || outside_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "scissor failed: inside(5,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a~%u), "
          "outside(%d,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a=%u)",
          kHeight / 2,
          (unsigned long)inside,
          (unsigned)inside_a,
          (unsigned long)expected_green_rgb,
          (unsigned)kExpectedAlphaHalf,
          kWidth - 5,
          kHeight / 2,
          (unsigned long)outside,
          (unsigned)outside_a,
          (unsigned long)expected_red_rgb,
          0xFFu);
    }

    // Verify that RSSetState(NULL) restores the default rasterizer state, which has scissor disabled.
    // Keep the scissor rect set to left-half; if ScissorEnable is still effectively TRUE, the draw will
    // remain clipped and the outside pixel will stay red.
    context->RSSetState(NULL);
    context->RSSetScissorRects(1, &scissor);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [scissor NULL state]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [scissor NULL state]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t inside_null =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, kHeight / 2);
    const uint32_t outside_null =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth - 5, kHeight / 2);

    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_scissor_null_state.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: scissor-NULL-state BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_scissor_null_state.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }

    context->Unmap(staging.get(), 0);

    const uint8_t inside_null_a = (uint8_t)((inside_null >> 24) & 0xFFu);
    const uint8_t outside_null_a = (uint8_t)((outside_null >> 24) & 0xFFu);
    if ((inside_null & 0x00FFFFFFu) != expected_green_rgb ||
        (inside_null_a < kExpectedAlphaHalf - kAlphaTol ||
         inside_null_a > kExpectedAlphaHalf + kAlphaTol) ||
        (outside_null & 0x00FFFFFFu) != expected_green_rgb ||
        (outside_null_a < kExpectedAlphaHalf - kAlphaTol ||
         outside_null_a > kExpectedAlphaHalf + kAlphaTol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "scissor NULL state failed: inside(5,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a~%u), "
          "outside(%d,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a~%u)",
          kHeight / 2,
          (unsigned long)inside_null,
          (unsigned)inside_null_a,
          (unsigned long)expected_green_rgb,
          (unsigned)kExpectedAlphaHalf,
          kWidth - 5,
          kHeight / 2,
          (unsigned long)outside_null,
          (unsigned)outside_null_a,
          (unsigned long)expected_green_rgb,
          (unsigned)kExpectedAlphaHalf);
    }

    // Verify that the scissor rect is ignored when ScissorEnable is FALSE (explicit rasterizer state).
    context->RSSetState(rs_no_cull.get());
    context->RSSetScissorRects(1, &scissor);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [scissor disabled]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [scissor disabled]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t inside_disabled =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, kHeight / 2);
    const uint32_t outside_disabled =
        aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth - 5, kHeight / 2);

    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_scissor_disabled.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: scissor-disabled BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_scissor_disabled.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }

    context->Unmap(staging.get(), 0);

    const uint8_t inside_disabled_a = (uint8_t)((inside_disabled >> 24) & 0xFFu);
    const uint8_t outside_disabled_a = (uint8_t)((outside_disabled >> 24) & 0xFFu);
    if ((inside_disabled & 0x00FFFFFFu) != expected_green_rgb ||
        (inside_disabled_a < kExpectedAlphaHalf - kAlphaTol ||
         inside_disabled_a > kExpectedAlphaHalf + kAlphaTol) ||
        (outside_disabled & 0x00FFFFFFu) != expected_green_rgb ||
        (outside_disabled_a < kExpectedAlphaHalf - kAlphaTol ||
         outside_disabled_a > kExpectedAlphaHalf + kAlphaTol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "scissor disable failed: inside(5,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a~%u), "
          "outside(%d,%d)=0x%08lX (a=%u) expected ~(rgb=0x%06lX a~%u)",
          kHeight / 2,
          (unsigned long)inside_disabled,
          (unsigned)inside_disabled_a,
          (unsigned long)expected_green_rgb,
          (unsigned)kExpectedAlphaHalf,
          kWidth - 5,
          kHeight / 2,
          (unsigned long)outside_disabled,
          (unsigned)outside_disabled_a,
          (unsigned long)expected_green_rgb,
          (unsigned)kExpectedAlphaHalf);
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

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [cull culled]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [cull culled]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t center_culled = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_culled.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(
              bmp_path,
              kWidth,
              kHeight,
              map.pData,
              (int)map.RowPitch,
              &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(culled) BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_cull_culled.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_red = 0xFFFF0000u;
    const uint8_t center_culled_a = (uint8_t)((center_culled >> 24) & 0xFFu);
    if ((center_culled & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected culled): center(%d,%d)=0x%08lX expected ~0x%08lX",
                           cx,
                           cy,
                           (unsigned long)center_culled,
                           (unsigned long)expected_red);
    }
    if (center_culled_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected culled): center(%d,%d) alpha mismatch: got %u expected 255",
                           cx,
                           cy,
                           (unsigned)center_culled_a);
    }

    // Next: CullMode = NONE should draw regardless of winding/front-face config.
    context->RSSetState(rs_no_cull.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [cull none]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [cull none]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_no_cull = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_none.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(none) BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_cull_none.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0xFF00FF00u;
    const uint8_t center_no_cull_a = (uint8_t)((center_no_cull >> 24) & 0xFFu);
    if ((center_no_cull & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "cull failed (expected visible with CullMode=NONE): center(%d,%d)=0x%08lX expected ~0x%08lX",
          cx,
          cy,
          (unsigned long)center_no_cull,
          (unsigned long)expected_green);
    }
    if (center_no_cull_a < kExpectedAlphaHalf - kAlphaTol || center_no_cull_a > kExpectedAlphaHalf + kAlphaTol) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "cull failed (expected visible with CullMode=NONE): center(%d,%d) alpha mismatch: got %u expected ~%u",
          cx,
          cy,
          (unsigned)center_no_cull_a,
          (unsigned)kExpectedAlphaHalf);
    }

    // Second: FrontCounterClockwise=TRUE, same CCW triangle should render (center becomes green).
    context->RSSetState(rs_cull_front_ccw.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [cull drawn]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [cull drawn]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_drawn = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_drawn.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(
              bmp_path,
              kWidth,
              kHeight,
              map.pData,
              (int)map.RowPitch,
              &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(drawn) BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_cull_drawn.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    if ((center_drawn & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected visible): center(%d,%d)=0x%08lX expected ~0x%08lX",
                           cx,
                           cy,
                           (unsigned long)center_drawn,
                           (unsigned long)expected_green);
    }
    const uint8_t center_drawn_a = (uint8_t)((center_drawn >> 24) & 0xFFu);
    if (center_drawn_a < kExpectedAlphaHalf - kAlphaTol || center_drawn_a > kExpectedAlphaHalf + kAlphaTol) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected visible): center(%d,%d) alpha mismatch: got %u expected ~%u",
                           cx,
                           cy,
                           (unsigned)center_drawn_a,
                           (unsigned)kExpectedAlphaHalf);
    }

    // Finally: RSSetState(NULL) should restore the default rasterizer state, which culls backfaces with
    // FrontCounterClockwise=FALSE (CW is front). Our CCW triangle should be culled (center remains red).
    context->RSSetState(NULL);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [cull NULL state]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [cull NULL state]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_null = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_cull_null_state.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: cull(NULL state) BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_cull_null_state.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    if ((center_null & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull NULL state failed: center(%d,%d)=0x%08lX expected ~0x%08lX",
                           cx,
                           cy,
                           (unsigned long)center_null,
                           (unsigned long)expected_red);
    }
    const uint8_t center_null_a = (uint8_t)((center_null >> 24) & 0xFFu);
    if (center_null_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull NULL state failed: center(%d,%d) alpha mismatch: got %u expected 255",
                           cx,
                           cy,
                           (unsigned)center_null_a);
    }
  }

  // Subtest 3: Depth clipping toggle (DepthClipEnable).
  {
    context->VSSetShader(vs_depth_clip.get(), NULL, 0);
    context->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    context->RSSetScissorRects(1, &full_rect);

    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

    // With depth clipping enabled, the primitive is outside the 0<=z<=w clip volume and should be discarded.
    context->RSSetState(rs_no_cull.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(
          &reporter, kTestName, "Map(staging) [depth clip enabled]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [depth clip enabled]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t center_clipped = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_depth_clip_enabled.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: depth-clip-enabled BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_depth_clip_enabled.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_red = 0xFFFF0000u;
    const uint8_t center_clipped_a = (uint8_t)((center_clipped >> 24) & 0xFFu);
    if ((center_clipped & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip failed (expected clipped): center(%d,%d)=0x%08lX expected ~0x%08lX",
          cx,
          cy,
          (unsigned long)center_clipped,
          (unsigned long)expected_red);
    }
    if (center_clipped_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip failed (expected clipped): center(%d,%d) alpha mismatch: got %u expected 255",
          cx,
          cy,
          (unsigned)center_clipped_a);
    }

    // With depth clipping disabled, the primitive should rasterize even though z is out of range.
    context->RSSetState(rs_no_cull_no_depth_clip.get());
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(
          &reporter, kTestName, "Map(staging) [depth clip disabled]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [depth clip disabled]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_unclipped = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_depth_clip_disabled.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: depth-clip-disabled BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_depth_clip_disabled.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0xFF00FF00u;
    const uint8_t center_unclipped_a = (uint8_t)((center_unclipped >> 24) & 0xFFu);
    if ((center_unclipped & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip failed (expected visible when disabled): center(%d,%d)=0x%08lX expected ~0x%08lX",
          cx,
          cy,
          (unsigned long)center_unclipped,
          (unsigned long)expected_green);
    }
    if (center_unclipped_a < kExpectedAlphaHalf - kAlphaTol ||
        center_unclipped_a > kExpectedAlphaHalf + kAlphaTol) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip failed (expected visible when disabled): center(%d,%d) alpha mismatch: got %u expected ~%u",
          cx,
          cy,
          (unsigned)center_unclipped_a,
          (unsigned)kExpectedAlphaHalf);
    }

    // RSSetState(NULL) should restore the default rasterizer state, where DepthClipEnable is TRUE.
    // The primitive should be clipped again.
    context->RSSetState(NULL);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->CopyResource(staging.get(), rt_tex.get());
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(
          &reporter, kTestName, "Map(staging) [depth clip NULL state]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [depth clip NULL state]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_null = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_depth_clip_null_state.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: depth-clip-NULL-state BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_depth_clip_null_state.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    if ((center_null & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip NULL state failed (expected clipped): center(%d,%d)=0x%08lX expected ~0x%08lX",
          cx,
          cy,
          (unsigned long)center_null,
          (unsigned long)expected_red);
    }
    const uint8_t depth_null_a = (uint8_t)((center_null >> 24) & 0xFFu);
    if (depth_null_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "depth clip NULL state failed (expected clipped): center(%d,%d) alpha mismatch: got %u expected 255",
          cx,
          cy,
          (unsigned)depth_null_a);
    }

    context->VSSetShader(vs.get(), NULL, 0);
  }

  // Subtest 4: Blend (green with alpha=0.5 over red should yield ~yellow).
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

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [blend]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [blend]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);

    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_blend.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: blend BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_blend.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }

    context->Unmap(staging.get(), 0);

    const uint8_t b = (uint8_t)(center & 0xFFu);
    const uint8_t g = (uint8_t)((center >> 8) & 0xFFu);
    const uint8_t r = (uint8_t)((center >> 16) & 0xFFu);
    const uint8_t a = (uint8_t)((center >> 24) & 0xFFu);

    const uint8_t exp_r = 0x80;
    const uint8_t exp_g = 0x80;
    const uint8_t exp_b = 0x00;
    const uint8_t exp_a = 0x80;
    const uint8_t tol = 2;

    if ((r < exp_r - tol || r > exp_r + tol) || (g < exp_g - tol || g > exp_g + tol) ||
        (b < exp_b - tol || b > exp_b + tol) || (a < exp_a - tol || a > exp_a + tol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "blend failed: center(%d,%d)=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=%u g=%u b=%u a=%u) tol=%u",
          cx,
          cy,
          (unsigned long)center,
          (unsigned)r,
          (unsigned)g,
          (unsigned)b,
          (unsigned)a,
          (unsigned)exp_r,
          (unsigned)exp_g,
          (unsigned)exp_b,
          (unsigned)exp_a,
          (unsigned)tol);
    }

    // Verify that disabling blending returns to unblended rendering.
    context->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [blend disabled]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [blend disabled]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_disabled = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_blend_disabled.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: blend-disabled BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_blend_disabled.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint8_t b2 = (uint8_t)(center_disabled & 0xFFu);
    const uint8_t g2 = (uint8_t)((center_disabled >> 8) & 0xFFu);
    const uint8_t r2 = (uint8_t)((center_disabled >> 16) & 0xFFu);
    const uint8_t a2 = (uint8_t)((center_disabled >> 24) & 0xFFu);
    const uint8_t exp_a2 = 0x80;
    if (r2 != 0 || g2 != 0xFFu || b2 != 0 || (a2 < exp_a2 - tol || a2 > exp_a2 + tol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "blend disable failed: center(%d,%d)=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=0 g=255 b=0 a=%u) tol=%u",
          cx,
          cy,
          (unsigned long)center_disabled,
          (unsigned)r2,
          (unsigned)g2,
          (unsigned)b2,
          (unsigned)a2,
          (unsigned)exp_a2,
          (unsigned)tol);
    }

    // Verify that RenderTargetWriteMask in the bound blend state is respected.
    // Clear to red, then draw green while only the G channel is writable => red channel should remain 0xFF,
    // yielding yellow (0xFFFF_FF00).
    context->OMSetBlendState(green_write_mask.get(), blend_factor, 0xFFFFFFFFu);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [write mask]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [write mask]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_mask = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_write_mask.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: write-mask BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_write_mask.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }

    context->Unmap(staging.get(), 0);

    const uint32_t expected_yellow = 0xFFFFFF00u;
    if ((center_mask & 0x00FFFFFFu) != (expected_yellow & 0x00FFFFFFu)) {
      const uint8_t b3 = (uint8_t)(center_mask & 0xFFu);
      const uint8_t g3 = (uint8_t)((center_mask >> 8) & 0xFFu);
      const uint8_t r3 = (uint8_t)((center_mask >> 16) & 0xFFu);
      const uint8_t a3 = (uint8_t)((center_mask >> 24) & 0xFFu);
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "write mask failed: center(%d,%d)=0x%08lX (r=%u g=%u b=%u a=%u) expected ~0x%08lX",
          cx,
          cy,
          (unsigned long)center_mask,
          (unsigned)r3,
          (unsigned)g3,
          (unsigned)b3,
          (unsigned)a3,
          (unsigned long)expected_yellow);
    }
    const uint8_t a3 = (uint8_t)((center_mask >> 24) & 0xFFu);
    if (a3 != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("write mask failed: expected alpha preserved (0xFF), got a=%u (center=0x%08lX)",
                           (unsigned)a3,
                           (unsigned long)center_mask);
    }

    // Verify that OMSetBlendState's blend-factor parameter is honored.
    const FLOAT bf25[4] = {0.25f, 0.25f, 0.25f, 0.25f};
    context->OMSetBlendState(blend_factor_state.get(), bf25, 0xFFFFFFFFu);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [blend factor]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [blend factor]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_bf = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_blend_factor.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: blend-factor BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_blend_factor.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint8_t b4 = (uint8_t)(center_bf & 0xFFu);
    const uint8_t g4 = (uint8_t)((center_bf >> 8) & 0xFFu);
    const uint8_t r4 = (uint8_t)((center_bf >> 16) & 0xFFu);
    const uint8_t a4 = (uint8_t)((center_bf >> 24) & 0xFFu);
    // With BF=0.25, output should be ~0.75*red + 0.25*green => R~0xBF, G~0x40, B~0.
    const uint8_t exp_r2 = 0xBF;
    const uint8_t exp_g2 = 0x40;
    const uint8_t exp_b2 = 0x00;
    const uint8_t exp_a3 = 0x80;
    const uint8_t tol2 = 2;
    if ((r4 < exp_r2 - tol2 || r4 > exp_r2 + tol2) || (g4 < exp_g2 - tol2 || g4 > exp_g2 + tol2) ||
        (b4 < exp_b2 - tol2 || b4 > exp_b2 + tol2) || (a4 < exp_a3 - tol2 || a4 > exp_a3 + tol2)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "blend factor failed: center(%d,%d)=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=%u g=%u b=%u a=%u) tol=%u",
          cx,
          cy,
          (unsigned long)center_bf,
          (unsigned)r4,
          (unsigned)g4,
          (unsigned)b4,
          (unsigned)a4,
          (unsigned)exp_r2,
          (unsigned)exp_g2,
          (unsigned)exp_b2,
          (unsigned)exp_a3,
          (unsigned)tol2);
    }

    // Verify OMSetBlendState's SampleMask parameter is honored.
    // With a 1-sample render target, a sample mask of 0 should discard all color writes.
    context->OMSetBlendState(NULL, blend_factor, 0u);
    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter,
                                        kTestName,
                                        "Map(staging) [sample mask]",
                                        hr,
                                        device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [sample mask]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center_sm0 = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_sample_mask_0.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: sample-mask BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_sample_mask_0.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_red = 0xFFFF0000u;
    const uint8_t a5 = (uint8_t)((center_sm0 >> 24) & 0xFFu);
    if (a5 != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("sample mask failed: expected alpha preserved (0xFF), got a=%u (center=0x%08lX)",
                           (unsigned)a5,
                           (unsigned long)center_sm0);
    }
    if ((center_sm0 & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("sample mask failed: center(%d,%d)=0x%08lX expected ~0x%08lX",
                           cx,
                           cy,
                           (unsigned long)center_sm0,
                           (unsigned long)expected_red);
    }
  }

  // Subtest 5: ClearState resets RS/OM state (scissor/cull/depth-clip/blend).
  //
  // This is specifically intended to validate the ClearState path in the UMD: if
  // ClearState does not emit default RS/OM state packets, host-side state can
  // "stick" across ClearState, causing clipped/incorrect output.
  {
    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const D3D11_RECT small_scissor = {16, 16, 48, 48};

    // Set a non-default state (scissor enabled + depth clip disabled + blending enabled), then draw once so the
    // state is definitely active on the host before we call ClearState.
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->RSSetViewports(1, &vp);
    context->IASetInputLayout(input_layout.get());
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->VSSetShader(vs.get(), NULL, 0);
    context->PSSetShader(ps.get(), NULL, 0);
    context->OMSetBlendState(alpha_blend.get(), blend_factor, 0xFFFFFFFFu);
    context->RSSetState(rs_scissor_no_depth_clip.get());
    context->RSSetScissorRects(1, &small_scissor);

    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs0[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs0, &stride, &offset);

    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    // Now ClearState and re-bind only the minimum required state for a draw. Do
    // NOT explicitly set rasterizer/blend state; output should match defaults
    // (no scissor, blending disabled, cull back, depth clip enabled).
    context->ClearState();

    context->OMSetRenderTargets(1, rtvs, NULL);
    context->RSSetViewports(1, &vp);
    context->IASetInputLayout(input_layout.get());
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->VSSetShader(vs.get(), NULL, 0);
    context->PSSetShader(ps.get(), NULL, 0);
    ID3D11Buffer* vbs1[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs1, &stride, &offset);

    context->ClearRenderTargetView(rtv.get(), clear_red);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    D3D11_MAPPED_SUBRESOURCE map;
    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [ClearState]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [ClearState]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);

    if (dump) {
      const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_clear_state.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: ClearState BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_clear_state.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_green = 0x8000FF00u;
    if ((center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (corner & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState reset failed: center=0x%08lX corner=0x%08lX expected ~0x%08lX",
                           (unsigned long)center,
                           (unsigned long)corner,
                           (unsigned long)expected_green);
    }
    const uint8_t center_a = (uint8_t)((center >> 24) & 0xFFu);
    const uint8_t corner_a = (uint8_t)((corner >> 24) & 0xFFu);
    if ((center_a < kExpectedAlphaHalf - kAlphaTol || center_a > kExpectedAlphaHalf + kAlphaTol) ||
        (corner_a < kExpectedAlphaHalf - kAlphaTol || corner_a > kExpectedAlphaHalf + kAlphaTol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState alpha mismatch: center_a=%u corner_a=%u expected ~%u",
                           (unsigned)center_a,
                           (unsigned)corner_a,
                           (unsigned)kExpectedAlphaHalf);
    }

    // Verify default culling: the CCW triangle should be culled, leaving clear red intact.
    context->ClearRenderTargetView(rtv.get(), clear_red);
    ID3D11Buffer* vbs2[] = {vb_cull.get()};
    context->IASetVertexBuffers(0, 1, vbs2, &stride, &offset);
    context->VSSetShader(vs.get(), NULL, 0);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [ClearState cull]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [ClearState cull]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t cull_center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_clear_state_cull.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: ClearState-cull BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_clear_state_cull.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    const uint32_t expected_red = 0xFFFF0000u;
    if ((cull_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) || ((cull_center >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState cull reset failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)cull_center,
                           (unsigned long)expected_red);
    }

    // Verify default depth clip: VS outputs Z=-0.5, so with DepthClipEnable=TRUE it should be clipped.
    context->ClearRenderTargetView(rtv.get(), clear_red);
    ID3D11Buffer* vbs3[] = {vb_fs.get()};
    context->IASetVertexBuffers(0, 1, vbs3, &stride, &offset);
    context->VSSetShader(vs_depth_clip.get(), NULL, 0);
    context->Draw(3, 0);

    context->OMSetRenderTargets(0, NULL, NULL);
    context->CopyResource(staging.get(), rt_tex.get());
    context->OMSetRenderTargets(1, rtvs, NULL);
    context->Flush();

    ZeroMemory(&map, sizeof(map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging) [ClearState depth clip]", hr, device.get());
    }
    map_rc = ValidateStagingMap("Map(staging) [ClearState depth clip]", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const uint32_t depth_clip_center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    if (dump) {
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(dir, L"d3d11_rs_om_state_sanity_clear_state_depth_clip.bmp");
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: ClearState-depth-clip BMP dump failed: %s", kTestName, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d11_rs_om_state_sanity_clear_state_depth_clip.bin",
                      map.pData,
                      map.RowPitch,
                      kWidth,
                      kHeight);
    }
    context->Unmap(staging.get(), 0);

    if ((depth_clip_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) ||
        ((depth_clip_center >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState depth-clip reset failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)depth_clip_center,
                           (unsigned long)expected_red);
    }

    context->VSSetShader(vs.get(), NULL, 0);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11RSOMStateSanity(argc, argv);
}
