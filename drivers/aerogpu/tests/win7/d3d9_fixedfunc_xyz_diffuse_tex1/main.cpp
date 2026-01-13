#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
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

static D3DMATRIX MakeIdentityMatrix() {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));
  m._11 = 1.0f;
  m._22 = 1.0f;
  m._33 = 1.0f;
  m._44 = 1.0f;
  return m;
}

static D3DMATRIX MakeScaleTranslateMatrix(float sx, float sy, float sz, float tx, float ty, float tz) {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));
  m._11 = sx;
  m._22 = sy;
  m._33 = sz;
  m._44 = 1.0f;
  m._41 = tx;
  m._42 = ty;
  m._43 = tz;
  return m;
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

static uint8_t ModulateChan(uint8_t a, uint8_t b) {
  // Fixed-function D3DTOP_MODULATE nominally does (a*b)/255. Exact rounding can vary by hardware;
  // keep this close and rely on a small tolerance in comparisons.
  const int v = (int)a * (int)b;
  return (uint8_t)((v + 127) / 255);
}

static D3DCOLOR ModulateRgb(D3DCOLOR tex, D3DCOLOR diffuse) {
  const uint8_t tr = (uint8_t)((tex >> 16) & 0xFFu);
  const uint8_t tg = (uint8_t)((tex >> 8) & 0xFFu);
  const uint8_t tb = (uint8_t)((tex >> 0) & 0xFFu);
  const uint8_t dr = (uint8_t)((diffuse >> 16) & 0xFFu);
  const uint8_t dg = (uint8_t)((diffuse >> 8) & 0xFFu);
  const uint8_t db = (uint8_t)((diffuse >> 0) & 0xFFu);
  return D3DCOLOR_XRGB(ModulateChan(tr, dr), ModulateChan(tg, dg), ModulateChan(tb, db));
}

