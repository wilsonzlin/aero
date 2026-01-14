#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"

#include <d3d10_1.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[3];
  float color[4];
};

static const char kStateHlsl[] = R"(struct VSIn {
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

static void PrintDeviceRemovedReasonIfAny(const char* test_name, ID3D10Device* device) {
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

static int RunD3D101RSOMStateSanity(int argc, char** argv) {
  const char* kTestName = "d3d10_1_rs_om_state_sanity";
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

  ComPtr<ID3D10Device1> device;
  const UINT flags = D3D10_CREATE_DEVICE_BGRA_SUPPORT;
  D3D10_FEATURE_LEVEL1 feature_levels[] = {D3D10_FEATURE_LEVEL1_10_1, D3D10_FEATURE_LEVEL1_10_0};
  D3D10_FEATURE_LEVEL1 chosen_level = (D3D10_FEATURE_LEVEL1)0;
  HRESULT hr = E_FAIL;
  for (size_t i = 0; i < ARRAYSIZE(feature_levels); ++i) {
    chosen_level = feature_levels[i];
    hr = D3D10CreateDevice1(NULL,
                            D3D10_DRIVER_TYPE_HARDWARE,
                            NULL,
                            flags,
                            chosen_level,
                            D3D10_SDK_VERSION,
                            device.put());
    if (SUCCEEDED(hr)) {
      break;
    }
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D10CreateDevice1(HARDWARE)", hr);
  }

  // This test is specifically intended to exercise the D3D10.1 runtime path (d3d10_1.dll).
  if (!GetModuleHandleW(L"d3d10_1.dll")) {
    return reporter.Fail("d3d10_1.dll is not loaded");
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);
  const D3D10_FEATURE_LEVEL1 actual_level = device->GetFeatureLevel();
  if (actual_level != chosen_level) {
    return reporter.Fail("ID3D10Device1::GetFeatureLevel returned 0x%04X (expected 0x%04X)",
                         (unsigned)actual_level,
                         (unsigned)chosen_level);
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

    // This test is explicitly intended to cover the D3D10.1 UMD entrypoint path (`OpenAdapter10_2`).
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_10_2 = GetProcAddress(umd, "OpenAdapter10_2");
    if (!open_adapter_10_2) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_10_2 = GetProcAddress(umd, "_OpenAdapter10_2@4");
    }
    if (!open_adapter_10_2) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter10_2 (D3D10.1 entrypoint)");
    }
  }

  // Compile shaders at runtime (no fxc.exe build-time dependency).
  const std::wstring dir = aerogpu_test::GetModuleDir();
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(kStateHlsl,
                                           strlen(kStateHlsl),
                                           "d3d10_1_rs_om_state_sanity.hlsl",
                                           "vs_main",
                                           "vs_4_0",
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(kStateHlsl,
                                           strlen(kStateHlsl),
                                           "d3d10_1_rs_om_state_sanity.hlsl",
                                           "ps_main",
                                           "ps_4_0",
                                           &ps_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile pixel shader: %s", shader_err.c_str());
  }

  ComPtr<ID3D10VertexShader> vs;
  hr = device->CreateVertexShader(&vs_bytes[0], vs_bytes.size(), vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }
  ComPtr<ID3D10PixelShader> ps;
  hr = device->CreatePixelShader(&ps_bytes[0], ps_bytes.size(), ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  D3D10_INPUT_ELEMENT_DESC il[] = {
      {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D10_INPUT_PER_VERTEX_DATA, 0},
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 12, D3D10_INPUT_PER_VERTEX_DATA, 0},
  };
  ComPtr<ID3D10InputLayout> input_layout;
  hr = device->CreateInputLayout(il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateInputLayout", hr);
  }

  const int kWidth = 64;
  const int kHeight = 64;

  D3D10_TEXTURE2D_DESC rt_desc;
  ZeroMemory(&rt_desc, sizeof(rt_desc));
  rt_desc.Width = (UINT)kWidth;
  rt_desc.Height = (UINT)kHeight;
  rt_desc.MipLevels = 1;
  rt_desc.ArraySize = 1;
  rt_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  rt_desc.SampleDesc.Count = 1;
  rt_desc.SampleDesc.Quality = 0;
  rt_desc.Usage = D3D10_USAGE_DEFAULT;
  rt_desc.BindFlags = D3D10_BIND_RENDER_TARGET;
  rt_desc.CPUAccessFlags = 0;
  rt_desc.MiscFlags = 0;

  ComPtr<ID3D10Texture2D> rt_tex;
  hr = device->CreateTexture2D(&rt_desc, NULL, rt_tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(render target)", hr);
  }
  ComPtr<ID3D10RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(rt_tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView", hr);
  }

  D3D10_TEXTURE2D_DESC st_desc = rt_desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;
  st_desc.Usage = D3D10_USAGE_STAGING;
  ComPtr<ID3D10Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  const UINT min_row_pitch = (UINT)kWidth * 4u;
  const auto ValidateStagingMap = [&](const char* label, const D3D10_MAPPED_TEXTURE2D& map) -> int {
    if (!map.pData) {
      staging->Unmap(0);
      return reporter.Fail("%s returned NULL pData", label);
    }
    if (map.RowPitch < min_row_pitch) {
      staging->Unmap(0);
      return reporter.Fail("%s returned too-small RowPitch=%u (min=%u)",
                           label,
                           (unsigned)map.RowPitch,
                           (unsigned)min_row_pitch);
    }
    return 0;
  };

  D3D10_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = (UINT)kWidth;
  vp.Height = (UINT)kHeight;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  device->RSSetViewports(1, &vp);

  device->IASetInputLayout(input_layout.get());
  device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  device->VSSetShader(vs.get());
  device->PSSetShader(ps.get());

  // Vertex buffers.
  Vertex fs_verts[3];
  fs_verts[0].pos[0] = -1.0f;
  fs_verts[0].pos[1] = -1.0f;
  fs_verts[0].pos[2] = 0.0f;
  fs_verts[1].pos[0] = -1.0f;
  fs_verts[1].pos[1] = 3.0f;
  fs_verts[1].pos[2] = 0.0f;
  fs_verts[2].pos[0] = 3.0f;
  fs_verts[2].pos[1] = -1.0f;
  fs_verts[2].pos[2] = 0.0f;
  for (int i = 0; i < 3; ++i) {
    fs_verts[i].color[0] = 0.0f;
    fs_verts[i].color[1] = 1.0f;
    fs_verts[i].color[2] = 0.0f;
    fs_verts[i].color[3] = 0.5f;
  }

  Vertex depth_clip_verts[3] = {};
  memcpy(depth_clip_verts, fs_verts, sizeof(fs_verts));
  for (int i = 0; i < 3; ++i) {
    depth_clip_verts[i].pos[2] = -0.5f;
  }

  // CCW centered triangle (culled when CullMode==BACK and FrontCounterClockwise==FALSE).
  Vertex cull_verts[3];
  cull_verts[0].pos[0] = -0.5f;
  cull_verts[0].pos[1] = -0.5f;
  cull_verts[0].pos[2] = 0.0f;
  cull_verts[1].pos[0] = 0.5f;
  cull_verts[1].pos[1] = -0.5f;
  cull_verts[1].pos[2] = 0.0f;
  cull_verts[2].pos[0] = 0.0f;
  cull_verts[2].pos[1] = 0.5f;
  cull_verts[2].pos[2] = 0.0f;
  for (int i = 0; i < 3; ++i) {
    cull_verts[i].color[0] = 0.0f;
    cull_verts[i].color[1] = 1.0f;
    cull_verts[i].color[2] = 0.0f;
    cull_verts[i].color[3] = 0.5f;
  }

  Vertex depth_front_verts[3] = {};
  memcpy(depth_front_verts, fs_verts, sizeof(fs_verts));
  for (int i = 0; i < 3; ++i) {
    depth_front_verts[i].pos[2] = 0.5f;
    depth_front_verts[i].color[0] = 0.0f;
    depth_front_verts[i].color[1] = 1.0f;
    depth_front_verts[i].color[2] = 0.0f;
    depth_front_verts[i].color[3] = 1.0f;
  }

  Vertex depth_back_verts[3] = {};
  memcpy(depth_back_verts, fs_verts, sizeof(fs_verts));
  for (int i = 0; i < 3; ++i) {
    depth_back_verts[i].pos[2] = 0.75f;
    depth_back_verts[i].color[0] = 0.0f;
    depth_back_verts[i].color[1] = 0.0f;
    depth_back_verts[i].color[2] = 1.0f;
    depth_back_verts[i].color[3] = 1.0f;
  }

  D3D10_BUFFER_DESC vb_desc;
  ZeroMemory(&vb_desc, sizeof(vb_desc));
  vb_desc.Usage = D3D10_USAGE_DEFAULT;
  vb_desc.BindFlags = D3D10_BIND_VERTEX_BUFFER;

  D3D10_SUBRESOURCE_DATA vb_init;
  ZeroMemory(&vb_init, sizeof(vb_init));

  ComPtr<ID3D10Buffer> vb_fs;
  vb_desc.ByteWidth = sizeof(fs_verts);
  vb_init.pSysMem = fs_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_fs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_fs)", hr);
  }

  ComPtr<ID3D10Buffer> vb_cull;
  vb_desc.ByteWidth = sizeof(cull_verts);
  vb_init.pSysMem = cull_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_cull.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_cull)", hr);
  }

  ComPtr<ID3D10Buffer> vb_depth_clip;
  vb_desc.ByteWidth = sizeof(depth_clip_verts);
  vb_init.pSysMem = depth_clip_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_depth_clip.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_depth_clip)", hr);
  }

  ComPtr<ID3D10Buffer> vb_depth_front;
  vb_desc.ByteWidth = sizeof(depth_front_verts);
  vb_init.pSysMem = depth_front_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_depth_front.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_depth_front)", hr);
  }

  ComPtr<ID3D10Buffer> vb_depth_back;
  vb_desc.ByteWidth = sizeof(depth_back_verts);
  vb_init.pSysMem = depth_back_verts;
  hr = device->CreateBuffer(&vb_desc, &vb_init, vb_depth_back.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vb_depth_back)", hr);
  }

  const UINT stride = sizeof(Vertex);
  const UINT offset = 0;

  auto SetVb = [&](ID3D10Buffer* vb) {
    ID3D10Buffer* vbs[] = {vb};
    device->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
  };

  // Rasterizer states.
  ComPtr<ID3D10RasterizerState> rs_scissor;
  ComPtr<ID3D10RasterizerState> rs_no_cull;
  ComPtr<ID3D10RasterizerState> rs_cull_back_cw;
  ComPtr<ID3D10RasterizerState> rs_cull_back_ccw;
  ComPtr<ID3D10RasterizerState> rs_depth_clip_disabled;
  {
    D3D10_RASTERIZER_DESC desc;
    ZeroMemory(&desc, sizeof(desc));
    desc.FillMode = D3D10_FILL_SOLID;
    desc.CullMode = D3D10_CULL_NONE;
    desc.FrontCounterClockwise = FALSE;
    desc.DepthBias = 0;
    desc.DepthBiasClamp = 0.0f;
    desc.SlopeScaledDepthBias = 0.0f;
    desc.DepthClipEnable = TRUE;
    desc.ScissorEnable = TRUE;
    desc.MultisampleEnable = FALSE;
    desc.AntialiasedLineEnable = FALSE;
    hr = device->CreateRasterizerState(&desc, rs_scissor.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRasterizerState(scissor)", hr);
    }

    desc.ScissorEnable = FALSE;
    hr = device->CreateRasterizerState(&desc, rs_no_cull.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRasterizerState(no cull)", hr);
    }

    desc.CullMode = D3D10_CULL_BACK;
    desc.FrontCounterClockwise = FALSE;
    hr = device->CreateRasterizerState(&desc, rs_cull_back_cw.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRasterizerState(cull back CW)", hr);
    }

    desc.FrontCounterClockwise = TRUE;
    hr = device->CreateRasterizerState(&desc, rs_cull_back_ccw.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRasterizerState(cull back CCW)", hr);
    }

    desc.CullMode = D3D10_CULL_NONE;
    desc.FrontCounterClockwise = FALSE;
    desc.DepthClipEnable = FALSE;
    hr = device->CreateRasterizerState(&desc, rs_depth_clip_disabled.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRasterizerState(depth clip disabled)", hr);
    }
  }

  // Blend states.
  ComPtr<ID3D10BlendState1> alpha_blend;
  ComPtr<ID3D10BlendState1> green_write_mask;
  ComPtr<ID3D10BlendState1> blend_factor_state;
  {
    D3D10_BLEND_DESC1 desc;
    ZeroMemory(&desc, sizeof(desc));
    desc.AlphaToCoverageEnable = FALSE;
    desc.IndependentBlendEnable = FALSE;
    D3D10_RENDER_TARGET_BLEND_DESC1 rt;
    ZeroMemory(&rt, sizeof(rt));
    rt.BlendEnable = TRUE;
    rt.SrcBlend = D3D10_BLEND_SRC_ALPHA;
    rt.DestBlend = D3D10_BLEND_INV_SRC_ALPHA;
    rt.BlendOp = D3D10_BLEND_OP_ADD;
    rt.SrcBlendAlpha = D3D10_BLEND_ONE;
    rt.DestBlendAlpha = D3D10_BLEND_ZERO;
    rt.BlendOpAlpha = D3D10_BLEND_OP_ADD;
    rt.RenderTargetWriteMask = D3D10_COLOR_WRITE_ENABLE_ALL;
    for (int i = 0; i < 8; ++i) {
      desc.RenderTarget[i] = rt;
    }
    hr = device->CreateBlendState1(&desc, alpha_blend.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateBlendState1(alpha)", hr);
    }

    ZeroMemory(&desc, sizeof(desc));
    desc.AlphaToCoverageEnable = FALSE;
    desc.IndependentBlendEnable = FALSE;
    ZeroMemory(&rt, sizeof(rt));
    rt.BlendEnable = FALSE;
    rt.RenderTargetWriteMask = D3D10_COLOR_WRITE_ENABLE_GREEN;
    for (int i = 0; i < 8; ++i) {
      desc.RenderTarget[i] = rt;
    }
    hr = device->CreateBlendState1(&desc, green_write_mask.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateBlendState1(write mask)", hr);
    }

    ZeroMemory(&desc, sizeof(desc));
    desc.AlphaToCoverageEnable = FALSE;
    desc.IndependentBlendEnable = FALSE;
    ZeroMemory(&rt, sizeof(rt));
    rt.BlendEnable = TRUE;
    rt.SrcBlend = D3D10_BLEND_BLEND_FACTOR;
    rt.DestBlend = D3D10_BLEND_INV_BLEND_FACTOR;
    rt.BlendOp = D3D10_BLEND_OP_ADD;
    rt.SrcBlendAlpha = D3D10_BLEND_ONE;
    rt.DestBlendAlpha = D3D10_BLEND_ZERO;
    rt.BlendOpAlpha = D3D10_BLEND_OP_ADD;
    rt.RenderTargetWriteMask = D3D10_COLOR_WRITE_ENABLE_ALL;
    for (int i = 0; i < 8; ++i) {
      desc.RenderTarget[i] = rt;
    }
    hr = device->CreateBlendState1(&desc, blend_factor_state.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateBlendState1(blend factor)", hr);
    }
  }

  const FLOAT clear_red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  const FLOAT blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  const uint8_t kExpectedAlphaHalf = 0x80u;
  const uint8_t kAlphaTol = 2u;

  ID3D10RenderTargetView* rtvs[] = {rtv.get()};

  auto Readback = [&](ID3D10DepthStencilView* dsv,
                      const wchar_t* bmp_name,
                      const wchar_t* bin_name,
                      uint32_t* out_center,
                      uint32_t* out_corner) -> int {
    device->OMSetRenderTargets(0, NULL, NULL);
    device->CopyResource(staging.get(), rt_tex.get());
    device->OMSetRenderTargets(1, rtvs, dsv);
    device->Flush();

    D3D10_MAPPED_TEXTURE2D map;
    ZeroMemory(&map, sizeof(map));
    hr = staging->Map(0, D3D10_MAP_READ, 0, &map);
    if (FAILED(hr)) {
      return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(staging)", hr, device.get());
    }
    int map_rc = ValidateStagingMap("Map(staging)", map);
    if (map_rc != 0) {
      return map_rc;
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    if (out_center) {
      *out_center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
    }
    if (out_corner) {
      *out_corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);
    }

    if (dump && bmp_name) {
      const std::wstring bmp_path = aerogpu_test::JoinPath(dir, bmp_name);
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed (%ls): %s", kTestName, bmp_name, err.c_str());
      } else {
        reporter.AddArtifactPathW(bmp_path);
      }
    }
    if (dump && bin_name) {
      DumpTightBgra32(kTestName, &reporter, bin_name, map.pData, map.RowPitch, kWidth, kHeight);
    }

    staging->Unmap(0);
    return 0;
  };

  // Subtest 1: Scissor enable.
  {
    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetState(rs_scissor.get());
    const D3D10_RECT scissor = {16, 16, 48, 48};
    device->RSSetScissorRects(1, &scissor);
    device->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    SetVb(vb_fs.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center = 0;
    uint32_t corner = 0;
    int rb = Readback(NULL,
                      L"d3d10_1_rs_om_state_sanity_scissor.bmp",
                      L"d3d10_1_rs_om_state_sanity_scissor.bin",
                      &center,
                      &corner);
    if (rb != 0) {
      return rb;
    }

    const uint32_t expected_red = 0xFFFF0000u;
    const uint32_t expected_green = 0x8000FF00u;
    if ((center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (corner & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("scissor failed: center=0x%08lX expected ~0x%08lX; corner=0x%08lX expected ~0x%08lX",
                           (unsigned long)center,
                           (unsigned long)expected_green,
                           (unsigned long)corner,
                           (unsigned long)expected_red);
    }
    const uint8_t center_a = (uint8_t)((center >> 24) & 0xFFu);
    const uint8_t corner_a = (uint8_t)((corner >> 24) & 0xFFu);
    if ((center_a < kExpectedAlphaHalf - kAlphaTol || center_a > kExpectedAlphaHalf + kAlphaTol) || corner_a != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("scissor alpha mismatch: center_a=%u expected ~%u; corner_a=%u expected 255",
                           (unsigned)center_a,
                           (unsigned)kExpectedAlphaHalf,
                           (unsigned)corner_a);
    }
  }

  // Subtest 2: Cull mode + FrontCounterClockwise.
  {
    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetState(rs_cull_back_cw.get());
    device->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    SetVb(vb_cull.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center = 0;
    int rb = Readback(NULL,
                      L"d3d10_1_rs_om_state_sanity_cull_cw.bmp",
                      L"d3d10_1_rs_om_state_sanity_cull_cw.bin",
                      &center,
                      NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_red = 0xFFFF0000u;
    if ((center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected culled): center=0x%08lX expected ~0x%08lX",
                           (unsigned long)center,
                           (unsigned long)expected_red);
    }

    device->RSSetState(rs_cull_back_ccw.get());
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center2 = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_cull_ccw.bmp",
                  L"d3d10_1_rs_om_state_sanity_cull_ccw.bin",
                  &center2,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_green = 0x8000FF00u;
    const uint8_t a2 = (uint8_t)((center2 >> 24) & 0xFFu);
    if ((center2 & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (a2 < kExpectedAlphaHalf - kAlphaTol || a2 > kExpectedAlphaHalf + kAlphaTol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("cull failed (expected drawn): center=0x%08lX expected ~0x%08lX (a=%u exp~%u)",
                           (unsigned long)center2,
                           (unsigned long)expected_green,
                           (unsigned)a2,
                           (unsigned)kExpectedAlphaHalf);
    }
  }

  // Subtest 3: DepthClipEnable.
  {
    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetState(rs_depth_clip_disabled.get());
    device->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    SetVb(vb_depth_clip.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center = 0;
    int rb = Readback(NULL,
                      L"d3d10_1_rs_om_state_sanity_depth_clip_disabled.bmp",
                      L"d3d10_1_rs_om_state_sanity_depth_clip_disabled.bin",
                      &center,
                      NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_green = 0x8000FF00u;
    const uint8_t a = (uint8_t)((center >> 24) & 0xFFu);
    if ((center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (a < kExpectedAlphaHalf - kAlphaTol || a > kExpectedAlphaHalf + kAlphaTol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("depth clip disabled failed: center=0x%08lX expected ~0x%08lX (a=%u exp~%u)",
                           (unsigned long)center,
                           (unsigned long)expected_green,
                           (unsigned)a,
                           (unsigned)kExpectedAlphaHalf);
    }

    // RSSetState(NULL) restores the default rasterizer state, where DepthClipEnable is TRUE.
    device->RSSetState(NULL);
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center2 = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_depth_clip_null_state.bmp",
                  L"d3d10_1_rs_om_state_sanity_depth_clip_null_state.bin",
                  &center2,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_red = 0xFFFF0000u;
    if ((center2 & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("depth clip NULL state failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)center2,
                           (unsigned long)expected_red);
    }
  }

  // Subtest 4: Blend state.
  {
    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetState(rs_no_cull.get());
    device->OMSetBlendState(alpha_blend.get(), blend_factor, 0xFFFFFFFFu);
    SetVb(vb_fs.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center = 0;
    int rb = Readback(NULL,
                      L"d3d10_1_rs_om_state_sanity_blend.bmp",
                      L"d3d10_1_rs_om_state_sanity_blend.bin",
                      &center,
                      NULL);
    if (rb != 0) {
      return rb;
    }

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
          "blend failed: center=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=%u g=%u b=%u a=%u) tol=%u",
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

    // Disable blending => unblended green output (alpha=0.5).
    device->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center_disabled = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_blend_disabled.bmp",
                  L"d3d10_1_rs_om_state_sanity_blend_disabled.bin",
                  &center_disabled,
                  NULL);
    if (rb != 0) {
      return rb;
    }

    const uint8_t b2 = (uint8_t)(center_disabled & 0xFFu);
    const uint8_t g2 = (uint8_t)((center_disabled >> 8) & 0xFFu);
    const uint8_t r2 = (uint8_t)((center_disabled >> 16) & 0xFFu);
    const uint8_t a2 = (uint8_t)((center_disabled >> 24) & 0xFFu);
    if (r2 != 0 || g2 != 0xFFu || b2 != 0 || (a2 < exp_a - tol || a2 > exp_a + tol)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "blend disable failed: center=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=0 g=255 b=0 a=%u) tol=%u",
          (unsigned long)center_disabled,
          (unsigned)r2,
          (unsigned)g2,
          (unsigned)b2,
          (unsigned)a2,
          (unsigned)exp_a,
          (unsigned)tol);
    }

    // Write mask (green only): clear red, draw green => expect yellow with alpha preserved (0xFF).
    device->OMSetBlendState(green_write_mask.get(), blend_factor, 0xFFFFFFFFu);
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center_mask = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_write_mask.bmp",
                  L"d3d10_1_rs_om_state_sanity_write_mask.bin",
                  &center_mask,
                  NULL);
    if (rb != 0) {
      return rb;
    }

    const uint32_t expected_yellow = 0xFFFFFF00u;
    if ((center_mask & 0x00FFFFFFu) != (expected_yellow & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("write mask failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)center_mask,
                           (unsigned long)expected_yellow);
    }
    const uint8_t a3 = (uint8_t)((center_mask >> 24) & 0xFFu);
    if (a3 != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("write mask failed: expected alpha preserved (0xFF), got a=%u (center=0x%08lX)",
                           (unsigned)a3,
                           (unsigned long)center_mask);
    }

    // Blend factor (BF=0.25): output should be ~0.75*red + 0.25*green.
    const FLOAT bf25[4] = {0.25f, 0.25f, 0.25f, 0.25f};
    device->OMSetBlendState(blend_factor_state.get(), bf25, 0xFFFFFFFFu);
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center_bf = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_blend_factor.bmp",
                  L"d3d10_1_rs_om_state_sanity_blend_factor.bin",
                  &center_bf,
                  NULL);
    if (rb != 0) {
      return rb;
    }

    const uint8_t b4 = (uint8_t)(center_bf & 0xFFu);
    const uint8_t g4 = (uint8_t)((center_bf >> 8) & 0xFFu);
    const uint8_t r4 = (uint8_t)((center_bf >> 16) & 0xFFu);
    const uint8_t a4 = (uint8_t)((center_bf >> 24) & 0xFFu);
    const uint8_t exp_r2 = 0xBF;
    const uint8_t exp_g2 = 0x40;
    const uint8_t exp_b2 = 0x00;
    const uint8_t exp_a2 = 0x80;
    const uint8_t tol2 = 2;
    if ((r4 < exp_r2 - tol2 || r4 > exp_r2 + tol2) || (g4 < exp_g2 - tol2 || g4 > exp_g2 + tol2) ||
        (b4 < exp_b2 - tol2 || b4 > exp_b2 + tol2) || (a4 < exp_a2 - tol2 || a4 > exp_a2 + tol2)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "blend factor failed: center=0x%08lX (r=%u g=%u b=%u a=%u) expected ~(r=%u g=%u b=%u a=%u) tol=%u",
          (unsigned long)center_bf,
          (unsigned)r4,
          (unsigned)g4,
          (unsigned)b4,
          (unsigned)a4,
          (unsigned)exp_r2,
          (unsigned)exp_g2,
          (unsigned)exp_b2,
          (unsigned)exp_a2,
          (unsigned)tol2);
    }

    // SampleMask (0): should discard all color writes.
    device->OMSetBlendState(NULL, blend_factor, 0u);
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center_sm0 = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_sample_mask_0.bmp",
                  L"d3d10_1_rs_om_state_sanity_sample_mask_0.bin",
                  &center_sm0,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_red = 0xFFFF0000u;
    if ((center_sm0 & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) || ((center_sm0 >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("sample mask failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)center_sm0,
                           (unsigned long)expected_red);
    }
  }

  // Subtest 5: Depth/stencil state (depth func).
  {
    // Create + bind depth buffer.
    DXGI_FORMAT depth_format = DXGI_FORMAT_D24_UNORM_S8_UINT;
    const char* depth_format_label = "D24_UNORM_S8_UINT";
    ComPtr<ID3D10Texture2D> depth_tex;
    ComPtr<ID3D10DepthStencilView> dsv;
    HRESULT hr_d24_tex = S_OK;
    HRESULT hr_d24_dsv = S_OK;
    HRESULT hr_d32_tex = S_OK;
    HRESULT hr_d32_dsv = S_OK;

    D3D10_TEXTURE2D_DESC depth_desc = rt_desc;
    depth_desc.Format = depth_format;
    depth_desc.BindFlags = D3D10_BIND_DEPTH_STENCIL;

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

    D3D10_DEPTH_STENCIL_DESC ds_desc;
    ZeroMemory(&ds_desc, sizeof(ds_desc));
    ds_desc.DepthEnable = TRUE;
    ds_desc.DepthWriteMask = D3D10_DEPTH_WRITE_MASK_ALL;
    ds_desc.DepthFunc = D3D10_COMPARISON_LESS;
    ds_desc.StencilEnable = FALSE;
    ds_desc.StencilReadMask = D3D10_DEFAULT_STENCIL_READ_MASK;
    ds_desc.StencilWriteMask = D3D10_DEFAULT_STENCIL_WRITE_MASK;
    ds_desc.FrontFace.StencilFailOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilDepthFailOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilPassOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilFunc = D3D10_COMPARISON_ALWAYS;
    ds_desc.BackFace = ds_desc.FrontFace;

    ComPtr<ID3D10DepthStencilState> dss_less;
    hr = device->CreateDepthStencilState(&ds_desc, dss_less.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateDepthStencilState(LESS)", hr);
    }

    ds_desc.DepthFunc = D3D10_COMPARISON_GREATER;
    ComPtr<ID3D10DepthStencilState> dss_greater;
    hr = device->CreateDepthStencilState(&ds_desc, dss_greater.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateDepthStencilState(GREATER)", hr);
    }

    device->OMSetRenderTargets(1, rtvs, dsv.get());
    device->RSSetState(rs_no_cull.get());
    device->OMSetBlendState(NULL, blend_factor, 0xFFFFFFFFu);
    SetVb(vb_depth_front.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->ClearDepthStencilView(dsv.get(), D3D10_CLEAR_DEPTH, 1.0f, 0);

    device->OMSetDepthStencilState(dss_less.get(), 0);
    device->Draw(3, 0);

    // Draw blue behind; with LESS this should fail.
    SetVb(vb_depth_back.get());
    device->Draw(3, 0);

    uint32_t center = 0;
    int rb = Readback(dsv.get(),
                      L"d3d10_1_rs_om_state_sanity_depth_less.bmp",
                      L"d3d10_1_rs_om_state_sanity_depth_less.bin",
                      &center,
                      NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_green = 0xFF00FF00u;
    if ((center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("depth LESS failed: center=0x%08lX expected ~0x%08lX (fmt=%s)",
                           (unsigned long)center,
                           (unsigned long)expected_green,
                           depth_format_label);
    }

    // GREATER should pass for z=0.75 against existing z=0.5.
    device->OMSetDepthStencilState(dss_greater.get(), 0);
    device->Draw(3, 0);

    uint32_t center2 = 0;
    rb = Readback(dsv.get(),
                  L"d3d10_1_rs_om_state_sanity_depth_greater.bmp",
                  L"d3d10_1_rs_om_state_sanity_depth_greater.bin",
                  &center2,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_blue = 0xFF0000FFu;
    if ((center2 & 0x00FFFFFFu) != (expected_blue & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("depth GREATER failed: center=0x%08lX expected ~0x%08lX (fmt=%s)",
                           (unsigned long)center2,
                           (unsigned long)expected_blue,
                           depth_format_label);
    }
  }

  // Subtest 6: ClearState resets RS/OM state (no scissor, no blending).
  //
  // This specifically validates the UMD ClearState path: if the driver does not
  // emit default RS/OM state packets, host-side state would "stick" across the
  // ClearState call, causing clipped/incorrect rendering.
  {
    // Deliberately set a non-default scissor-enabled rasterizer state and enable
    // alpha blending.
    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetState(rs_scissor.get());
    const D3D10_RECT small_scissor = {16, 16, 48, 48};
    device->RSSetScissorRects(1, &small_scissor);
    device->OMSetBlendState(alpha_blend.get(), blend_factor, 0xFFFFFFFFu);
    SetVb(vb_fs.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    // ClearState unbinds most pipeline state; rebind only the minimum needed to
    // draw, but DO NOT explicitly reset rasterizer/blend state. The output
    // should reflect the D3D10 defaults: scissor disabled + blending disabled.
    device->ClearState();

    device->OMSetRenderTargets(1, rtvs, NULL);
    device->RSSetViewports(1, &vp);
    device->IASetInputLayout(input_layout.get());
    device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    SetVb(vb_fs.get());
    device->VSSetShader(vs.get());
    device->PSSetShader(ps.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t center = 0;
    uint32_t corner = 0;
    int rb = Readback(NULL,
                      L"d3d10_1_rs_om_state_sanity_clear_state.bmp",
                      L"d3d10_1_rs_om_state_sanity_clear_state.bin",
                      &center,
                      &corner);
    if (rb != 0) {
      return rb;
    }

    const uint32_t expected_green = 0x8000FF00u;
    if ((center & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        (corner & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail(
          "ClearState failed: expected no scissor + no blending, but got center=0x%08lX corner=0x%08lX (expected ~0x%08lX)",
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

    // Default RS state is CullMode=BACK, FrontCCW=FALSE. The CCW triangle should
    // be culled, leaving the clear color intact.
    SetVb(vb_cull.get());
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t cull_center = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_clear_state_cull.bmp",
                  L"d3d10_1_rs_om_state_sanity_clear_state_cull.bin",
                  &cull_center,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_red = 0xFFFF0000u;
    if ((cull_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) || ((cull_center >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState cull reset failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)cull_center,
                           (unsigned long)expected_red);
    }

    // Default RS state has DepthClipEnable=TRUE. The fullscreen triangle with
    // Z=-0.5 should be clipped, leaving the clear color intact.
    SetVb(vb_depth_clip.get());
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->Draw(3, 0);

    uint32_t depth_clip_center = 0;
    rb = Readback(NULL,
                  L"d3d10_1_rs_om_state_sanity_clear_state_depth_clip.bmp",
                  L"d3d10_1_rs_om_state_sanity_clear_state_depth_clip.bin",
                  &depth_clip_center,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    if ((depth_clip_center & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu) ||
        ((depth_clip_center >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState depth-clip reset failed: center=0x%08lX expected ~0x%08lX",
                           (unsigned long)depth_clip_center,
                           (unsigned long)expected_red);
    }
  }

  // Subtest 7: ClearState resets depth-stencil state.
  {
    // Create + bind depth buffer.
    DXGI_FORMAT depth_format = DXGI_FORMAT_D24_UNORM_S8_UINT;
    const char* depth_format_label = "D24_UNORM_S8_UINT";
    ComPtr<ID3D10Texture2D> depth_tex;
    ComPtr<ID3D10DepthStencilView> dsv;
    HRESULT hr_d24_tex = S_OK;
    HRESULT hr_d24_dsv = S_OK;
    HRESULT hr_d32_tex = S_OK;
    HRESULT hr_d32_dsv = S_OK;

    D3D10_TEXTURE2D_DESC depth_desc = rt_desc;
    depth_desc.Format = depth_format;
    depth_desc.BindFlags = D3D10_BIND_DEPTH_STENCIL;

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

    D3D10_DEPTH_STENCIL_DESC ds_desc;
    ZeroMemory(&ds_desc, sizeof(ds_desc));
    ds_desc.DepthEnable = TRUE;
    ds_desc.DepthWriteMask = D3D10_DEPTH_WRITE_MASK_ALL;
    ds_desc.DepthFunc = D3D10_COMPARISON_GREATER;
    ds_desc.StencilEnable = FALSE;
    ds_desc.StencilReadMask = D3D10_DEFAULT_STENCIL_READ_MASK;
    ds_desc.StencilWriteMask = D3D10_DEFAULT_STENCIL_WRITE_MASK;
    ds_desc.FrontFace.StencilFailOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilDepthFailOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilPassOp = D3D10_STENCIL_OP_KEEP;
    ds_desc.FrontFace.StencilFunc = D3D10_COMPARISON_ALWAYS;
    ds_desc.BackFace = ds_desc.FrontFace;

    ComPtr<ID3D10DepthStencilState> dss_greater;
    hr = device->CreateDepthStencilState(&ds_desc, dss_greater.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateDepthStencilState(GREATER) [ClearState subtest]", hr);
    }

    device->OMSetRenderTargets(1, rtvs, dsv.get());
    device->RSSetViewports(1, &vp);
    device->IASetInputLayout(input_layout.get());
    device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    device->VSSetShader(vs.get());
    device->PSSetShader(ps.get());
    SetVb(vb_depth_front.get());

    // With depth cleared to 1.0, DepthFunc=GREATER should reject Z=0.5, leaving the clear color intact.
    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->ClearDepthStencilView(dsv.get(), D3D10_CLEAR_DEPTH, 1.0f, 0);
    device->OMSetDepthStencilState(dss_greater.get(), 0);
    device->Draw(3, 0);

    uint32_t before_clear = 0;
    int rb = Readback(dsv.get(),
                      L"d3d10_1_rs_om_state_sanity_clear_state_depth_before.bmp",
                      L"d3d10_1_rs_om_state_sanity_clear_state_depth_before.bin",
                      &before_clear,
                      NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_red = 0xFFFF0000u;
    if ((before_clear & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState depth precondition failed: center=0x%08lX expected ~0x%08lX (fmt=%s)",
                           (unsigned long)before_clear,
                           (unsigned long)expected_red,
                           depth_format_label);
    }

    device->ClearState();

    // ClearState unbinds state; rebind required pipeline state, but do not
    // explicitly set a depth-stencil state. The default should no longer be
    // DepthFunc=GREATER, so the Z=0.5 triangle should draw.
    device->OMSetRenderTargets(1, rtvs, dsv.get());
    device->RSSetViewports(1, &vp);
    device->IASetInputLayout(input_layout.get());
    device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    device->VSSetShader(vs.get());
    device->PSSetShader(ps.get());
    SetVb(vb_depth_front.get());

    device->ClearRenderTargetView(rtv.get(), clear_red);
    device->ClearDepthStencilView(dsv.get(), D3D10_CLEAR_DEPTH, 1.0f, 0);
    device->Draw(3, 0);

    uint32_t after_clear = 0;
    rb = Readback(dsv.get(),
                  L"d3d10_1_rs_om_state_sanity_clear_state_depth_after.bmp",
                  L"d3d10_1_rs_om_state_sanity_clear_state_depth_after.bin",
                  &after_clear,
                  NULL);
    if (rb != 0) {
      return rb;
    }
    const uint32_t expected_green = 0xFF00FF00u;
    if ((after_clear & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu) ||
        ((after_clear >> 24) & 0xFFu) != 0xFFu) {
      PrintDeviceRemovedReasonIfAny(kTestName, device.get());
      return reporter.Fail("ClearState depth reset failed: center=0x%08lX expected ~0x%08lX (fmt=%s)",
                           (unsigned long)after_clear,
                           (unsigned long)expected_green,
                           depth_format_label);
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D101RSOMStateSanity(argc, argv);
}
