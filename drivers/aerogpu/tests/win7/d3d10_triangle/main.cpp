#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"
#include "..\\common\\aerogpu_test_shaders.h"

#include <d3d10.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float pos[2];
  float color[4];
};

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

struct MapDoNotWaitThreadArgs {
  ID3D10Texture2D* tex;
  HRESULT hr;
};

static DWORD WINAPI MapDoNotWaitThreadProc(LPVOID param) {
  MapDoNotWaitThreadArgs* args = (MapDoNotWaitThreadArgs*)param;
  args->hr = E_FAIL;

  D3D10_MAPPED_TEXTURE2D mapped;
  ZeroMemory(&mapped, sizeof(mapped));
  args->hr = args->tex->Map(0, D3D10_MAP_READ, D3D10_MAP_FLAG_DO_NOT_WAIT, &mapped);
  if (SUCCEEDED(args->hr)) {
    args->tex->Unmap(0);
  }

  args->tex->Release();
  args->tex = NULL;
  return 0;
}

static int RunD3D10Triangle(int argc, char** argv) {
  const char* kTestName = "d3d10_triangle";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);
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

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D10Triangle",
                                              L"AeroGPU D3D10 Triangle",
                                               kWidth,
                                               kHeight,
                                               !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
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

  ComPtr<ID3D10Device> device;
  ComPtr<IDXGISwapChain> swapchain;

  const UINT flags = D3D10_CREATE_DEVICE_BGRA_SUPPORT;

  HRESULT hr = D3D10CreateDeviceAndSwapChain(NULL,
                                             D3D10_DRIVER_TYPE_HARDWARE,
                                             NULL,
                                             flags,
                                             D3D10_SDK_VERSION,
                                              &scd,
                                              swapchain.put(),
                                              device.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D10CreateDeviceAndSwapChain(HARDWARE)", hr);
  }

  // This test is specifically intended to exercise the D3D10 runtime path (d3d10.dll), which
  // should in turn use the UMD's OpenAdapter10 entrypoint.
  if (!GetModuleHandleW(L"d3d10.dll")) {
    return reporter.Fail("d3d10.dll is not loaded");
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

    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_10 = GetProcAddress(umd, "OpenAdapter10");
    if (!open_adapter_10) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_10 = GetProcAddress(umd, "_OpenAdapter10@4");
    }
    if (!open_adapter_10) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter10 (D3D10 entrypoint)");
    }
  }

  ComPtr<ID3D10Texture2D> backbuffer;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D10Texture2D), (void**)backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDXGISwapChain::GetBuffer", hr);
  }

  ComPtr<ID3D10RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(backbuffer.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView", hr);
  }

  ID3D10RenderTargetView* rtvs[] = {rtv.get()};
  device->OMSetRenderTargets(1, rtvs, NULL);

  D3D10_VIEWPORT vp;
  vp.TopLeftX = 0;
  vp.TopLeftY = 0;
  vp.Width = (UINT)kWidth;
  vp.Height = (UINT)kHeight;
  vp.MinDepth = 0.0f;
  vp.MaxDepth = 1.0f;
  device->RSSetViewports(1, &vp);

  // Compile shaders at runtime (no fxc.exe build-time dependency).
  const std::wstring dir = aerogpu_test::GetModuleDir();
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestConstantBufferColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestConstantBufferColorHlsl),
                                           "d3d10_triangle.hlsl",
                                           "vs_main",
                                           "vs_4_0",
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestConstantBufferColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestConstantBufferColorHlsl),
                                           "d3d10_triangle.hlsl",
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
    return reporter.FailHresult("CreateInputLayout", hr);
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
    // Vertex colors should not affect output; keep them as red so the test fails if the wrong
    // shader is accidentally compiled/bound (e.g. one that directly uses vertex color).
    verts[i].color[0] = 1.0f;
    verts[i].color[1] = 0.0f;
    verts[i].color[2] = 0.0f;
    verts[i].color[3] = 1.0f;
  }

  D3D10_BUFFER_DESC bd;
  ZeroMemory(&bd, sizeof(bd));
  bd.ByteWidth = sizeof(verts);
  bd.Usage = D3D10_USAGE_DEFAULT;
  bd.BindFlags = D3D10_BIND_VERTEX_BUFFER;

  D3D10_SUBRESOURCE_DATA init;
  ZeroMemory(&init, sizeof(init));
  init.pSysMem = verts;

  ComPtr<ID3D10Buffer> vb;
  hr = device->CreateBuffer(&bd, &init, vb.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(vertex)", hr);
  }

  // Bind an extra dummy vertex buffer in slot 1 to exercise multi-buffer IA binding.
  // Many real D3D10 apps bind multiple VBs even if the current input layout only
  // references slot 0.
  uint32_t dummy_vb_data[4] = {};
  D3D10_BUFFER_DESC dummy_desc;
  ZeroMemory(&dummy_desc, sizeof(dummy_desc));
  dummy_desc.ByteWidth = sizeof(dummy_vb_data);
  dummy_desc.Usage = D3D10_USAGE_DEFAULT;
  dummy_desc.BindFlags = D3D10_BIND_VERTEX_BUFFER;
  D3D10_SUBRESOURCE_DATA dummy_init;
  ZeroMemory(&dummy_init, sizeof(dummy_init));
  dummy_init.pSysMem = dummy_vb_data;
  ComPtr<ID3D10Buffer> dummy_vb;
  hr = device->CreateBuffer(&dummy_desc, &dummy_init, dummy_vb.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(dummy vertex)", hr);
  }

  UINT strides[2] = {sizeof(Vertex), sizeof(uint32_t)};
  UINT offsets[2] = {0, 0};
  ID3D10Buffer* vbs[] = {vb.get(), dummy_vb.get()};
  device->IASetVertexBuffers(0, 2, vbs, strides, offsets);

  // Exercise non-zero StartSlot updates and null-buffer unbinds.
  {
    UINT slot1_stride = sizeof(uint32_t);
    UINT slot1_offset = 0;
    ID3D10Buffer* slot1_vbs[] = {dummy_vb.get()};
    device->IASetVertexBuffers(1, 1, slot1_vbs, &slot1_stride, &slot1_offset);

    // Some D3D10 runtimes issue NumBuffers==0 calls to clear a tail range of slots.
    device->IASetVertexBuffers(1, 0, NULL, NULL, NULL);

    ID3D10Buffer* null_vbs[] = {NULL};
    UINT zero = 0;
    device->IASetVertexBuffers(1, 1, null_vbs, &zero, &zero);
    device->IASetVertexBuffers(1, 1, slot1_vbs, &slot1_stride, &slot1_offset);
  }

  device->VSSetShader(vs.get());
  device->PSSetShader(ps.get());

  struct Constants {
    float vs_color[4];
    float ps_mod[4];
  };
  Constants constants{};
  constants.vs_color[0] = 0.0f;
  constants.vs_color[1] = 1.0f;
  constants.vs_color[2] = 0.0f;
  constants.vs_color[3] = 1.0f;
  constants.ps_mod[0] = 1.0f;
  constants.ps_mod[1] = 1.0f;
  constants.ps_mod[2] = 1.0f;
  constants.ps_mod[3] = 1.0f;

  D3D10_BUFFER_DESC cb_desc;
  ZeroMemory(&cb_desc, sizeof(cb_desc));
  cb_desc.ByteWidth = sizeof(constants);
  // Use DEFAULT so the resource is guest-backed (exercises alloc-table tracking +
  // dirty-range uploads), instead of a host-owned dynamic buffer.
  cb_desc.Usage = D3D10_USAGE_DEFAULT;
  cb_desc.BindFlags = D3D10_BIND_CONSTANT_BUFFER;
  cb_desc.CPUAccessFlags = 0;
  cb_desc.MiscFlags = 0;

  ComPtr<ID3D10Buffer> cb0;
  hr = device->CreateBuffer(&cb_desc, NULL, cb0.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateBuffer(constant)", hr);
  }

  device->UpdateSubresource(cb0.get(), 0, NULL, &constants, 0, 0);

  ID3D10Buffer* cb_ptr = cb0.get();
  device->VSSetConstantBuffers(0, 1, &cb_ptr);
  device->PSSetConstantBuffers(0, 1, &cb_ptr);

  const FLOAT clear_rgba[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  device->ClearRenderTargetView(rtv.get(), clear_rgba);
  device->Draw(3, 0);
  // Avoid any ambiguity around copying from a still-bound render target.
  device->OMSetRenderTargets(0, NULL, NULL);

  // Read back the center pixel before present.
  D3D10_TEXTURE2D_DESC bb_desc;
  backbuffer->GetDesc(&bb_desc);
  if (bb_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
    return reporter.Fail("unexpected backbuffer format: %u (expected DXGI_FORMAT_B8G8R8A8_UNORM=%u)",
                         (unsigned)bb_desc.Format,
                         (unsigned)DXGI_FORMAT_B8G8R8A8_UNORM);
  }

  D3D10_TEXTURE2D_DESC st_desc = bb_desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;
  st_desc.Usage = D3D10_USAGE_STAGING;

  ComPtr<ID3D10Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  device->CopyResource(staging.get(), backbuffer.get());

  // Probe DO_NOT_WAIT map before any explicit Flush call. A correct UMD should
  // either return DXGI_ERROR_WAS_STILL_DRAWING (in-flight copy) or succeed if the
  // work completed quickly.
  {
    MapDoNotWaitThreadArgs args;
    ZeroMemory(&args, sizeof(args));
    args.tex = staging.get();
    args.hr = E_FAIL;
    staging->AddRef();

    HANDLE thread = CreateThread(NULL, 0, &MapDoNotWaitThreadProc, &args, 0, NULL);
    if (!thread) {
      staging->Release();
      return reporter.Fail("CreateThread(Map DO_NOT_WAIT) failed");
    }
    const DWORD wait = WaitForSingleObject(thread, 250);
    CloseHandle(thread);
    if (wait == WAIT_TIMEOUT) {
      return reporter.Fail("Map(staging, DO_NOT_WAIT) appears to have blocked (>250ms)");
    }

    hr = args.hr;
    if (hr == DXGI_ERROR_WAS_STILL_DRAWING) {
      // Expected: the CopyResource is still being processed by the GPU.
    } else if (SUCCEEDED(hr)) {
      // Allowed: work completed quickly.
    } else {
      return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(staging, DO_NOT_WAIT)", hr, device.get());
    }
  }

  device->Flush();

  D3D10_MAPPED_TEXTURE2D map;
  ZeroMemory(&map, sizeof(map));
  hr = staging->Map(0, D3D10_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(&reporter, kTestName, "Map(staging)", hr, device.get());
  }
  if (!map.pData) {
    staging->Unmap(0);
    return reporter.Fail("Map(staging) returned NULL pData");
  }
  const UINT min_row_pitch = bb_desc.Width * 4;
  if (map.RowPitch < min_row_pitch) {
    staging->Unmap(0);
    return reporter.Fail("Map(staging) returned too-small RowPitch=%u (min=%u)",
                         (unsigned)map.RowPitch,
                         (unsigned)min_row_pitch);
  }

  const int cx = (int)bb_desc.Width / 2;
  const int cy = (int)bb_desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 5, 5);
  const uint32_t expected = 0xFF00FF00u;
  const uint32_t expected_corner = 0xFFFF0000u;

  if (dump) {
    std::string err;
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d10_triangle.bmp");
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path,
                                      (int)bb_desc.Width,
                                      (int)bb_desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
    DumpTightBgra32(kTestName,
                    &reporter,
                    L"d3d10_triangle.bin",
                    map.pData,
                    map.RowPitch,
                    (int)bb_desc.Width,
                    (int)bb_desc.Height);
  }

  staging->Unmap(0);

  hr = swapchain->Present(0, 0);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(&reporter, kTestName, "IDXGISwapChain::Present", hr, device.get());
  }

  if ((center & 0x00FFFFFFu) != (expected & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    PrintDeviceRemovedReasonIfAny(kTestName, device.get());
    return reporter.Fail("pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                         (unsigned long)center,
                         (unsigned long)expected,
                         (unsigned long)corner,
                         (unsigned long)expected_corner);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D10Triangle(argc, argv);
  Sleep(30);
  return rc;
}
