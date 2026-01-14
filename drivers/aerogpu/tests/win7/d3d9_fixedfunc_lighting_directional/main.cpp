#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <algorithm>
#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
};

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

static D3DMATRIX MakeIdentityMatrix() {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));
  m._11 = 1.0f;
  m._22 = 1.0f;
  m._33 = 1.0f;
  m._44 = 1.0f;
  return m;
}

static int RunD3D9FixedfuncLightingDirectional(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_lighting_directional";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedfuncLightingDirectional",
                                              L"AeroGPU D3D9 Fixedfunc Lighting (Directional)",
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

  // Fixed-function path.
  dev->SetVertexShader(NULL);
  dev->SetPixelShader(NULL);

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

  // Render state.
  hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_NORMAL);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF(D3DFVF_XYZ|D3DFVF_NORMAL)", hr);
  }

  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_LIGHTING, TRUE);
  dev->SetRenderState(D3DRS_AMBIENT, 0xFF202020u);

  dev->SetTexture(0, NULL);
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
  dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE);

  const D3DMATRIX identity = MakeIdentityMatrix();
  dev->SetTransform(D3DTS_WORLD, &identity);
  dev->SetTransform(D3DTS_VIEW, &identity);
  dev->SetTransform(D3DTS_PROJECTION, &identity);

  // Material + light (MVP subset: ambient+diffuse, directional light 0).
  D3DMATERIAL9 mat;
  ZeroMemory(&mat, sizeof(mat));
  mat.Diffuse.r = 0.5f;
  mat.Diffuse.g = 0.0f;
  mat.Diffuse.b = 0.0f;
  mat.Diffuse.a = 1.0f;
  mat.Ambient.r = 1.0f;
  mat.Ambient.g = 1.0f;
  mat.Ambient.b = 1.0f;
  mat.Ambient.a = 1.0f;
  hr = dev->SetMaterial(&mat);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetMaterial", hr);
  }

  D3DLIGHT9 light;
  ZeroMemory(&light, sizeof(light));
  light.Type = D3DLIGHT_DIRECTIONAL;
  light.Direction.x = 0.0f;
  light.Direction.y = 0.0f;
  light.Direction.z = -1.0f;
  light.Diffuse.r = 1.0f;
  light.Diffuse.g = 1.0f;
  light.Diffuse.b = 1.0f;
  light.Diffuse.a = 1.0f;
  hr = dev->SetLight(0, &light);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetLight(0)", hr);
  }
  hr = dev->LightEnable(0, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::LightEnable(0,TRUE)", hr);
  }

  // Triangle in clip space (identity WVP). Use a moderately large triangle that
  // covers the center pixel but not the corners.
  const Vertex tri[3] = {
      {-0.8f, 0.8f, 0.5f, 0.0f, 0.0f, 1.0f},
      {-0.8f, -0.8f, 0.5f, 0.0f, 0.0f, 1.0f},
      {0.8f, 0.0f, 0.5f, 0.0f, 0.0f, 1.0f},
  };

  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(sizeof(tri), 0, D3DFVF_XYZ | D3DFVF_NORMAL, D3DPOOL_DEFAULT, vb.put(), NULL);
  if (FAILED(hr) || !vb) {
    return reporter.FailHresult("CreateVertexBuffer(XYZ|NORMAL)", FAILED(hr) ? hr : E_FAIL);
  }
  void* vb_data = NULL;
  hr = vb->Lock(0, sizeof(tri), &vb_data, 0);
  if (FAILED(hr) || !vb_data) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_data, tri, sizeof(tri));
  vb->Unlock();

  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetStreamSource", hr);
  }

  const DWORD kClear = D3DCOLOR_XRGB(0, 0, 255);
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitive", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
  }

  // Read back before PresentEx (DISCARD swap effect makes post-Present contents undefined).
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

  // Expected output:
  //   out = material_ambient * global_ambient + material_diffuse * light_diffuse * ndotl
  // With:
  //   global_ambient = (0.125, 0.125, 0.125)
  //   material_ambient = (1,1,1)
  //   material_diffuse = (0.5,0,0)
  //   light_diffuse = (1,1,1)
  //   ndotl = 1 (normal=(0,0,1), light_dir=(0,0,-1))
  const DWORD kExpected = D3DCOLOR_XRGB(159, 32, 32);
  const int kTol = 12;
  const bool center_ok = ColorWithinTolerance(center, kExpected, kTol);
  const bool corner_ok = ColorWithinTolerance(corner, kClear, kTol);
  if (!center_ok || !corner_ok) {
    if (dump) {
      std::string err;
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_lighting_directional.bmp");
      if (aerogpu_test::WriteBmp32BGRA(bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
    }
    sysmem->UnlockRect();
    return reporter.Fail(
        "pixel mismatch (tol=%d): center(%d,%d)=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
        kTol,
        cx,
        cy,
        (unsigned long)center,
        (unsigned long)kExpected,
        (unsigned long)corner,
        (unsigned long)kClear);
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
  int rc = RunD3D9FixedfuncLightingDirectional(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}

