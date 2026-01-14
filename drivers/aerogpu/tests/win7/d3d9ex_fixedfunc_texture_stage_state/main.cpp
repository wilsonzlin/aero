#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
  float u;
  float v;
};

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
                            int row_pitch,
                            int width,
                            int height) {
  if (!data || width <= 0 || height <= 0 || row_pitch < width * 4) {
    return;
  }
  std::vector<uint8_t> tight((size_t)width * (size_t)height * 4u, 0);
  for (int y = 0; y < height; ++y) {
    const uint8_t* src_row = (const uint8_t*)data + (size_t)y * (size_t)row_pitch;
    memcpy(&tight[(size_t)y * (size_t)width * 4u], src_row, (size_t)width * 4u);
  }
  DumpBytesToFile(test_name, reporter, file_name, &tight[0], (UINT)tight.size());
}

static HRESULT CreateTestTexture2x2(IDirect3DDevice9Ex* dev, IDirect3DTexture9** out_tex) {
  if (!dev || !out_tex) {
    return E_INVALIDARG;
  }

  // Stage through a systemmem texture so UpdateTexture works even when the
  // default-pool texture is guest-backed.
  ComPtr<IDirect3DTexture9> sys_tex;
  HRESULT hr = dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys_tex->LockRect(0, &lr, NULL, 0);
  if (FAILED(hr)) {
    return hr;
  }

  // Distinct colors; we sample the bottom-right texel (blue).
  const D3DCOLOR kRed = 0xFFFF0000u;
  const D3DCOLOR kGreen = 0xFF00FF00u;
  const D3DCOLOR kMagenta = 0xFFFF00FFu;
  const D3DCOLOR kBlue = 0xFF0000FFu;

  uint8_t* base = (uint8_t*)lr.pBits;
  D3DCOLOR* row0 = (D3DCOLOR*)base;
  D3DCOLOR* row1 = (D3DCOLOR*)(base + lr.Pitch);
  row0[0] = kRed;
  row0[1] = kGreen;
  row1[0] = kMagenta;
  row1[1] = kBlue;

  sys_tex->UnlockRect(0);

  ComPtr<IDirect3DTexture9> gpu_tex;
  hr = dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, gpu_tex.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->UpdateTexture(sys_tex.get(), gpu_tex.get());
  if (FAILED(hr)) {
    return hr;
  }

  *out_tex = gpu_tex.detach();
  return S_OK;
}

static HRESULT DrawTriangleAndReadPixels(const char* test_name,
                                         aerogpu_test::TestReporter* reporter,
                                         IDirect3DDevice9Ex* dev,
                                         const Vertex* verts,
                                         D3DCOLOR clear_color,
                                         bool dump,
                                         const wchar_t* dump_prefix,
                                         D3DCOLOR* out_center,
                                         D3DCOLOR* out_corner) {
  if (!dev || !verts || !out_center || !out_corner) {
    return E_INVALIDARG;
  }

  HRESULT hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear_color, 1.0f, 0);
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
  if (FAILED(hr)) {
    dev->EndScene();
    return hr;
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return hr;
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return hr;
  }

  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return hr;
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return hr;
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width,
                                        desc.Height,
                                        desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return hr;
  }

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  *out_center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  *out_corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  if (dump && dump_prefix) {
    wchar_t bmp_name[128] = {};
    wchar_t bin_name[128] = {};
    _snwprintf(bmp_name, _countof(bmp_name) - 1, L"%ls.bmp", dump_prefix);
    _snwprintf(bin_name, _countof(bin_name) - 1, L"%ls.bin", dump_prefix);
    std::string err;
    const std::wstring bmp_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), bmp_name);
    if (aerogpu_test::WriteBmp32BGRA(bmp_path,
                                     (int)desc.Width,
                                     (int)desc.Height,
                                     lr.pBits,
                                     (int)lr.Pitch,
                                     &err)) {
      if (reporter) {
        reporter->AddArtifactPathW(bmp_path);
      }
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name, err.c_str());
    }
    DumpTightBgra32(test_name,
                    reporter,
                    bin_name,
                    lr.pBits,
                    (int)lr.Pitch,
                    (int)desc.Width,
                    (int)desc.Height);
  }

  sysmem->UnlockRect();
  return S_OK;
}

static bool PixelRgbEquals(D3DCOLOR actual, D3DCOLOR expected) {
  return (actual & 0x00FFFFFFu) == (expected & 0x00FFFFFFu);
}

