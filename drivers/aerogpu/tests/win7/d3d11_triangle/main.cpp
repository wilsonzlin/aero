#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_shader_compiler.h"
#include "..\\common\\aerogpu_test_shaders.h"

#include <d3d11.h>
#include <dxgi.h>

using aerogpu_test::ComPtr;

#ifndef DXGI_ERROR_WAS_STILL_DRAWING
  #define DXGI_ERROR_WAS_STILL_DRAWING ((HRESULT)0x887A000AL)
#endif

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

struct MapDoNotWaitThreadArgs {
  ID3D11DeviceContext* ctx;
  ID3D11Texture2D* tex;
  HRESULT hr;
  UINT row_pitch;
  uint32_t pixel;
  bool has_pixel;
};

static DWORD WINAPI MapDoNotWaitThreadProc(LPVOID param) {
  MapDoNotWaitThreadArgs* args = (MapDoNotWaitThreadArgs*)param;
  args->hr = E_FAIL;
  args->row_pitch = 0;
  args->pixel = 0;
  args->has_pixel = false;

  D3D11_MAPPED_SUBRESOURCE mapped;
  ZeroMemory(&mapped, sizeof(mapped));
  args->hr = args->ctx->Map(args->tex, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, &mapped);
  if (SUCCEEDED(args->hr) && mapped.pData) {
    args->row_pitch = mapped.RowPitch;
    args->pixel = aerogpu_test::ReadPixelBGRA(mapped.pData, (int)mapped.RowPitch, 5, 5);
    args->has_pixel = true;
    args->ctx->Unmap(args->tex, 0);
  }

  args->tex->Release();
  args->ctx->Release();
  args->tex = NULL;
  args->ctx = NULL;
  return 0;
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

static int RunD3D11Triangle(int argc, char** argv) {
  const char* kTestName = "d3d11_triangle";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D11Triangle",
                                              L"AeroGPU D3D11 Triangle",
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
    return reporter.FailHresult("D3D11CreateDeviceAndSwapChain(HARDWARE)", hr);
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

    // This test is specifically intended to exercise the D3D11 runtime path (d3d11.dll), which
    // should in turn use the UMD's OpenAdapter11 entrypoint.
    if (!GetModuleHandleW(L"d3d11.dll")) {
      return reporter.Fail("d3d11.dll is not loaded");
    }
    HMODULE umd = GetModuleHandleW(aerogpu_test::ExpectedAeroGpuD3D10UmdModuleBaseName());
    if (!umd) {
      return reporter.Fail("failed to locate loaded AeroGPU D3D10/11 UMD module");
    }
    FARPROC open_adapter_11 = GetProcAddress(umd, "OpenAdapter11");
    if (!open_adapter_11) {
      // On x86, stdcall decoration may be present depending on how the DLL was linked.
      open_adapter_11 = GetProcAddress(umd, "_OpenAdapter11@4");
    }
    if (!open_adapter_11) {
      return reporter.Fail("expected AeroGPU D3D10/11 UMD to export OpenAdapter11 (D3D11 entrypoint)");
    }
  }

  ComPtr<ID3D11Texture2D> backbuffer;
  hr = swapchain->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDXGISwapChain::GetBuffer", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(backbuffer.get(), NULL, rtv.put());
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

  // Compile shaders at runtime (no fxc.exe build-time dependency).
  const std::wstring dir = aerogpu_test::GetModuleDir();
  std::vector<unsigned char> vs_bytes;
  std::vector<unsigned char> ps_bytes;
  std::string shader_err;
  const char* vs_profile = (chosen_level >= D3D_FEATURE_LEVEL_10_0) ? "vs_4_0" : "vs_4_0_level_9_1";
  const char* ps_profile = (chosen_level >= D3D_FEATURE_LEVEL_10_0) ? "ps_4_0" : "ps_4_0_level_9_1";
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestBasicColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestBasicColorHlsl),
                                           "d3d11_triangle.hlsl",
                                           "vs_main",
                                           vs_profile,
                                           &vs_bytes,
                                           &shader_err)) {
    return reporter.Fail("failed to compile vertex shader: %s", shader_err.c_str());
  }
  if (!aerogpu_test::CompileHlslToBytecode(aerogpu_test::kAeroGpuTestBasicColorHlsl,
                                           strlen(aerogpu_test::kAeroGpuTestBasicColorHlsl),
                                           "d3d11_triangle.hlsl",
                                           "ps_main",
                                           ps_profile,
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

  context->IASetInputLayout(input_layout.get());
  context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

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

  // Read back the center pixel before present.
  D3D11_TEXTURE2D_DESC bb_desc;
  backbuffer->GetDesc(&bb_desc);
  if (bb_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
    return reporter.Fail("unexpected backbuffer format: %u (expected DXGI_FORMAT_B8G8R8A8_UNORM=%u)",
                         (unsigned)bb_desc.Format,
                         (unsigned)DXGI_FORMAT_B8G8R8A8_UNORM);
  }

  D3D11_TEXTURE2D_DESC st_desc = bb_desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  st_desc.Usage = D3D11_USAGE_STAGING;

  ComPtr<ID3D11Texture2D> staging;
  hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(staging)", hr);
  }

  context->CopyResource(staging.get(), backbuffer.get());

  // Probe DO_NOT_WAIT map before any explicit Flush call. A correct UMD should
  // either return DXGI_ERROR_WAS_STILL_DRAWING (in-flight copy) or succeed if the
  // work completed quickly. DO_NOT_WAIT must never block.
  {
    MapDoNotWaitThreadArgs args;
    ZeroMemory(&args, sizeof(args));
    args.ctx = context.get();
    args.tex = staging.get();
    args.hr = E_FAIL;
    context->AddRef();
    staging->AddRef();

    HANDLE thread = CreateThread(NULL, 0, &MapDoNotWaitThreadProc, &args, 0, NULL);
    if (!thread) {
      context->Release();
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
      if (!args.has_pixel) {
        return reporter.Fail("Map(staging, DO_NOT_WAIT) returned NULL pData");
      }
      const UINT min_row_pitch = bb_desc.Width * 4u;
      if (args.row_pitch < min_row_pitch) {
        return reporter.Fail("Map(staging, DO_NOT_WAIT) returned too-small RowPitch=%u (min=%u)",
                             (unsigned)args.row_pitch,
                             (unsigned)min_row_pitch);
      }
      const uint32_t expected_corner = 0xFFFF0000u;  // red in BGRA memory order
      if ((args.pixel & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
        return reporter.Fail("Map(staging, DO_NOT_WAIT) pixel mismatch at (5,5): got 0x%08lX expected ~0x%08lX",
                             (unsigned long)args.pixel,
                             (unsigned long)expected_corner);
      }
    } else {
      return FailD3D11WithRemovedReason(&reporter, kTestName, "Map(staging, DO_NOT_WAIT)", hr, device.get());
    }
  }

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
  const UINT min_row_pitch = bb_desc.Width * 4;
  if (map.RowPitch < min_row_pitch) {
    context->Unmap(staging.get(), 0);
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
    const std::wstring bmp_path = aerogpu_test::JoinPath(dir, L"d3d11_triangle.bmp");
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

    // Also dump a tightly-packed raw BGRA32 buffer for easier machine inspection.
    std::vector<uint8_t> tight((size_t)bb_desc.Width * (size_t)bb_desc.Height * 4u, 0);
    for (UINT y = 0; y < bb_desc.Height; ++y) {
      const uint8_t* src_row = (const uint8_t*)map.pData + (size_t)y * (size_t)map.RowPitch;
      memcpy(&tight[(size_t)y * (size_t)bb_desc.Width * 4u], src_row, (size_t)bb_desc.Width * 4u);
    }
    DumpBytesToFile(
        kTestName, &reporter, L"d3d11_triangle.bin", &tight[0], (UINT)tight.size());
  }

  context->Unmap(staging.get(), 0);

  hr = swapchain->Present(0, 0);
  if (FAILED(hr)) {
    return FailD3D11WithRemovedReason(&reporter, kTestName, "IDXGISwapChain::Present", hr, device.get());
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
  int rc = RunD3D11Triangle(argc, argv);
  Sleep(30);
  return rc;
}
