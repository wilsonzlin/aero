#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"
#include "..\\common\\aerogpu_test_shaders.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float color[4];
};

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

static int RunReadbackSanity(int argc, char** argv) {
  const char* kTestName = "readback_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd]",
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
  const std::wstring dir = aerogpu_test::GetModuleDir();
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestBasicColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestBasicColorHlsl),
                                           "readback_sanity.hlsl",
                                           "vs_main",
                                           "vs_4_0_level_9_1",
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestBasicColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestBasicColorHlsl),
                                           "readback_sanity.hlsl",
                                           "ps_main",
                                           "ps_4_0_level_9_1",
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
      {"POSITION", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
      {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 8, D3D11_INPUT_PER_VERTEX_DATA, 0},
  };

  ComPtr<ID3D11InputLayout> input_layout;
  hr = device->CreateInputLayout(il,
                                 ARRAYSIZE(il),
                                  &vs_bytes[0],
                                  vs_bytes.size(),
                                  input_layout.put());
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

  Vertex verts[3];
  // A centered triangle that covers the center pixel but not the top-left corner.
  verts[0].pos[0] = -0.5f;
  verts[0].pos[1] = -0.5f;
  verts[1].pos[0] = 0.0f;
  verts[1].pos[1] = 0.5f;
  verts[2].pos[0] = 0.5f;
  verts[2].pos[1] = -0.5f;
  for (int i = 0; i < 3; ++i) {
    verts[i].color[0] = 0.0f;
    verts[i].color[1] = 1.0f;
    verts[i].color[2] = 0.0f;
    verts[i].color[3] = 1.0f;
  }

  D3D11_BUFFER_DESC bd;
  ZeroMemory(&bd, sizeof(bd));
  bd.ByteWidth = sizeof(verts);
  bd.Usage = D3D11_USAGE_DEFAULT;
  bd.BindFlags = D3D11_BIND_VERTEX_BUFFER;

  D3D11_SUBRESOURCE_DATA init;
  ZeroMemory(&init, sizeof(init));
  init.pSysMem = verts;

  ComPtr<ID3D11Buffer> vb;
  hr = device->CreateBuffer(&bd, &init, vb.put());
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
  context->Draw(3, 0);
  // Avoid any ambiguity around copying from a still-bound render target.
  context->OMSetRenderTargets(0, NULL, NULL);

  // Read back the result via a staging texture.
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
  const UINT min_row_pitch = kWidth * 4;
  if (map.RowPitch < min_row_pitch) {
    context->Unmap(staging.get(), 0);
    return reporter.Fail("Map(staging) returned too-small RowPitch=%u (min=%u)",
                         (unsigned)map.RowPitch,
                         (unsigned)min_row_pitch);
  }

  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 0, 0);
  const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, kWidth / 2, kHeight / 2);

  const uint32_t expected_corner = 0xFFFF0000u;
  const uint32_t expected_center = 0xFF00FF00u;

  if (dump) {
    std::string err;
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"readback_sanity.bmp");
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path,
                                      kWidth,
                                      kHeight,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
    DumpTightBgra32(kTestName, &reporter, L"readback_sanity.bin", map.pData, map.RowPitch, kWidth, kHeight);
  }

  context->Unmap(staging.get(), 0);

  if ((corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    PrintDeviceRemovedReasonIfAny(kTestName, device.get());
    return reporter.Fail("corner pixel mismatch: got 0x%08lX expected ~0x%08lX",
                         (unsigned long)corner,
                         (unsigned long)expected_corner);
  }
  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu)) {
    PrintDeviceRemovedReasonIfAny(kTestName, device.get());
    return reporter.Fail("center pixel mismatch: got 0x%08lX expected ~0x%08lX",
                         (unsigned long)center,
                         (unsigned long)expected_center);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunReadbackSanity(argc, argv);
}
