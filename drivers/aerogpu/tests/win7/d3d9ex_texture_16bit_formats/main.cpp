#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <cstring>

using aerogpu_test::ComPtr;

struct VertexPosTex {
  float x;
  float y;
  float z;
  float w;
  float u;
  float v;
  float tu2;
  float tv2;
};

static const DWORD kVsCopyPosTex[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, 0x400F0000u, 0x10E40000u, // mov oPos, v0
    0x03000001u, 0x600F0000u, 0x10E40001u, // mov oT0, v1
    0x0000FFFFu, // end
};

static const DWORD kPsCopyTex[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, 0x000F0000u, 0x30E40000u, 0x20E40800u, // texld r0, t0, s0
    0x03000001u, 0x000F0800u, 0x00E40000u, // mov oC0, r0
    0x0000FFFFu, // end
};

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         DWORD create_flags,
                                         IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    DWORD fallback_flags = create_flags;
    fallback_flags &= ~D3DCREATE_HARDWARE_VERTEXPROCESSING;
    fallback_flags |= D3DCREATE_SOFTWARE_VERTEXPROCESSING;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             fallback_flags,
                             pp,
                             NULL,
                             out_dev);
  }
  return hr;
}

static HRESULT ReadBackbufferCenterPixel(IDirect3DDevice9Ex* dev, D3DCOLOR* out_pixel) {
  if (!dev || !out_pixel) {
    return E_INVALIDARG;
  }

  ComPtr<IDirect3DSurface9> rt;
  HRESULT hr = dev->GetRenderTarget(0, rt.put());
  if (FAILED(hr)) {
    return hr;
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = rt->GetDesc(&desc);
  if (FAILED(hr)) {
    return hr;
  }

  ComPtr<IDirect3DSurface9> sys;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sys.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->GetRenderTargetData(rt.get(), sys.get());
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr) || !lr.pBits) {
    return FAILED(hr) ? hr : E_FAIL;
  }

  const UINT x = desc.Width / 2;
  const UINT y = desc.Height / 2;
  const uint8_t* row = (const uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
  *out_pixel = ((const D3DCOLOR*)row)[x];

  sys->UnlockRect();
  return S_OK;
}

