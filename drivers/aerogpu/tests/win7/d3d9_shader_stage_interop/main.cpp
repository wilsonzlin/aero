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

  // Distinct colors; we sample the bottom-right texel (magenta).
  const D3DCOLOR kRed = 0xFFFF0000u;
  const D3DCOLOR kGreen = 0xFF00FF00u;
  const D3DCOLOR kYellow = 0xFFFFFF00u;
  const D3DCOLOR kMagenta = 0xFFFF00FFu;

  uint8_t* base = (uint8_t*)lr.pBits;
  D3DCOLOR* row0 = (D3DCOLOR*)base;
  D3DCOLOR* row1 = (D3DCOLOR*)(base + lr.Pitch);
  row0[0] = kRed;
  row0[1] = kGreen;
  row1[0] = kYellow;
  row1[1] = kMagenta;

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

static int RunD3D9ShaderStageInterop(int argc, char** argv) {
  const char* kTestName = "d3d9_shader_stage_interop";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ShaderStageInterop",
                                              L"AeroGPU D3D9 Shader Stage Interop",
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

  // Fixed-function stage0 setup: force stage0 to select texture (no vertex color
  // dependence). This is important because the test binds a user VS that only
  // writes oPos/oT0, and then leaves the PS stage NULL.
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);

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

  // Minimal vs_2_0:
  //   mov oPos, v0
  //   mov oT0, v2
  //   end
  static const DWORD kVsPosTex[] = {
      0xFFFE0200u, // vs_2_0
      0x03000001u, // mov
      0x400F0000u, // oPos.xyzw
      0x10E40000u, // v0.xyzw
      0x03000001u, // mov
      0x600F0000u, // oT0.xyzw
      0x10E40002u, // v2.xyzw (TEXCOORD0 when using XYZRHW|DIFFUSE|TEX1)
      0x0000FFFFu, // end
  };

  ComPtr<IDirect3DVertexShader9> vs;
  hr = dev->CreateVertexShader(kVsPosTex, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }

  hr = dev->SetVertexShader(vs.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader", hr);
  }

  // Exercise the interop path: VS is non-null, PS is NULL.
  hr = dev->SetPixelShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader(NULL)", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetFVF", hr);
  }

  Vertex verts[3];
  const DWORD kWhite = 0xFFFFFFFFu;

  verts[0].x = (float)kWidth * 0.25f;
  verts[0].y = (float)kHeight * 0.25f;
  verts[0].z = 0.5f;
  verts[0].rhw = 1.0f;
  verts[0].color = kWhite;
  verts[0].u = 0.75f;
  verts[0].v = 0.75f;

  verts[1] = verts[0];
  verts[1].x = (float)kWidth * 0.75f;

  verts[2] = verts[0];
  verts[2].x = (float)kWidth * 0.5f;
  verts[2].y = (float)kHeight * 0.75f;

  const DWORD kClear = D3DCOLOR_XRGB(255, 0, 0);
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }

  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetBackBuffer", hr);
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetDesc", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
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
    return reporter.FailHresult("LockRect", hr);
  }

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  // Expected: stage0 selects texture -> bottom-right texel (magenta).
  const uint32_t expected_center = 0xFFFF00FFu;
  const uint32_t expected_corner = 0xFFFF0000u;
  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    sysmem->UnlockRect();
    return reporter.Fail("pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                         (unsigned long)center,
                         (unsigned long)expected_center,
                         (unsigned long)corner,
                         (unsigned long)expected_corner);
  }

  sysmem->UnlockRect();

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ShaderStageInterop(argc, argv);
  aerogpu_test::FlushStdout();
  return rc;
}
