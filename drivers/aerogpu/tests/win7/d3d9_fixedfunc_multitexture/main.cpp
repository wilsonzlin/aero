#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
  float u;
  float v;
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

static uint8_t ModulateChan(uint8_t a, uint8_t b) {
  // D3DTOP_MODULATE nominally performs (a*b)/255 with implementation-defined rounding.
  // Use a common "(v + 127) / 255" rounding and allow a small tolerance at comparison.
  const int v = (int)a * (int)b;
  return (uint8_t)((v + 127) / 255);
}

static D3DCOLOR ModulateRgb(D3DCOLOR a, D3DCOLOR b) {
  const uint8_t ar = (uint8_t)((a >> 16) & 0xFFu);
  const uint8_t ag = (uint8_t)((a >> 8) & 0xFFu);
  const uint8_t ab = (uint8_t)((a >> 0) & 0xFFu);
  const uint8_t br = (uint8_t)((b >> 16) & 0xFFu);
  const uint8_t bg = (uint8_t)((b >> 8) & 0xFFu);
  const uint8_t bb = (uint8_t)((b >> 0) & 0xFFu);
  return D3DCOLOR_XRGB(ModulateChan(ar, br), ModulateChan(ag, bg), ModulateChan(ab, bb));
}

static HRESULT CreateSolidTexture1x1(IDirect3DDevice9Ex* dev, D3DCOLOR color, IDirect3DTexture9** out_tex) {
  if (!dev || !out_tex) {
    return E_INVALIDARG;
  }

  // Stage through a systemmem texture so UpdateTexture works even when the
  // default-pool texture is guest-backed.
  ComPtr<IDirect3DTexture9> sys_tex;
  HRESULT hr = dev->CreateTexture(1, 1, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys_tex->LockRect(0, &lr, NULL, 0);
  if (FAILED(hr)) {
    return hr;
  }
  *(D3DCOLOR*)lr.pBits = color;
  sys_tex->UnlockRect(0);

  ComPtr<IDirect3DTexture9> gpu_tex;
  hr = dev->CreateTexture(1, 1, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, gpu_tex.put(), NULL);
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

static int RunD3D9FixedFuncMultitexture(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_multitexture";
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

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncMultitexture",
                                              L"AeroGPU D3D9 FixedFunc MultiTexture",
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
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                           D3DDEVTYPE_HAL,
                           hwnd,
                           create_flags,
                           &pp,
                           NULL,
                           dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             &pp,
                             NULL,
                             dev.put());
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

  // Force fixed-function (no user shaders).
  hr = dev->SetVertexShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetVertexShader(NULL)", hr);
  }
  hr = dev->SetPixelShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetPixelShader(NULL)", hr);
  }

  hr = dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetRenderState(LIGHTING=FALSE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetRenderState(CULLMODE=NONE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetRenderState(ALPHABLENDENABLE=FALSE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetRenderState(ZENABLE=FALSE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_SRGBWRITEENABLE, FALSE);
  if (FAILED(hr)) {
    // Not all devices support sRGB writes; the D3D9 default is disabled.
    aerogpu_test::PrintfStdout("INFO: %s: SetRenderState(SRGBWRITEENABLE=FALSE) failed: %s",
                               kTestName,
                               aerogpu_test::HresultToString(hr).c_str());
  }

  // Two solid textures with non-trivial RGB values so MODULATE yields a distinct color.
  const D3DCOLOR kTex0 = D3DCOLOR_XRGB(200, 100, 50);
  const D3DCOLOR kTex1 = D3DCOLOR_XRGB(128, 200, 80);
  const D3DCOLOR kExpected = ModulateRgb(kTex0, kTex1);

  ComPtr<IDirect3DTexture9> tex0;
  ComPtr<IDirect3DTexture9> tex1;
  hr = CreateSolidTexture1x1(dev.get(), kTex0, tex0.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateSolidTexture1x1(tex0)", hr);
  }
  hr = CreateSolidTexture1x1(dev.get(), kTex1, tex1.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateSolidTexture1x1(tex1)", hr);
  }

  hr = dev->SetTexture(0, tex0.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTexture(stage0)", hr);
  }
  hr = dev->SetTexture(1, tex1.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTexture(stage1)", hr);
  }

  // Point sampling so results are deterministic.
  for (DWORD stage = 0; stage < 2; ++stage) {
    hr = dev->SetSamplerState(stage, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(MINFILTER=POINT)", hr);
    }
    hr = dev->SetSamplerState(stage, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(MAGFILTER=POINT)", hr);
    }
    hr = dev->SetSamplerState(stage, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(MIPFILTER=NONE)", hr);
    }
    hr = dev->SetSamplerState(stage, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(ADDRESSU=CLAMP)", hr);
    }
    hr = dev->SetSamplerState(stage, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(ADDRESSV=CLAMP)", hr);
    }
  }

  // Stage 0: CURRENT = tex0.
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0 COLOROP=SELECTARG1)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0 COLORARG1=TEXTURE)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0 ALPHAOP=SELECTARG1)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0 ALPHAARG1=TEXTURE)", hr);
  }

  // Stage 1: CURRENT = tex1 * CURRENT.
  hr = dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_MODULATE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage1 COLOROP=MODULATE)", hr);
  }
  hr = dev->SetTextureStageState(1, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage1 COLORARG1=TEXTURE)", hr);
  }
  hr = dev->SetTextureStageState(1, D3DTSS_COLORARG2, D3DTA_CURRENT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage1 COLORARG2=CURRENT)", hr);
  }

  // Disable stage 2 to terminate the combiner chain.
  hr = dev->SetTextureStageState(2, D3DTSS_COLOROP, D3DTOP_DISABLE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage2 COLOROP=DISABLE)", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF(XYZRHW|DIFFUSE|TEX1)", hr);
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, 0x00000000u, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  const VertexXyzrhwDiffuseTex1 quad[6] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {(float)kWidth, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {(float)kWidth, (float)kHeight, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 1.0f},

      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {(float)kWidth, (float)kHeight, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 1.0f},
      {0.0f, (float)kHeight, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 2, quad, sizeof(quad[0]));
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

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface(sysmem)", hr);
  }

  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DSurface9::LockRect(sysmem)", hr);
  }

  const int sample_x = kWidth / 2;
  const int sample_y = kHeight / 2;
  const D3DCOLOR got = (D3DCOLOR)aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, sample_x, sample_y);
  sysmem->UnlockRect();

  if (!ColorWithinTolerance(got, kExpected, /*tol=*/6)) {
    return reporter.Fail("center pixel mismatch: got=0x%08lX expected~=0x%08lX",
                         (unsigned long)got,
                         (unsigned long)kExpected);
  }

  if (dump) {
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      const std::wstring bmp_path =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_multitexture.bmp");
      if (aerogpu_test::WriteBmp32BGRA(bmp_path,
                                       (int)desc.Width,
                                       (int)desc.Height,
                                       lr.pBits,
                                       (int)lr.Pitch,
                                       &err)) {
        reporter.AddArtifactPathW(bmp_path);
      } else {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
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
  int rc = RunD3D9FixedFuncMultitexture(argc, argv);
  Sleep(30);
  return rc;
}