static HRESULT DrawFullscreenQuad(IDirect3DDevice9Ex* dev, D3DCOLOR clear_color) {
  if (!dev) {
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
  hr = dev->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
  dev->EndScene();
  if (FAILED(hr)) {
    return hr;
  }

  // Ensure the draw is submitted before readback.
  dev->Flush();
  return S_OK;
}

static inline uint8_t ChannelR(D3DCOLOR c) { return (uint8_t)((c >> 16) & 0xFFu); }
static inline uint8_t ChannelG(D3DCOLOR c) { return (uint8_t)((c >> 8) & 0xFFu); }
static inline uint8_t ChannelB(D3DCOLOR c) { return (uint8_t)(c & 0xFFu); }

static bool IsMostlyColor(D3DCOLOR c, uint8_t r, uint8_t g, uint8_t b) {
  const uint8_t cr = ChannelR(c);
  const uint8_t cg = ChannelG(c);
  const uint8_t cb = ChannelB(c);
  // Use wide thresholds to avoid flakiness from dither/rounding.
  const bool r_ok = (r == 0) ? (cr < 32) : (cr > 223);
  const bool g_ok = (g == 0) ? (cg < 32) : (cg > 223);
  const bool b_ok = (b == 0) ? (cb < 32) : (cb > 223);
  return r_ok && g_ok && b_ok;
}

static HRESULT CreateTexture1x1FromSysmem16(IDirect3DDevice9Ex* dev,
                                           D3DFORMAT fmt,
                                           WORD pixel,
                                           IDirect3DTexture9** out_tex) {
  if (!dev || !out_tex) {
    return E_INVALIDARG;
  }

  ComPtr<IDirect3DTexture9> sys_tex;
  HRESULT hr = dev->CreateTexture(1, 1, 1, 0, fmt, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return hr;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys_tex->LockRect(0, &lr, NULL, 0);
  if (FAILED(hr) || !lr.pBits) {
    return FAILED(hr) ? hr : E_FAIL;
  }
  *(WORD*)lr.pBits = pixel;
  sys_tex->UnlockRect(0);

  ComPtr<IDirect3DTexture9> gpu_tex;
  hr = dev->CreateTexture(1, 1, 1, 0, fmt, D3DPOOL_DEFAULT, gpu_tex.put(), NULL);
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

static int RunD3D9ExTexture16BitFormats(int argc, char** argv) {
  const char* kTestName = "d3d9ex_texture_16bit_formats";
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

  const int kWidth = 128;
  const int kHeight = 128;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExTexture16BitFormats",
                                              L"AeroGPU D3D9Ex 16-bit texture formats",
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

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  ComPtr<IDirect3DVertexShader9> vs;
  hr = dev->CreateVertexShader(kVsCopyPosTex, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }

  ComPtr<IDirect3DPixelShader9> ps;
  hr = dev->CreatePixelShader(kPsCopyTex, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  const D3DVERTEXELEMENT9 decl[] = {
      {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {0, 16, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
      D3DDECL_END(),
  };

  ComPtr<IDirect3DVertexDeclaration9> vdecl;
  hr = dev->CreateVertexDeclaration(decl, vdecl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }

  const VertexPosTex verts[4] = {
      {-1.0f, -1.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
      {-1.0f,  1.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      { 1.0f, -1.0f, 0.0f, 1.0f, 1.0f, 1.0f, 0.0f, 1.0f},
      { 1.0f,  1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(sizeof(verts),
                               D3DUSAGE_WRITEONLY | D3DUSAGE_DYNAMIC,
                               0,
                               D3DPOOL_DEFAULT,
                               vb.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }

  void* vb_ptr = NULL;
  hr = vb->Lock(0, sizeof(verts), &vb_ptr, D3DLOCK_DISCARD);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("VertexBuffer Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts, sizeof(verts));
  vb->Unlock();

  // Bind static pipeline state (quad + texture copy shaders).
  dev->SetVertexDeclaration(vdecl.get());
  dev->SetStreamSource(0, vb.get(), 0, sizeof(VertexPosTex));
  dev->SetVertexShader(vs.get());
  dev->SetPixelShader(ps.get());
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);

  dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_NONE);

  struct Case {
    const char* name;
    D3DFORMAT fmt;
    WORD pixel;
    bool alpha_blend;
    D3DCOLOR clear_color;
    uint8_t expect_r;
    uint8_t expect_g;
    uint8_t expect_b;
  };

  const Case cases[] = {
      // R5G6B5: draw solid red.
      {"R5G6B5", D3DFMT_R5G6B5, 0xF800u, false, D3DCOLOR_XRGB(0, 0, 0), 255, 0, 0},
      // A1R5G5B5: draw solid green with alpha=1.
      {"A1R5G5B5", D3DFMT_A1R5G5B5, 0x83E0u, false, D3DCOLOR_XRGB(0, 0, 0), 0, 255, 0},
      // X1R5G5B5: write alpha bit as 0, but sampling must treat alpha as 1.
      // Use alpha blending to validate alpha==1 by drawing over a green clear.
      {"X1R5G5B5(alpha=1)", D3DFMT_X1R5G5B5, 0x7C00u, true, D3DCOLOR_XRGB(0, 255, 0), 255, 0, 0},
  };

  for (size_t i = 0; i < aerogpu_test::ARRAYSIZE(cases); ++i) {
    const Case& c = cases[i];
    ComPtr<IDirect3DTexture9> tex;
    hr = CreateTexture1x1FromSysmem16(dev.get(), c.fmt, c.pixel, tex.put());
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("CreateTexture/UpdateTexture(%s)", c.name).c_str(), hr);
    }

    dev->SetTexture(0, tex.get());

    if (c.alpha_blend) {
      dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
      dev->SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
      dev->SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
      dev->SetRenderState(D3DRS_BLENDOP, D3DBLENDOP_ADD);
    } else {
      dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
    }

    hr = DrawFullscreenQuad(dev.get(), c.clear_color);
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("DrawFullscreenQuad(%s)", c.name).c_str(), hr);
    }

    D3DCOLOR pixel = 0;
    hr = ReadBackbufferCenterPixel(dev.get(), &pixel);
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("ReadBackbufferCenterPixel(%s)", c.name).c_str(), hr);
    }

    if (!IsMostlyColor(pixel, c.expect_r, c.expect_g, c.expect_b)) {
      return reporter.Fail("%s: unexpected pixel 0x%08lX (R=%u G=%u B=%u)",
                           c.name,
                           (unsigned long)pixel,
                           (unsigned)ChannelR(pixel),
                           (unsigned)ChannelG(pixel),
                           (unsigned)ChannelB(pixel));
    }
  }

  // Present once so interactive runs can observe the final state.
  dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExTexture16BitFormats(argc, argv);
  Sleep(30);
  return rc;
}
