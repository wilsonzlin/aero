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
};

static DWORD FloatBits(float f) {
  DWORD u = 0;
  memcpy(&u, &f, sizeof(u));
  return u;
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

static int AbsInt(int v) { return v < 0 ? -v : v; }

static bool ColorWithinTolerance(D3DCOLOR got, D3DCOLOR expected, int tol) {
  const int gr = (int)((got >> 16) & 0xFFu);
  const int gg = (int)((got >> 8) & 0xFFu);
  const int gb = (int)((got >> 0) & 0xFFu);
  const int er = (int)((expected >> 16) & 0xFFu);
  const int eg = (int)((expected >> 8) & 0xFFu);
  const int eb = (int)((expected >> 0) & 0xFFu);
  return AbsInt(gr - er) <= tol && AbsInt(gg - eg) <= tol && AbsInt(gb - eb) <= tol;
}

static uint8_t ClampU8(int v) {
  if (v < 0) return 0;
  if (v > 255) return 255;
  return (uint8_t)v;
}

static float Clamp01(float v) {
  if (v < 0.0f) return 0.0f;
  if (v > 1.0f) return 1.0f;
  return v;
}

static D3DCOLOR LerpRgb(D3DCOLOR src, D3DCOLOR fog, float t) {
  const float k = Clamp01(t);
  const float inv = 1.0f - k;

  const int sr = (int)((src >> 16) & 0xFFu);
  const int sg = (int)((src >> 8) & 0xFFu);
  const int sb = (int)((src >> 0) & 0xFFu);
  const int fr = (int)((fog >> 16) & 0xFFu);
  const int fg = (int)((fog >> 8) & 0xFFu);
  const int fb = (int)((fog >> 0) & 0xFFu);

  const int r = (int)(sr * inv + fr * k + 0.5f);
  const int g = (int)(sg * inv + fg * k + 0.5f);
  const int b = (int)(sb * inv + fb * k + 0.5f);

  const DWORD a = src & 0xFF000000u;
  return a | ((DWORD)ClampU8(r) << 16) | ((DWORD)ClampU8(g) << 8) | ((DWORD)ClampU8(b) << 0);
}

static int RunD3D9FixedfuncFogSmoke(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_fog_smoke";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool strict_checks = require_umd || (!allow_microsoft && !allow_non_aerogpu);

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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedfuncFogSmoke",
                                              L"AeroGPU D3D9 Fixedfunc Fog Smoke",
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
    if (has_require_vid && ident.VendorId != require_vid) {
      return reporter.Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.VendorId,
                           (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return reporter.Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.DeviceId,
                           (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return reporter.Fail(
          "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu or use --require-vid/--require-did)",
          ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return reporter.FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  if (strict_checks) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  D3DVIEWPORT9 vp;
  ZeroMemory(&vp, sizeof(vp));
  vp.X = 0;
  vp.Y = 0;
  vp.Width = kWidth;
  vp.Height = kHeight;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = dev->SetViewport(&vp);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetViewport", hr);
  }

  // No shaders: exercise the fixed-function fallback path for XYZRHW vertices.
  dev->SetVertexShader(NULL);
  dev->SetPixelShader(NULL);

  // Basic fixed-function render state.
  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_COLORVERTEX, TRUE);
  dev->SetTexture(0, NULL);
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
  dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE);

  // Fixed-function fog (linear).
  const float fog_start = 0.2f;
  const float fog_end = 0.8f;
  const D3DCOLOR fog_color = D3DCOLOR_XRGB(255, 0, 0);
  dev->SetRenderState(D3DRS_FOGENABLE, TRUE);
  dev->SetRenderState(D3DRS_FOGTABLEMODE, D3DFOG_LINEAR);
  dev->SetRenderState(D3DRS_FOGSTART, FloatBits(fog_start));
  dev->SetRenderState(D3DRS_FOGEND, FloatBits(fog_end));
  dev->SetRenderState(D3DRS_FOGCOLOR, fog_color);

  const D3DCOLOR clear = D3DCOLOR_XRGB(0, 0, 0);
  const D3DCOLOR diffuse = D3DCOLOR_XRGB(0, 255, 0);

  const float z_near = 0.25f;
  const float z_far = 0.75f;

  // Two quads: near on the left, far on the right.
  const float left_x0 = 20.0f;
  const float left_x1 = 120.0f;
  const float right_x0 = 136.0f;
  const float right_x1 = 236.0f;
  const float y0 = 60.0f;
  const float y1 = 190.0f;

  Vertex verts[12];
  // Near quad.
  verts[0] = {left_x0, y0, z_near, 1.0f, diffuse};
  verts[1] = {left_x1, y0, z_near, 1.0f, diffuse};
  verts[2] = {left_x0, y1, z_near, 1.0f, diffuse};
  verts[3] = {left_x1, y0, z_near, 1.0f, diffuse};
  verts[4] = {left_x1, y1, z_near, 1.0f, diffuse};
  verts[5] = {left_x0, y1, z_near, 1.0f, diffuse};
  // Far quad.
  verts[6] = {right_x0, y0, z_far, 1.0f, diffuse};
  verts[7] = {right_x1, y0, z_far, 1.0f, diffuse};
  verts[8] = {right_x0, y1, z_far, 1.0f, diffuse};
  verts[9] = {right_x1, y0, z_far, 1.0f, diffuse};
  verts[10] = {right_x1, y1, z_far, 1.0f, diffuse};
  verts[11] = {right_x0, y1, z_far, 1.0f, diffuse};

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 4, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
  }

  // Read back the backbuffer before PresentEx: for D3DSWAPEFFECT_DISCARD the contents after Present
  // are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::GetBackBuffer", hr);
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DSurface9::GetDesc", hr);
  }
  if (desc.Format != D3DFMT_X8R8G8B8 && desc.Format != D3DFMT_A8R8G8B8) {
    return reporter.Fail("unexpected backbuffer format: %lu", (unsigned long)desc.Format);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(
      desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DSurface9::LockRect", hr);
  }

  // Sample inside each quad.
  const int near_x = (int)((left_x0 + left_x1) * 0.5f);
  const int far_x = (int)((right_x0 + right_x1) * 0.5f);
  const int sample_y = (int)((y0 + y1) * 0.5f);
  const uint32_t near_px = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, near_x, sample_y);
  const uint32_t far_px = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, far_x, sample_y);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  const float inv_range = (fog_end != fog_start) ? (1.0f / (fog_end - fog_start)) : 0.0f;
  const float near_amount = Clamp01((z_near - fog_start) * inv_range);
  const float far_amount = Clamp01((z_far - fog_start) * inv_range);
  const D3DCOLOR expected_near = LerpRgb(diffuse, fog_color, near_amount);
  const D3DCOLOR expected_far = LerpRgb(diffuse, fog_color, far_amount);

  const int kTol = 24;
  const bool near_ok = ColorWithinTolerance(near_px, expected_near, kTol);
  const bool far_ok = ColorWithinTolerance(far_px, expected_far, kTol);
  const bool corner_ok = ColorWithinTolerance(corner, clear, /*tol=*/8);
  if (!near_ok || !far_ok || !corner_ok) {
    if (dump) {
      std::string err;
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_fog_smoke.bmp");
      if (aerogpu_test::WriteBmp32BGRA(
              bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d9_fixedfunc_fog_smoke.bin",
                      lr.pBits,
                      (int)lr.Pitch,
                      (int)desc.Width,
                      (int)desc.Height);
    }
    sysmem->UnlockRect();
    return reporter.Fail(
        "pixel mismatch (tol=%d): near(%d,%d)=0x%08lX expected 0x%08lX; far(%d,%d)=0x%08lX expected 0x%08lX; corner=0x%08lX expected 0x%08lX",
        kTol,
        near_x,
        sample_y,
        (unsigned long)near_px,
        (unsigned long)expected_near,
        far_x,
        sample_y,
        (unsigned long)far_px,
        (unsigned long)expected_far,
        (unsigned long)corner,
        (unsigned long)clear);
  }

  sysmem->UnlockRect();

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9FixedfuncFogSmoke(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}