static int RunD3D9ExFixedFuncTextureStageState(int argc, char** argv) {
  const char* kTestName = "d3d9ex_fixedfunc_texture_stage_state";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExFixedFuncTextureStageState",
                                              L"AeroGPU D3D9Ex FixedFunc TextureStageState",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("Direct3DCreate9Ex", hr);
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = kWidth;
  pp.BackBufferHeight = kHeight;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               kTestName,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    reporter.SetAdapterInfoA(ident.Description, ident.VendorId, ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return reporter.Fail(
          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
          (unsigned)ident.VendorId,
          (unsigned)ident.DeviceId);
    }
    if (!allow_non_aerogpu &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return reporter.Fail("adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu)",
                           ident.Description);
    }
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);

  // Diffuse red * (blue texel) = black.
  const DWORD kDiffuseRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kDiffuseBlue64 = D3DCOLOR_XRGB(0, 0, 64);
  const DWORD kDiffuseBlue128 = D3DCOLOR_XRGB(0, 0, 128);
  const DWORD kClearGreen = D3DCOLOR_XRGB(0, 255, 0);
  const DWORD kClearBlack = D3DCOLOR_XRGB(0, 0, 0);
  const DWORD kTexBlue = D3DCOLOR_ARGB(255, 0, 0, 255);
  const DWORD kDiffuseRedA128 = D3DCOLOR_ARGB(128, 255, 0, 0);
  const DWORD kDiffuseRedA32 = D3DCOLOR_ARGB(32, 255, 0, 0);
  const DWORD kDiffuseRedA64 = D3DCOLOR_ARGB(64, 255, 0, 0);
  const DWORD kHalfRed = D3DCOLOR_XRGB(128, 0, 0);
  const DWORD kQuarterRed = D3DCOLOR_XRGB(64, 0, 0);
  const DWORD kRed191 = D3DCOLOR_XRGB(191, 0, 0);
  const DWORD kMagenta = D3DCOLOR_XRGB(255, 0, 255);
  const DWORD kBlue128 = D3DCOLOR_XRGB(0, 0, 128);
  const DWORD kTfColor = D3DCOLOR_XRGB(12, 34, 56);

  Vertex verts[3];
  // Same coverage as d3d9ex_triangle: center pixel covered; top-left corner untouched.
  verts[0].x = (float)kWidth * 0.25f;
  verts[0].y = (float)kHeight * 0.25f;
  verts[0].z = 0.5f;
  verts[0].rhw = 1.0f;
  verts[0].color = kDiffuseRed;
  verts[0].u = 0.75f;
  verts[0].v = 0.75f;
  verts[1].x = (float)kWidth * 0.75f;
  verts[1].y = (float)kHeight * 0.25f;
  verts[1].z = 0.5f;
  verts[1].rhw = 1.0f;
  verts[1].color = kDiffuseRed;
  verts[1].u = 0.75f;
  verts[1].v = 0.75f;
  verts[2].x = (float)kWidth * 0.5f;
  verts[2].y = (float)kHeight * 0.75f;
  verts[2].z = 0.5f;
  verts[2].rhw = 1.0f;
  verts[2].color = kDiffuseRed;
  verts[2].u = 0.75f;
  verts[2].v = 0.75f;

  ComPtr<IDirect3DTexture9> tex0;
  hr = CreateTestTexture2x2(dev.get(), tex0.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTestTexture2x2", hr);
  }
  hr = dev->SetTexture(0, tex0.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture(0)", hr);
  }
  dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

  auto run_phase = [&](const char* phase_name, D3DCOLOR clear_color, D3DCOLOR expected_center) -> int {
    D3DCOLOR center = 0;
    D3DCOLOR corner = 0;
    wchar_t dump_prefix[128] = {};
    _snwprintf(dump_prefix, _countof(dump_prefix) - 1, L"%hs_%hs", kTestName, phase_name);

    HRESULT draw_hr = DrawTriangleAndReadPixels(kTestName,
                                                &reporter,
                                                dev.get(),
                                                verts,
                                                clear_color,
                                                dump,
                                                dump_prefix,
                                                &center,
                                                &corner);
    if (FAILED(draw_hr)) {
      return reporter.FailHresult(phase_name, draw_hr);
    }

    if (!PixelRgbEquals(corner, clear_color)) {
      return reporter.Fail("%s: corner pixel mismatch: got=0x%08lX expected(clear)=0x%08lX",
                           phase_name,
                           (unsigned long)corner,
                           (unsigned long)clear_color);
    }

    if (!PixelRgbEquals(center, expected_center)) {
      return reporter.Fail("%s: center pixel mismatch: got=0x%08lX expected=0x%08lX",
                           phase_name,
                           (unsigned long)center,
                           (unsigned long)expected_center);
    }

    return 0;
  };

  // Stage0 MODULATE: TEXTURE * DIFFUSE.
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=MODULATE)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG2=DIFFUSE)", hr);
  }
  int rc = run_phase("modulate", kClearGreen, D3DCOLOR_XRGB(0, 0, 0));
  if (rc != 0) {
    return rc;
  }

  // Switch to SELECTARG1=TEXTURE.
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=SELECTARG1)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE) (select)", hr);
  }
  rc = run_phase("select_texture", kClearGreen, kTexBlue);
  if (rc != 0) {
    return rc;
  }

  // Switch to SELECTARG1=DIFFUSE.
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=SELECTARG1) (diffuse)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=DIFFUSE)", hr);
  }
  rc = run_phase("select_diffuse", kClearGreen, kDiffuseRed);
  if (rc != 0) {
    return rc;
  }

  // ADD: TEXTURE + DIFFUSE (blue + red => magenta).
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_ADD);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=ADD)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE) (add)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG2=DIFFUSE) (add)", hr);
  }
  // Keep sampling blue texel.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRed;
    verts[i].u = 0.75f;
    verts[i].v = 0.75f;
  }
  rc = run_phase("add", kClearGreen, kMagenta);
  if (rc != 0) {
    return rc;
  }

  // SUBTRACT: TEXTURE - DIFFUSE, sampling magenta texel (magenta - half-blue => (255,0,127)).
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SUBTRACT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=SUBTRACT)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE) (subtract)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG2=DIFFUSE) (subtract)", hr);
  }
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseBlue128;
    verts[i].u = 0.25f; // bottom-left texel (magenta)
    verts[i].v = 0.75f;
  }
  rc = run_phase("subtract", kClearGreen, D3DCOLOR_XRGB(255, 0, 127));
  if (rc != 0) {
    return rc;
  }

  // MODULATE2X and MODULATE4X: sample blue texel and scale a low-intensity diffuse.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseBlue64;
    verts[i].u = 0.75f; // bottom-right texel (blue)
    verts[i].v = 0.75f;
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE2X);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=MODULATE2X)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE) (mod2x)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG2=DIFFUSE) (mod2x)", hr);
  }
  rc = run_phase("modulate2x", kClearGreen, kBlue128);
  if (rc != 0) {
    return rc;
  }

  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE4X);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=MODULATE4X)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TEXTURE) (mod4x)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG2=DIFFUSE) (mod4x)", hr);
  }
  rc = run_phase("modulate4x", kClearGreen, kTexBlue);
  if (rc != 0) {
    return rc;
  }

  // TFACTOR source: SELECTARG1=TFACTOR.
  hr = dev->SetRenderState(D3DRS_TEXTUREFACTOR, kTfColor);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(TEXTUREFACTOR)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=SELECTARG1) (tfactor)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TFACTOR);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=TFACTOR)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=SELECTARG1) (tfactor)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TFACTOR);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=TFACTOR)", hr);
  }
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRed;
    verts[i].u = 0.75f;
    verts[i].v = 0.75f;
  }
  rc = run_phase("tfactor", kClearGreen, kTfColor);
  if (rc != 0) {
    return rc;
  }

  // Alpha-op coverage via alpha blending. Keep RGB fixed (DIFFUSE) and vary ALPHAOP
  // to affect the blend factor.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRedA128;
    verts[i].u = 0.75f;
    verts[i].v = 0.75f;
  }
  hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(ALPHABLENDENABLE=TRUE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(SRCBLEND=SRCALPHA)", hr);
  }
  hr = dev->SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(DESTBLEND=INVSRCALPHA)", hr);
  }

  // RGB=DIFFUSE, A=TEXTURE -> alpha=1.0 (texture is opaque) => full red over black.
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=SELECTARG1) (alpha)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLORARG1=DIFFUSE) (alpha)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=SELECTARG1) (texture)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=TEXTURE)", hr);
  }
  rc = run_phase("alpha_texture", kClearBlack, kDiffuseRed);
  if (rc != 0) {
    return rc;
  }

  // RGB=DIFFUSE, A=DIFFUSE -> alpha=0.5 => half red over black.
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=SELECTARG1) (diffuse)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=DIFFUSE)", hr);
  }
  rc = run_phase("alpha_diffuse", kClearBlack, kHalfRed);
  if (rc != 0) {
    return rc;
  }

  // ALPHAOP=MODULATE2X (TEXTURE * DIFFUSE * 2): with diffuse alpha=0.125 and texture alpha=1.0 => alpha=0.25.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRedA32;
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_MODULATE2X);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=MODULATE2X)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=TEXTURE) (mod2x)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG2=DIFFUSE) (mod2x)", hr);
  }
  rc = run_phase("alpha_modulate2x", kClearBlack, kQuarterRed);
  if (rc != 0) {
    return rc;
  }

  // ALPHAOP=SUBTRACT (TEXTURE - DIFFUSE): with diffuse alpha=0.25 and texture alpha=1.0 => alpha=0.75.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRedA64;
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SUBTRACT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=SUBTRACT)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=TEXTURE) (subtract)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG2=DIFFUSE) (subtract)", hr);
  }
  rc = run_phase("alpha_subtract", kClearBlack, kRed191);
  if (rc != 0) {
    return rc;
  }

  // Restore alpha=0.5 for the DISABLE coverage below.
  for (int i = 0; i < 3; ++i) {
    verts[i].color = kDiffuseRedA128;
  }

  // COLOROP=DISABLE disables the stage entirely, so ALPHAOP must be ignored and
  // alpha should come from diffuse/current (0.5).
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAOP=SELECTARG1) (disable)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(ALPHAARG1=TEXTURE) (disable)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_DISABLE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(COLOROP=DISABLE)", hr);
  }
  rc = run_phase("colorop_disable", kClearBlack, kHalfRed);
  if (rc != 0) {
    return rc;
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExFixedFuncTextureStageState(argc, argv);
  Sleep(30);
  return rc;
}
