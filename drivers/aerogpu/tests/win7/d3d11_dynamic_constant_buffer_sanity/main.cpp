#include "..\\common\\aerogpu_test_common.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
};

struct ConstantBufferData {
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

static int RunD3D11DynamicConstantBufferSanity(int argc, char** argv) {
  const char* kTestName = "d3d11_dynamic_constant_buffer_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }
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
    return aerogpu_test::FailHresult(kTestName, "D3D11CreateDevice(HARDWARE)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

  ComPtr<IDXGIDevice> dxgi_device;
  hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (SUCCEEDED(hr)) {
    ComPtr<IDXGIAdapter> adapter;
    HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
    if (FAILED(hr_adapter)) {
      if (has_require_vid || has_require_did) {
        return aerogpu_test::FailHresult(
            kTestName,
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
    return aerogpu_test::FailHresult(kTestName,
                                     "QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)",
                                     hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  // Load precompiled shaders generated by build_vs2010.cmd.
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring vs_path =
      aerogpu_test::JoinPath(dir, L"d3d11_dynamic_constant_buffer_sanity_vs.cso");
  const std::wstring ps_path =
      aerogpu_test::JoinPath(dir, L"d3d11_dynamic_constant_buffer_sanity_ps.cso");

  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string file_err;
  if (!aerogpu_test::ReadFileBytes(vs_path, &vs_bytes, &file_err)) {
    return aerogpu_test::Fail(
        kTestName, "failed to read %ls: %s", vs_path.c_str(), file_err.c_str());
  }
  if (!aerogpu_test::ReadFileBytes(ps_path, &ps_bytes, &file_err)) {
    return aerogpu_test::Fail(
        kTestName, "failed to read %ls: %s", ps_path.c_str(), file_err.c_str());
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
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(
      il, ARRAYSIZE(il), &vs_bytes[0], vs_bytes.size(), input_layout.put());
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

  // A fullscreen triangle followed by a smaller centered triangle. We update the same dynamic
  // constant buffer between draws using Map(WRITE_DISCARD) and validate both colors in a single
  // readback (corner vs center).
  Vertex verts[6];
  // Fullscreen triangle (covers the viewport using the top-left rule).
  verts[0].pos[0] = -1.0f;
  verts[0].pos[1] = -1.0f;
  verts[1].pos[0] = -1.0f;
  verts[1].pos[1] = 3.0f;
  verts[2].pos[0] = 3.0f;
  verts[2].pos[1] = -1.0f;
  // Centered triangle (covers the center pixel but should not touch the corners).
  verts[3].pos[0] = -0.5f;
  verts[3].pos[1] = -0.5f;
  verts[4].pos[0] = 0.0f;
  verts[4].pos[1] = 0.5f;
  verts[5].pos[0] = 0.5f;
  verts[5].pos[1] = -0.5f;

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
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(vertex)", hr);
  }

  // Dynamic constant buffer.
  D3D11_BUFFER_DESC cb_desc;
  ZeroMemory(&cb_desc, sizeof(cb_desc));
  cb_desc.ByteWidth = sizeof(ConstantBufferData);
  cb_desc.Usage = D3D11_USAGE_DYNAMIC;
  cb_desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
  cb_desc.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE;
  cb_desc.MiscFlags = 0;
  cb_desc.StructureByteStride = 0;

  ComPtr<ID3D11Buffer> cb;
  hr = device->CreateBuffer(&cb_desc, NULL, cb.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateBuffer(constant buffer)", hr);
  }

  D3D11_MAPPED_SUBRESOURCE cb_map;
  ZeroMemory(&cb_map, sizeof(cb_map));
  hr = context->Map(cb.get(), 0, D3D11_MAP_WRITE_DISCARD, 0, &cb_map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(constant buffer, WRITE_DISCARD)", hr, device.get());
  }

  ConstantBufferData* cb_data = (ConstantBufferData*)cb_map.pData;
  cb_data->color[0] = 0.0f;
  cb_data->color[1] = 0.0f;
  cb_data->color[2] = 1.0f;
  cb_data->color[3] = 1.0f;

  context->Unmap(cb.get(), 0);

  context->IASetInputLayout(input_layout.get());
  context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

  UINT stride = sizeof(Vertex);
  UINT offset = 0;
  ID3D11Buffer* vbs[] = {vb.get()};
  context->IASetVertexBuffers(0, 1, vbs, &stride, &offset);

  context->VSSetShader(vs.get(), NULL, 0);
  context->PSSetShader(ps.get(), NULL, 0);

  ID3D11Buffer* cbs[] = {cb.get()};
  context->PSSetConstantBuffers(0, 1, cbs);

  const FLOAT clear_rgba[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  context->ClearRenderTargetView(rtv.get(), clear_rgba);
  context->Draw(3, 0);

  // Update the constant buffer again after it has been bound and used, to exercise the discard
  // path more realistically.
  ZeroMemory(&cb_map, sizeof(cb_map));
  hr = context->Map(cb.get(), 0, D3D11_MAP_WRITE_DISCARD, 0, &cb_map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(
        kTestName, "Map(constant buffer, WRITE_DISCARD #2)", hr, device.get());
  }
  cb_data = (ConstantBufferData*)cb_map.pData;
  cb_data->color[0] = 0.0f;
  cb_data->color[1] = 1.0f;
  cb_data->color[2] = 0.0f;
  cb_data->color[3] = 1.0f;
  context->Unmap(cb.get(), 0);

  context->Draw(3, 3);

  // Read back the result via a staging texture.
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

  context->CopyResource(staging.get(), rt_tex.get());
  context->Flush();

  D3D11_MAPPED_SUBRESOURCE map;
  ZeroMemory(&map, sizeof(map));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(kTestName, "Map(staging)", hr, device.get());
  }

  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);
  const uint32_t center =
      aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth / 2, kHeight / 2);
  const uint32_t expected_corner = 0xFF0000FFu;
  const uint32_t expected_center = 0xFF00FF00u;

  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(
            aerogpu_test::JoinPath(dir, L"d3d11_dynamic_constant_buffer_sanity.bmp"),
            kWidth,
            kHeight,
            map.pData,
            (int)map.RowPitch,
            &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    }
  }

  context->Unmap(staging.get(), 0);

  if ((corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu) ||
      (center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(kTestName,
                              "pixel mismatch: corner(5,5)=0x%08lX center=0x%08lX",
                              (unsigned long)corner,
                              (unsigned long)center);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D11DynamicConstantBufferSanity(argc, argv);
}
