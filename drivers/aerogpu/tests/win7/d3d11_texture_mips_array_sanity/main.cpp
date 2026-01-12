#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

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

static uint32_t PackBGRA(uint8_t b, uint8_t g, uint8_t r, uint8_t a) {
  uint32_t v = 0;
  v |= (uint32_t)b;
  v |= (uint32_t)g << 8;
  v |= (uint32_t)r << 16;
  v |= (uint32_t)a << 24;
  return v;
}

static const char kHlsl[] = R"(
Texture2DArray tex0 : register(t0);
SamplerState samp0 : register(s0);

struct VSIn {
  float2 pos : POSITION;
};

struct VSOut {
  float4 pos : SV_Position;
};

VSOut vs_main(VSIn input) {
  VSOut o;
  o.pos = float4(input.pos.xy, 0.0f, 1.0f);
  return o;
}

float4 ps_main(VSOut input) : SV_Target {
  uint2 pix = uint2(input.pos.xy);
  float slice = (pix.y == 0) ? 0.0f : 1.0f;
  float mip = (pix.x == 0) ? 0.0f : 1.0f;
  return tex0.SampleLevel(samp0, float3(0.5f, 0.5f, slice), mip);
}
)";

struct Vertex {
  float pos[2];
};

static int RunD3D11TextureMipsArraySanity(int argc, char** argv) {
  const char* kTestName = "d3d11_texture_mips_array_sanity";
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

  // Adapter selection / allow-listing (mirrors other tests).
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
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(
          kHlsl, strlen(kHlsl), "d3d11_texture_mips_array_sanity.hlsl", "vs_main", "vs_4_0", &vs_bytes, &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(
          kHlsl, strlen(kHlsl), "d3d11_texture_mips_array_sanity.hlsl", "ps_main", "ps_4_0", &ps_bytes, &shader_err)) {
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
      {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };
  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateInputLayout", hr);
  }

  // Render target: 2x2 so SV_Position-based selection is unambiguous.
  const int kWidth = 2;
  const int kHeight = 2;

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

  // Source texture: 2 slices, 2 mips.
  D3D11_TEXTURE2D_DESC src_desc;
  ZeroMemory(&src_desc, sizeof(src_desc));
  src_desc.Width = 2;
  src_desc.Height = 2;
  src_desc.MipLevels = 2;
  src_desc.ArraySize = 2;
  src_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  src_desc.SampleDesc.Count = 1;
  src_desc.SampleDesc.Quality = 0;
  src_desc.Usage = D3D11_USAGE_DEFAULT;
  src_desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
  src_desc.CPUAccessFlags = 0;
  src_desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> src_tex;
  hr = device->CreateTexture2D(&src_desc, NULL, src_tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(src texture array)", hr);
  }

  auto upload_solid = [&](UINT mip, UINT slice, uint32_t color) -> HRESULT {
    const UINT mip_w = (mip == 0) ? 2u : 1u;
    const UINT mip_h = (mip == 0) ? 2u : 1u;
    const UINT tight_pitch = mip_w * 4u;
    const UINT row_pitch = tight_pitch + 8u; // include padding to validate RowPitch handling
    std::vector<uint8_t> upload((size_t)row_pitch * (size_t)mip_h, 0);
    for (UINT y = 0; y < mip_h; ++y) {
      uint32_t* row = (uint32_t*)(&upload[(size_t)y * (size_t)row_pitch]);
      for (UINT x = 0; x < mip_w; ++x) {
        row[x] = color;
      }
    }
    const UINT sub = D3D11CalcSubresource(mip, slice, src_desc.MipLevels);
    context->UpdateSubresource(src_tex.get(), sub, NULL, &upload[0], row_pitch, 0);
    return S_OK;
  };

  // Distinct colors per (slice,mip).
  hr = upload_solid(0, 0, PackBGRA(0, 0, 255, 255));        // slice0 mip0 = red
  if (FAILED(hr)) {
    return reporter.FailHresult("UpdateSubresource(slice0 mip0)", hr);
  }
  hr = upload_solid(1, 0, PackBGRA(0, 255, 0, 255));        // slice0 mip1 = green
  if (FAILED(hr)) {
    return reporter.FailHresult("UpdateSubresource(slice0 mip1)", hr);
  }
  hr = upload_solid(0, 1, PackBGRA(255, 0, 0, 255));        // slice1 mip0 = blue
  if (FAILED(hr)) {
    return reporter.FailHresult("UpdateSubresource(slice1 mip0)", hr);
  }
  hr = upload_solid(1, 1, PackBGRA(255, 255, 255, 255));    // slice1 mip1 = white
  if (FAILED(hr)) {
    return reporter.FailHresult("UpdateSubresource(slice1 mip1)", hr);
  }

  D3D11_SHADER_RESOURCE_VIEW_DESC srv_desc;
  ZeroMemory(&srv_desc, sizeof(srv_desc));
  srv_desc.Format = src_desc.Format;
  srv_desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2DARRAY;
  srv_desc.Texture2DArray.MostDetailedMip = 0;
  srv_desc.Texture2DArray.MipLevels = src_desc.MipLevels;
  srv_desc.Texture2DArray.FirstArraySlice = 0;
  srv_desc.Texture2DArray.ArraySize = src_desc.ArraySize;

  ComPtr<ID3D11ShaderResourceView> srv;
  hr = device->CreateShaderResourceView(src_tex.get(), &srv_desc, srv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateShaderResourceView", hr);
  }

  D3D11_SAMPLER_DESC samp_desc;
  ZeroMemory(&samp_desc, sizeof(samp_desc));
  samp_desc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
  samp_desc.AddressU = D3D11_TEXTURE_ADDRESS_CLAMP;
  samp_desc.AddressV = D3D11_TEXTURE_ADDRESS_CLAMP;
  samp_desc.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
  samp_desc.MipLODBias = 0.0f;
  samp_desc.MaxAnisotropy = 1;
  samp_desc.ComparisonFunc = D3D11_COMPARISON_NEVER;
  samp_desc.MinLOD = 0.0f;
  samp_desc.MaxLOD = D3D11_FLOAT32_MAX;

  ComPtr<ID3D11SamplerState> sampler;
  hr = device->CreateSamplerState(&samp_desc, sampler.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateSamplerState", hr);
  }

  // Fullscreen quad.
  Vertex verts[4];
  verts[0].pos[0] = -1.0f; verts[0].pos[1] = 1.0f;
  verts[1].pos[0] = 1.0f;  verts[1].pos[1] = 1.0f;
  verts[2].pos[0] = 1.0f;  verts[2].pos[1] = -1.0f;
  verts[3].pos[0] = -1.0f; verts[3].pos[1] = -1.0f;

  uint16_t indices[6] = {0, 1, 2, 0, 2, 3};

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

  D3D11_BUFFER_DESC ib_desc;
  ZeroMemory(&ib_desc, sizeof(ib_desc));
  ib_desc.ByteWidth = sizeof(indices);
  ib_desc.Usage = D3D11_USAGE_DEFAULT;
  ib_desc.BindFlags = D3D11_BIND_INDEX_BUFFER;

  D3D11_SUBRESOURCE_DATA ib_init;
  ZeroMemory(&ib_init, sizeof(ib_init));
  ib_init.pSysMem = indices;

  ComPtr<ID3D11Buffer> ib;
  hr = device->CreateBuffer(&ib_desc, &ib_init, ib.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(index)", hr);
  }

  context->IASetInputLayout(input_layout.get());
  context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  UINT stride = sizeof(Vertex);
  UINT offset = 0;
  ID3D11Buffer* vbs[] = {vb.get()};
  context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);
  context->IASetIndexBuffer(ib.get(), DXGI_FORMAT_R16_UINT, 0);

  context->VSSetShader(vs.get(), NULL, 0);
  context->PSSetShader(ps.get(), NULL, 0);

  ID3D11ShaderResourceView* srvs[] = {srv.get()};
  context->PSSetShaderResources(0, 1, srvs);
  ID3D11SamplerState* samplers[] = {sampler.get()};
  context->PSSetSamplers(0, 1, samplers);

  const FLOAT clear_rgba[4] = {0.0f, 0.0f, 0.0f, 1.0f};
  context->ClearRenderTargetView(rtv.get(), clear_rgba);
  context->DrawIndexed(6, 0, 0);

  // Explicitly unbind.
  ID3D11ShaderResourceView* null_srvs[] = {NULL};
  context->PSSetShaderResources(0, 1, null_srvs);
  ID3D11SamplerState* null_samplers[] = {NULL};
  context->PSSetSamplers(0, 1, null_samplers);
  context->IASetIndexBuffer(NULL, DXGI_FORMAT_UNKNOWN, 0);
  ID3D11Buffer* null_vb = NULL;
  const UINT zero = 0;
  context->IASetVertexBuffers(0, 1, &null_vb, &zero, &zero);
  context->IASetInputLayout(NULL);
  context->VSSetShader(NULL, NULL, 0);
  context->PSSetShader(NULL, NULL, 0);
  context->OMSetRenderTargets(0, NULL, NULL);

  // Read back via staging.
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

  context->CopyResource(staging.get(), rt_tex.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging)", hr, device.get());
  }
  if (!map.pData) {
    context->Unmap(staging.get(), 0);
    return reporter.Fail("Map(staging) returned NULL pData");
  }
  if ((int)map.RowPitch < kWidth * 4) {
    context->Unmap(staging.get(), 0);
    return reporter.Fail("Map(staging) returned unexpected RowPitch=%ld (expected >= %d)",
                         (long)map.RowPitch,
                         kWidth * 4);
  }

  const uint32_t p00 = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 0, 0);
  const uint32_t p10 = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 1, 0);
  const uint32_t p01 = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 0, 1);
  const uint32_t p11 = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 1, 1);

  const uint32_t expected_p00 = PackBGRA(0, 0, 255, 255);          // slice0 mip0
  const uint32_t expected_p10 = PackBGRA(0, 255, 0, 255);          // slice0 mip1
  const uint32_t expected_p01 = PackBGRA(255, 0, 0, 255);          // slice1 mip0
  const uint32_t expected_p11 = PackBGRA(255, 255, 255, 255);      // slice1 mip1

  if (dump) {
    const std::wstring dir = aerogpu_test::GetModuleDir();
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_texture_mips_array_sanity.bmp");
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path, kWidth, kHeight, map.pData, (int)map.RowPitch, &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
  }

  context->Unmap(staging.get(), 0);

  if ((p00 & 0x00FFFFFFu) != (expected_p00 & 0x00FFFFFFu) ||
      (p10 & 0x00FFFFFFu) != (expected_p10 & 0x00FFFFFFu) ||
      (p01 & 0x00FFFFFFu) != (expected_p01 & 0x00FFFFFFu) ||
      (p11 & 0x00FFFFFFu) != (expected_p11 & 0x00FFFFFFu)) {
    PrintD3D11DeviceRemovedReasonIfFailed(kTestName, device.get());
    return reporter.Fail("pixel mismatch: (0,0)=0x%08lX expected 0x%08lX; (1,0)=0x%08lX expected 0x%08lX; "
                         "(0,1)=0x%08lX expected 0x%08lX; (1,1)=0x%08lX expected 0x%08lX",
                         (unsigned long)p00,
                         (unsigned long)expected_p00,
                         (unsigned long)p10,
                         (unsigned long)expected_p10,
                         (unsigned long)p01,
                         (unsigned long)expected_p01,
                         (unsigned long)p11,
                         (unsigned long)expected_p11);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11TextureMipsArraySanity(argc, argv);
}

