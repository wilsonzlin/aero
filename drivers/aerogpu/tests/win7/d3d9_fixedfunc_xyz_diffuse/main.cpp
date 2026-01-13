#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  DWORD color;
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

static int RunD3D9FixedFuncXyzDiffuse(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_xyz_diffuse";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncXyzDiffuse",
                                              L"AeroGPU D3D9 FixedFunc XYZ Diffuse",
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

  // No shaders: exercise the fixed-function fallback path for untransformed XYZ vertices.
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

  // Object-space vertices live far outside clip space; they only become visible if the WORLD
  // transform is applied (scale < 1, plus translation into clip space).
  const D3DMATRIX world = MakeScaleTranslateMatrix(0.25f, 0.25f, 1.0f, -1.0f, -1.0f, 0.0f);
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

  const DWORD kClear = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kDiffuse = D3DCOLOR_XRGB(16, 200, 40);  // Non-symmetric to catch channel ordering bugs.

  Vertex verts[3];
  verts[0].x = 2.0f;
  verts[0].y = 2.0f;
  verts[0].z = 0.5f;
  verts[0].color = kDiffuse;

  verts[1].x = 6.0f;
  verts[1].y = 2.0f;
  verts[1].z = 0.5f;
  verts[1].color = kDiffuse;

  verts[2].x = 4.0f;
  verts[2].y = 6.0f;
  verts[2].z = 0.5f;
  verts[2].color = kDiffuse;

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
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

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  const int kTol = 8;
  const bool center_ok = ColorWithinTolerance(center, kDiffuse, kTol);
  const bool corner_ok = ColorWithinTolerance(corner, kClear, kTol);
  if (!center_ok || !corner_ok) {
    if (dump) {
      std::string err;
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_xyz_diffuse.bmp");
      if (aerogpu_test::WriteBmp32BGRA(
              bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d9_fixedfunc_xyz_diffuse.bin",
                      lr.pBits,
                      (int)lr.Pitch,
                      (int)desc.Width,
                      (int)desc.Height);
    }
    sysmem->UnlockRect();
    return reporter.Fail(
        "pixel mismatch (tol=%d): center(%d,%d)=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
        kTol,
        cx,
        cy,
        (unsigned long)center,
        (unsigned long)kDiffuse,
        (unsigned long)corner,
        (unsigned long)kClear);
  }

  sysmem->UnlockRect();

  if (dump) {
    // Re-lock for dump (LockRect/UnlockRect can invalidate lr.pBits).
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_xyz_diffuse.bmp");
      if (aerogpu_test::WriteBmp32BGRA(
              bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      DumpTightBgra32(kTestName,
                      &reporter,
                      L"d3d9_fixedfunc_xyz_diffuse.bin",
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
  int rc = RunD3D9FixedFuncXyzDiffuse(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}