static HRESULT CreateTestTexture2x2(IDirect3DDevice9Ex* dev, IDirect3DTexture9** out_tex) {
  if (!dev || !out_tex) {
    return E_INVALIDARG;
  }

  // Stage through a systemmem texture so UpdateTexture works even when the
  // default-pool texture is guest-backed.
  ComPtr<IDirect3DTexture9> sys_tex;
  HRESULT hr =
      dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys_tex->LockRect(0, &lr, NULL, 0);
  if (FAILED(hr)) {
    return hr;
  }

  // Distinct 2x2 pattern (top-left origin in D3D9 texture coordinates):
  //   [R G]
  //   [B W]
  const D3DCOLOR kRed = 0xFFFF0000u;
  const D3DCOLOR kGreen = 0xFF00FF00u;
  const D3DCOLOR kBlue = 0xFF0000FFu;
  const D3DCOLOR kWhite = 0xFFFFFFFFu;

  uint8_t* base = (uint8_t*)lr.pBits;
  D3DCOLOR* row0 = (D3DCOLOR*)base;
  D3DCOLOR* row1 = (D3DCOLOR*)(base + lr.Pitch);
  row0[0] = kRed;
  row0[1] = kGreen;
  row1[0] = kBlue;
  row1[1] = kWhite;

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

static int RunD3D9FixedFuncXyzDiffuseTex1(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_xyz_diffuse_tex1";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncXyzDiffuseTex1",
                                              L"AeroGPU D3D9 FixedFunc XYZ Diffuse Tex1",
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

  // No shaders: fixed-function pipeline with untransformed XYZ vertices + WVP transforms.
  dev->SetVertexShader(NULL);
  dev->SetPixelShader(NULL);

  // Basic fixed-function state.
  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_COLORVERTEX, TRUE);

  // WORLD transform maps object coordinates (2..10) into clip space (-1..1). Vertices are
  // completely outside clip space if transforms are ignored.
  const D3DMATRIX world = MakeScaleTranslateMatrix(0.25f, 0.25f, 1.0f, -1.5f, -1.5f, 0.0f);
  const D3DMATRIX view = MakeIdentityMatrix();
  const D3DMATRIX proj = MakeIdentityMatrix();
  hr = dev->SetTransform(D3DTS_WORLD, &world);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(WORLD)", hr);
  }
  hr = dev->SetTransform(D3DTS_VIEW, &view);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(VIEW)", hr);
  }
  hr = dev->SetTransform(D3DTS_PROJECTION, &proj);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(PROJECTION)", hr);
  }

  ComPtr<IDirect3DTexture9> tex;
  hr = CreateTestTexture2x2(dev.get(), tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTestTexture2x2", hr);
  }

  hr = dev->SetTexture(0, tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTexture", hr);
  }

  dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

  // Make the modulate explicit (default is modulate, but keep this test self-contained).
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
  dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_DISABLE);
  dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE);
  dev->SetTextureStageState(1, D3DTSS_ALPHAOP, D3DTOP_DISABLE);

  const DWORD kClear = D3DCOLOR_XRGB(0, 0, 0);
  const DWORD kDiffuse = D3DCOLOR_XRGB(128, 64, 192);

  Vertex quad[6];
  // Triangle 1: TL, TR, BL
  quad[0].x = 2.0f;
  quad[0].y = 10.0f;
  quad[0].z = 0.5f;
  quad[0].color = kDiffuse;
  quad[0].u = 0.0f;
  quad[0].v = 0.0f;

  quad[1] = quad[0];
  quad[1].x = 10.0f;
  quad[1].u = 1.0f;

  quad[2] = quad[0];
  quad[2].y = 2.0f;
  quad[2].v = 1.0f;

  // Triangle 2: BL, TR, BR
  quad[3] = quad[2];

  quad[4] = quad[1];

  quad[5] = quad[2];
  quad[5].x = 10.0f;
  quad[5].u = 1.0f;

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 2, quad, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
  }

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

  const int x0 = (int)desc.Width / 4;
  const int x1 = (int)desc.Width * 3 / 4;
  const int y0 = (int)desc.Height / 4;
  const int y1 = (int)desc.Height * 3 / 4;

  const uint32_t tl = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, x0, y0);
  const uint32_t tr = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, x1, y0);
  const uint32_t bl = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, x0, y1);
  const uint32_t br = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, x1, y1);

  const D3DCOLOR tex_tl = 0xFFFF0000u;
  const D3DCOLOR tex_tr = 0xFF00FF00u;
  const D3DCOLOR tex_bl = 0xFF0000FFu;
  const D3DCOLOR tex_br = 0xFFFFFFFFu;

  const D3DCOLOR expected_tl = ModulateRgb(tex_tl, kDiffuse);
  const D3DCOLOR expected_tr = ModulateRgb(tex_tr, kDiffuse);
  const D3DCOLOR expected_bl = ModulateRgb(tex_bl, kDiffuse);
  const D3DCOLOR expected_br = ModulateRgb(tex_br, kDiffuse);

  const int kTol = 8;
  const bool ok_tl = ColorWithinTolerance(tl, expected_tl, kTol);
  const bool ok_tr = ColorWithinTolerance(tr, expected_tr, kTol);
  const bool ok_bl = ColorWithinTolerance(bl, expected_bl, kTol);
  const bool ok_br = ColorWithinTolerance(br, expected_br, kTol);

  if (!ok_tl || !ok_tr || !ok_bl || !ok_br) {
    if (dump) {
      std::string err;
      const std::wstring bmp_path = aerogpu_test::JoinPath(
          aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_xyz_diffuse_tex1.bmp");
      if (aerogpu_test::WriteBmp32BGRA(
              bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d9_fixedfunc_xyz_diffuse_tex1.bin",
                      lr.pBits,
                      (int)lr.Pitch,
                      (int)desc.Width,
                      (int)desc.Height);
    }
    sysmem->UnlockRect();
    return reporter.Fail(
        "pixel mismatch (tol=%d): TL(%d,%d)=0x%08lX expected 0x%08lX; TR(%d,%d)=0x%08lX expected 0x%08lX; "
        "BL(%d,%d)=0x%08lX expected 0x%08lX; BR(%d,%d)=0x%08lX expected 0x%08lX",
        kTol,
        x0,
        y0,
        (unsigned long)tl,
        (unsigned long)expected_tl,
        x1,
        y0,
        (unsigned long)tr,
        (unsigned long)expected_tr,
        x0,
        y1,
        (unsigned long)bl,
        (unsigned long)expected_bl,
        x1,
        y1,
        (unsigned long)br,
        (unsigned long)expected_br);
  }

  sysmem->UnlockRect();

  if (dump) {
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      const std::wstring bmp_path = aerogpu_test::JoinPath(
          aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_xyz_diffuse_tex1.bmp");
      if (aerogpu_test::WriteBmp32BGRA(
              bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d9_fixedfunc_xyz_diffuse_tex1.bin",
                      lr.pBits,
                      (int)lr.Pitch,
                      (int)desc.Width,
                      (int)desc.Height);
      sysmem->UnlockRect();
    }
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9FixedFuncXyzDiffuseTex1(argc, argv);
  Sleep(30);
  return rc;
}

