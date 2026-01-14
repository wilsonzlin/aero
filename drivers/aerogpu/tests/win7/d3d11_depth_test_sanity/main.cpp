#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[3];
  float color[4];
};

static void PrintD3D11DeviceRemovedReasonIfFailed(const char* test_name, ID3D11Device* device) {
  if (!device) {
    return;
  }
  HRESULT reason = device->GetDeviceRemovedReason();
  if (FAILED(reason)) {
    aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                               test_name,
                               aerogpu_test::HresultToString(reason).c_str());
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

static const char kDepthHlsl[] = R"(struct VSIn {
  float3 pos : POSITION;
  float4 color : COLOR0;
};

struct VSOut {
  float4 pos : SV_Position;
  float4 color : COLOR0;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xyz, 1.0f);
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
  PrintD3D11DeviceRemovedReasonIfFailed(test_name, device);
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
}

static int RunD3D11DepthTestSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_depth_test_sanity";
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

  D3D_FEATURE_LEVEL feature_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                       D3D_FEATURE_LEVEL_10_1,
                                       D3D_FEATURE_LEVEL_10_0,
                                       D3D_FEATURE_LEVEL_9_3,
                                       D3D_FEATURE_LEVEL_9_2,
                                       D3D_FEATURE_LEVEL_9_1};

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
  }

  // Compile shaders at runtime (no fxc.exe build-time dependency).
  const std::wstring dir = aerogpu_test::GetModuleDir();

  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(kDepthHlsl,
                                           strlen(kDepthHlsl),
                                           "d3d11_depth_test_sanity.hlsl",
                                           "vs_main",
                                           "vs_4_0",
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(kDepthHlsl,
                                           strlen(kDepthHlsl),
                                           "d3d11_depth_test_sanity.hlsl",
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

  ComPtr<ID3D11PixelShader> ps;
  hr = device->CreatePixelShader(&ps_bytes[0], ps_bytes.size(), NULL, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  D3D11_INPUT_ELEMENT_DESC il[] = {
      {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(
      il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateInputLayout", hr);
  }

  const int kWidth = 64;
  const int kHeight = 64;

  D3D11_TEXTURE2D_DESC rt_desc;
  ZeroMemory(&rt_desc, sizeof(rt_desc));
  rt_desc.Width = kWidth;
  rt_desc.Height = kHeight;
  rt_desc.MipLevels = 1;
  rt_desc.ArraySize = 1;
  rt_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  rt_desc.SampleDesc.Count = 1;
  rt_desc.SampleDesc.Quality = 0;
  rt_desc.Usage = D3D11_USAGE_DEFAULT;
  rt_desc.BindFlags = D3D11_BIND_RENDER_TARGET;
  rt_desc.CPUAccessFlags = 0;
  rt_desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> rt_tex;
  hr = device->CreateTexture2D(&rt_desc, NULL, rt_tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(render target)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(rt_tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView", hr);
  }

  DXGI_FORMAT depth_format = DXGI_FORMAT_D24_UNORM_S8_UINT;
  const char* depth_format_label = "D24_UNORM_S8_UINT";
  ComPtr<ID3D11Texture2D> depth_tex;
  ComPtr<ID3D11DepthStencilView> dsv;
  HRESULT hr_d24_tex = S_OK;
  HRESULT hr_d24_dsv = S_OK;
  HRESULT hr_d32_tex = S_OK;
  HRESULT hr_d32_dsv = S_OK;

  D3D11_TEXTURE2D_DESC depth_desc;
  ZeroMemory(&depth_desc, sizeof(depth_desc));
  depth_desc.Width = kWidth;
  depth_desc.Height = kHeight;
  depth_desc.MipLevels = 1;
  depth_desc.ArraySize = 1;
  depth_desc.Format = depth_format;
  depth_desc.SampleDesc.Count = 1;
  depth_desc.SampleDesc.Quality = 0;
  depth_desc.Usage = D3D11_USAGE_DEFAULT;
  depth_desc.BindFlags = D3D11_BIND_DEPTH_STENCIL;
  depth_desc.CPUAccessFlags = 0;
  depth_desc.MiscFlags = 0;

  hr = device->CreateTexture2D(&depth_desc, NULL, depth_tex.put());
  if (FAILED(hr)) {
    hr_d24_tex = hr;
  } else {
    hr = device->CreateDepthStencilView(depth_tex.get(), NULL, dsv.put());
    if (FAILED(hr)) {
      hr_d24_dsv = hr;
    }
  }

  if (!depth_tex || !dsv) {
    // Fall back to D32_FLOAT when D24S8 isn't supported (common for early bring-up).
    depth_tex.reset();
    dsv.reset();
    depth_format = DXGI_FORMAT_D32_FLOAT;
    depth_format_label = "D32_FLOAT";
    depth_desc.Format = depth_format;

    hr = device->CreateTexture2D(&depth_desc, NULL, depth_tex.put());
    if (FAILED(hr)) {
      hr_d32_tex = hr;
      return reporter.Fail("CreateTexture2D(depth) failed: %s => %s; fallback %s => %s",
                           "D24_UNORM_S8_UINT",
                           aerogpu_test::HresultToString(hr_d24_tex).c_str(),
                           "D32_FLOAT",
                           aerogpu_test::HresultToString(hr_d32_tex).c_str());
    }
    hr = device->CreateDepthStencilView(depth_tex.get(), NULL, dsv.put());
    if (FAILED(hr)) {
      hr_d32_dsv = hr;
      return reporter.Fail("CreateDepthStencilView(depth) failed: %s => %s; fallback %s => %s",
                           "D24_UNORM_S8_UINT",
                           aerogpu_test::HresultToString(hr_d24_dsv).c_str(),
                           "D32_FLOAT",
                           aerogpu_test::HresultToString(hr_d32_dsv).c_str());
    }
    aerogpu_test::PrintfStdout("INFO: %s: depth format %s unavailable (%s / %s); using %s",
                               kTestName,
                               "D24_UNORM_S8_UINT",
                               aerogpu_test::HresultToString(hr_d24_tex).c_str(),
                               aerogpu_test::HresultToString(hr_d24_dsv).c_str(),
                               depth_format_label);
  }

  D3D11_DEPTH_STENCIL_DESC dss_desc;
  ZeroMemory(&dss_desc, sizeof(dss_desc));
  dss_desc.DepthEnable = TRUE;
  dss_desc.DepthWriteMask = D3D11_DEPTH_WRITE_MASK_ALL;
  dss_desc.DepthFunc = D3D11_COMPARISON_LESS;
  dss_desc.StencilEnable = FALSE;
  dss_desc.StencilReadMask = D3D11_DEFAULT_STENCIL_READ_MASK;
  dss_desc.StencilWriteMask = D3D11_DEFAULT_STENCIL_WRITE_MASK;
  dss_desc.FrontFace.StencilFailOp = D3D11_STENCIL_OP_KEEP;
  dss_desc.FrontFace.StencilDepthFailOp = D3D11_STENCIL_OP_KEEP;
  dss_desc.FrontFace.StencilPassOp = D3D11_STENCIL_OP_KEEP;
  dss_desc.FrontFace.StencilFunc = D3D11_COMPARISON_ALWAYS;
  dss_desc.BackFace = dss_desc.FrontFace;

  ComPtr<ID3D11DepthStencilState> dss;
  hr = device->CreateDepthStencilState(&dss_desc, dss.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateDepthStencilState", hr);
  }

  ID3D11RenderTargetView* rtvs[] = {rtv.get()};
  context->OMSetRenderTargets(1, rtvs, dsv.get());
  context->OMSetDepthStencilState(dss.get(), 0);

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

  Vertex verts[6];
  // Near triangle (blue) at z=0.2.
  verts[0].pos[0] = -0.5f;
  verts[0].pos[1] = -0.5f;
  verts[0].pos[2] = 0.2f;
  verts[1].pos[0] = 0.0f;
  verts[1].pos[1] = 0.5f;
  verts[1].pos[2] = 0.2f;
  verts[2].pos[0] = 0.5f;
  verts[2].pos[1] = -0.5f;
  verts[2].pos[2] = 0.2f;
  for (int i = 0; i < 3; ++i) {
    verts[i].color[0] = 0.0f;
    verts[i].color[1] = 0.0f;
    verts[i].color[2] = 1.0f;
    verts[i].color[3] = 1.0f;
  }

  // Far triangle (green) at z=0.8. Use a fullscreen triangle so the final image contains both
  // colors simultaneously (blue in the overlap, green elsewhere) when depth testing works.
  verts[3].pos[0] = -1.0f;
  verts[3].pos[1] = -1.0f;
  verts[3].pos[2] = 0.8f;
  verts[4].pos[0] = -1.0f;
  verts[4].pos[1] = 3.0f;
  verts[4].pos[2] = 0.8f;
  verts[5].pos[0] = 3.0f;
  verts[5].pos[1] = -1.0f;
  verts[5].pos[2] = 0.8f;
  for (int i = 3; i < 6; ++i) {
    verts[i].color[0] = 0.0f;
    verts[i].color[1] = 1.0f;
    verts[i].color[2] = 0.0f;
    verts[i].color[3] = 1.0f;
  }

  D3D11_BUFFER_DESC vb_desc;
  ZeroMemory(&vb_desc, sizeof(vb_desc));
  vb_desc.ByteWidth = sizeof(verts);
  vb_desc.Usage = D3D11_USAGE_DEFAULT;
  vb_desc.BindFlags = D3D11_BIND_VERTEX_BUFFER;

  D3D11_SUBRESOURCE_DATA vb_init;
  ZeroMemory(&vb_init, sizeof(vb_init));
  vb_init.pSysMem = verts;

  ComPtr<ID3D11Buffer> vb;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vertex)", hr);
  }

  UINT stride = sizeof(Vertex);
  UINT offset = 0;
  ID3D11Buffer* vbs[] = {vb.get()};
  context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

  context->VSSetShader(vs.get(), NULL, 0);
  context->PSSetShader(ps.get(), NULL, 0);

  const FLOAT clear_rgba[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  context->ClearRenderTargetView(rtv.get(), clear_rgba);

  UINT clear_flags = D3D11_CLEAR_DEPTH;
  if (depth_format == DXGI_FORMAT_D24_UNORM_S8_UINT) {
    clear_flags |= D3D11_CLEAR_STENCIL;
  }

  // Validate ClearDepthStencilView + depth testing deterministically:
  // 1) Clear depth to 0.0, then draw a far fullscreen triangle (z=0.8) in the LEFT viewport.
  //    It must be rejected (color stays red).
  // 2) Clear depth to 1.0, then in the RIGHT viewport draw:
  //    - near triangle (blue, z=0.2) first
  //    - far fullscreen triangle (green, z=0.8) second
  //    Result should be blue in the overlap (center) and green elsewhere (e.g. bottom-right).
  context->ClearDepthStencilView(dsv.get(), clear_flags, 0.0f, 0);

  // Left half.
  vp.TopLeftX = 0.0f;
  vp.TopLeftY = 0.0f;
  vp.Width = (FLOAT)(kWidth / 2);
  vp.Height = (FLOAT)kHeight;
  context->RSSetViewports(1, &vp);
  // Far triangle (green) should be rejected because depth was cleared to 0.0.
  context->Draw(3, 3);

  // Right half.
  context->ClearDepthStencilView(dsv.get(), clear_flags, 1.0f, 0);
  vp.TopLeftX = (FLOAT)(kWidth / 2);
  vp.TopLeftY = 0.0f;
  vp.Width = (FLOAT)(kWidth / 2);
  vp.Height = (FLOAT)kHeight;
  context->RSSetViewports(1, &vp);
  // Near triangle first.
  context->Draw(3, 0);
  // Far triangle second (should draw outside the near triangle only).
  context->Draw(3, 3);

  // Explicitly unbind to exercise the "bind NULL to clear" path (common during ClearState).
  context->OMSetRenderTargets(0, NULL, NULL);
  context->OMSetDepthStencilState(NULL, 0);
  ID3D11Buffer* null_vb = NULL;
  const UINT zero = 0;
  context->IASetVertexBuffers(0, 1, &null_vb, &zero, &zero);
  context->IASetInputLayout(NULL);
  context->VSSetShader(NULL, NULL, 0);
  context->PSSetShader(NULL, NULL, 0);

  // Read back the result via a staging texture.
  D3D11_TEXTURE2D_DESC st_desc = rt_desc;
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
  const auto MapStagingRead = [&](const char* label, D3D11_MAPPED_SUBRESOURCE* out_map) -> int {
    if (!out_map) {
      return reporter.Fail("%s: NULL out_map", label);
    }
    ZeroMemory(out_map, sizeof(*out_map));
    hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, out_map);
    if (FAILED(hr)) {
      return FailD3D11WithRemovedReason(&reporter, kTestName, label, hr, device.get());
    }
    if (!out_map->pData) {
      context->Unmap(staging.get(), 0);
      return reporter.Fail("%s returned NULL pData", label);
    }
    if (out_map->RowPitch < min_row_pitch) {
      context->Unmap(staging.get(), 0);
      return reporter.Fail("%s returned unexpected RowPitch=%ld (expected >= %d)",
                           label,
                           (long)out_map->RowPitch,
                           kWidth * 4);
    }
    return 0;
  };

  context->CopyResource(staging.get(), rt_tex.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  int map_rc = MapStagingRead("Map(staging)", &map);
  if (map_rc != 0) {
    return map_rc;
  }

  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 0, 0);
  const uint32_t left_center =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth / 4, kHeight / 2);
  const uint32_t right_center =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 3 * kWidth / 4, kHeight / 2);
  const uint32_t right_corner =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth - 4, kHeight - 4);
  const uint32_t expected_red = 0xFFFF0000u;
  const uint32_t expected_blue = 0xFF0000FFu;
  const uint32_t expected_green = 0xFF00FF00u;

  if (dump) {
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_depth_test_sanity.bmp");
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(
            bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }

    // Also dump a tightly-packed raw BGRA32 buffer for easier machine inspection.
    std::vector<uint8_t> tight((size_t)kWidth * (size_t)kHeight * 4u, 0);
    for (int y = 0; y < kHeight; ++y) {
      const uint8_t* src_row = (const uint8_t*)map.pData + (size_t)y * (size_t)map.RowPitch;
      memcpy(&tight[(size_t)y * (size_t)kWidth * 4u], src_row, (size_t)kWidth * 4u);
    }
    DumpBytesToFile(
        kTestName, &reporter, L"d3d11_depth_test_sanity.bin", &tight[0], (UINT)tight.size());
  }

  context->Unmap(staging.get(), 0);

  if ((corner & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) ||
      (left_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) ||
      (right_center & 0x00FFFFFFu) != (expected_blue & 0x00FFFFFFu) ||
      (right_corner & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
    PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
    return reporter.Fail(
        "pixel mismatch (%s): corner=0x%08lX expected 0x%08lX; left_center=0x%08lX expected 0x%08lX; "
        "right_center=0x%08lX expected 0x%08lX; right_corner=0x%08lX expected 0x%08lX",
        depth_format_label,
        (unsigned long)corner,
        (unsigned long)expected_red,
        (unsigned long)left_center,
        (unsigned long)expected_red,
        (unsigned long)right_center,
        (unsigned long)expected_blue,
        (unsigned long)right_corner,
        (unsigned long)expected_green);
  }

  // Subtest: ClearState resets depth-stencil state.
  //
  // Specifically validate that a non-default DepthFunc (GREATER) does not "stick"
  // across ClearState. This helps catch missing default OM state emission in the UMD.
  {
    D3D11_DEPTH_STENCIL_DESC dss_greater_desc = dss_desc;
    dss_greater_desc.DepthFunc = D3D11_COMPARISON_GREATER;
    ComPtr<ID3D11DepthStencilState> dss_greater;
    hr = device->CreateDepthStencilState(&dss_greater_desc, dss_greater.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateDepthStencilState(GREATER)", hr);
    }

    const D3D11_VIEWPORT vp_full = {0.0f, 0.0f, (FLOAT)kWidth, (FLOAT)kHeight, 0.0f, 1.0f};
    UINT stride = sizeof(Vertex);
    UINT offset = 0;
    ID3D11Buffer* vbs[] = {vb.get()};

    const auto ReadbackCenter = [&](const char* label,
                                    const wchar_t* bmp_name,
                                    const wchar_t* bin_name,
                                    uint32_t* out_pixel) -> int {
      if (!out_pixel) {
        return reporter.Fail("%s: NULL out_pixel", label);
      }
      context->OMSetRenderTargets(0, NULL, NULL);
      context->CopyResource(staging.get(), rt_tex.get());
      context->Flush();

      D3D11_MAPPED_SUBRESOURCE map2;
      int rc = MapStagingRead(label, &map2);
      if (rc != 0) {
        return rc;
      }
      *out_pixel =
          aerogpu_test::ReadPixelBGRA(map2.pData, (int)map2.RowPitch, kWidth / 2, kHeight / 2);

      if (dump) {
        const std::wstring bmp_path = aerogpu_test::JoinPath(dir, bmp_name);
        std::string err;
        if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map2.pData, (int)map2.RowPitch, &err)) {
          aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed (%ls): %s", kTestName, bmp_name, err.c_str());
        } else {
          reporter.AddArtifactPathW(bmp_path);
        }

        std::vector<uint8_t> tight((size_t)kWidth * (size_t)kHeight * 4u, 0);
        for (int y = 0; y < kHeight; ++y) {
          const uint8_t* src_row = (const uint8_t*)map2.pData + (size_t)y * (size_t)map2.RowPitch;
          memcpy(&tight[(size_t)y * (size_t)kWidth * 4u], src_row, (size_t)kWidth * 4u);
        }
        DumpBytesToFile(kTestName, &reporter, bin_name, &tight[0], (UINT)tight.size());
      }

      context->Unmap(staging.get(), 0);
      return 0;
    };

    // Dirty the host state: set DepthFunc=GREATER, clear depth to 0.0, and draw a fullscreen triangle at z=0.8.
    // With GREATER, this must PASS (0.8 > 0.0), producing green.
    context->OMSetRenderTargets(1, rtvs, dsv.get());
    context->OMSetDepthStencilState(dss_greater.get(), 0);
    context->RSSetViewports(1, &vp_full);
    context->IASetInputLayout(input_layout.get());
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
    context->VSSetShader(vs.get(), NULL, 0);
    context->PSSetShader(ps.get(), NULL, 0);
    context->ClearRenderTargetView(rtv.get(), clear_rgba);
    context->ClearDepthStencilView(dsv.get(), clear_flags, 0.0f, 0);
    context->Draw(3, 3);

    uint32_t dirty_center = 0;
    map_rc = ReadbackCenter("Map(staging) [ClearState dirty GREATER]",
                            L"d3d11_depth_test_sanity_clear_state_dirty_greater.bmp",
                            L"d3d11_depth_test_sanity_clear_state_dirty_greater.bin",
                            &dirty_center);
    if (map_rc != 0) {
      return map_rc;
    }
    if ((dirty_center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
      return reporter.Fail("DepthFunc(GREATER) unexpected output: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)dirty_center,
                           (unsigned long)expected_green);
    }

    // ClearState and then draw again WITHOUT setting a depth-stencil state.
    // Defaults should apply (DepthEnable=TRUE, DepthFunc=LESS).
    context->ClearState();

    context->OMSetRenderTargets(1, rtvs, dsv.get());
    context->RSSetViewports(1, &vp_full);
    context->IASetInputLayout(input_layout.get());
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
    context->VSSetShader(vs.get(), NULL, 0);
    context->PSSetShader(ps.get(), NULL, 0);

    // With depth cleared to 0.0, DepthFunc=LESS should REJECT z=0.8 (0.8 < 0.0 is false), leaving red.
    context->ClearRenderTargetView(rtv.get(), clear_rgba);
    context->ClearDepthStencilView(dsv.get(), clear_flags, 0.0f, 0);
    context->Draw(3, 3);

    uint32_t reset_depth0_center = 0;
    map_rc = ReadbackCenter("Map(staging) [ClearState reset depth=0]",
                            L"d3d11_depth_test_sanity_clear_state_reset_depth0.bmp",
                            L"d3d11_depth_test_sanity_clear_state_reset_depth0.bin",
                            &reset_depth0_center);
    if (map_rc != 0) {
      return map_rc;
    }
    if ((reset_depth0_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) ||
        ((reset_depth0_center >> 24) & 0xFFu) != 0xFFu) {
      PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
      return reporter.Fail("ClearState depth-stencil reset failed (depth=0): center=0x%08lX expected ~0x%08lX",
                           (unsigned long)reset_depth0_center,
                           (unsigned long)expected_red);
    }

    // With depth cleared to 1.0, DepthFunc=LESS should ACCEPT z=0.8, producing green.
    context->OMSetRenderTargets(1, rtvs, dsv.get());
    context->ClearRenderTargetView(rtv.get(), clear_rgba);
    context->ClearDepthStencilView(dsv.get(), clear_flags, 1.0f, 0);
    context->Draw(3, 3);

    uint32_t reset_depth1_center = 0;
    map_rc = ReadbackCenter("Map(staging) [ClearState reset depth=1]",
                            L"d3d11_depth_test_sanity_clear_state_reset_depth1.bmp",
                            L"d3d11_depth_test_sanity_clear_state_reset_depth1.bin",
                            &reset_depth1_center);
    if (map_rc != 0) {
      return map_rc;
    }
    if ((reset_depth1_center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        ((reset_depth1_center >> 24) & 0xFFu) != 0xFFu) {
      PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
      return reporter.Fail("ClearState depth-stencil reset failed (depth=1): center=0x%08lX expected ~0x%08lX",
                           (unsigned long)reset_depth1_center,
                           (unsigned long)expected_green);
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11DepthTestSanity(argc, argv);
}
