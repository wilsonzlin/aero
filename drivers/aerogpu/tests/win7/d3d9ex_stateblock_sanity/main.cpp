#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <cstring>

using aerogpu_test::ComPtr;

struct GammaRampGuard {
  GammaRampGuard() = default;
  explicit GammaRampGuard(IDirect3DDevice9Ex* dev) : dev(dev) {
    if (this->dev) {
      ZeroMemory(&ramp, sizeof(ramp));
      this->dev->GetGammaRamp(0, &ramp);
      have_ramp = true;
    }
  }
  ~GammaRampGuard() {
    if (dev && have_ramp) {
      dev->SetGammaRamp(0, 0, &ramp);
    }
  }
  IDirect3DDevice9Ex* dev = NULL;
  D3DGAMMARAMP ramp;
  bool have_ramp = false;
};

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
    0x02000001u, 0x400F0000u, 0x10E40000u, // mov oPos, v0
    0x02000001u, 0x600F0000u, 0x10E40001u, // mov oT0, v1
    0x0000FFFFu, // end
};

static const DWORD kPsCopyTexMulC0[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, 0x000F0000u, 0x30E40000u, 0x20E40800u, // texld r0, t0, s0
    0x03000005u, 0x000F0000u, 0x00E40000u, 0x20E40000u, // mul r0, r0, c0
    0x02000001u, 0x000F0800u, 0x00E40000u, // mov oC0, r0
    0x0000FFFFu, // end
};

// Pixel shader (ps_2_0):
//   texld r0, t0, s0
//   mov oC0, r0
//   end
//
// Used by the test to ensure ApplyStateBlock restores shader bindings even when a
// stateblock was created via Begin/End around an Apply() call (nested recording).
static const DWORD kPsCopyTex[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, 0x000F0000u, 0x30E40000u, 0x20E40800u, // texld r0, t0, s0
    0x02000001u, 0x000F0800u, 0x00E40000u, // mov oC0, r0
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

static HRESULT CreateSolidTexture(IDirect3DDevice9Ex* dev, D3DCOLOR argb, IDirect3DTexture9** out_tex) {
  if (!dev || !out_tex) {
    return E_INVALIDARG;
  }

  // Stage through a systemmem texture so the copy path works even when the default-pool texture
  // is guest-backed.
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
  *(D3DCOLOR*)lr.pBits = argb;
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

static HRESULT ReadBackbufferPixel(IDirect3DDevice9Ex* dev, UINT* out_width, UINT* out_height, D3DCOLOR* out_pixel) {
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

  if (out_width) {
    *out_width = desc.Width;
  }
  if (out_height) {
    *out_height = desc.Height;
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
  if (FAILED(hr)) {
    return hr;
  }

  const UINT x = desc.Width / 2;
  const UINT y = desc.Height / 2;
  const uint8_t* row = (const uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
  *out_pixel = ((const D3DCOLOR*)row)[x];

  sys->UnlockRect();
  return S_OK;
}

static HRESULT ReadBackbufferPixelXY(IDirect3DDevice9Ex* dev, UINT x, UINT y, D3DCOLOR* out_pixel) {
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
  if (x >= desc.Width || y >= desc.Height) {
    return E_INVALIDARG;
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
  if (FAILED(hr)) {
    return hr;
  }

  const uint8_t* row = (const uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
  *out_pixel = ((const D3DCOLOR*)row)[x];

  sys->UnlockRect();
  return S_OK;
}

static HRESULT DrawQuad(IDirect3DDevice9Ex* dev) {
  if (!dev) {
    return E_INVALIDARG;
  }

  HRESULT hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, 0xFF000000u, 1.0f, 0);
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

  dev->Flush();
  return S_OK;
}

static void FillGammaRamp(D3DGAMMARAMP* ramp, WORD base) {
  if (!ramp) {
    return;
  }
  ZeroMemory(ramp, sizeof(*ramp));
  for (UINT i = 0; i < 256; ++i) {
    const DWORD raw = (DWORD)i * 257u;
    const DWORD biased = (raw + (DWORD)base > 0xFFFFu) ? 0xFFFFu : (raw + (DWORD)base);
    const WORD v = (WORD)biased;
    ramp->red[i] = v;
    ramp->green[i] = v;
    ramp->blue[i] = v;
  }
}

static bool GammaRampEqual(const D3DGAMMARAMP& a, const D3DGAMMARAMP& b) {
  return memcmp(&a, &b, sizeof(D3DGAMMARAMP)) == 0;
}

static void FillPaletteEntries(PALETTEENTRY* entries, BYTE seed) {
  if (!entries) {
    return;
  }
  for (UINT i = 0; i < 256; ++i) {
    entries[i].peRed = (BYTE)(seed + i);
    entries[i].peGreen = (BYTE)(seed + i * 3);
    entries[i].peBlue = (BYTE)(seed + i * 7);
    entries[i].peFlags = 0;
  }
}

static bool PaletteEntriesEqual(const PALETTEENTRY* a, const PALETTEENTRY* b) {
  if (!a || !b) {
    return false;
  }
  return memcmp(a, b, sizeof(PALETTEENTRY) * 256u) == 0;
}

static int RunD3D9ExStateBlockSanity(int argc, char** argv) {
  const char* kTestName = "d3d9ex_stateblock_sanity";
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

  const int kWidth = 64;
  const int kHeight = 64;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExStateBlockSanity",
                                              L"AeroGPU D3D9Ex StateBlock Sanity",
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

  // Avoid leaving the desktop gamma ramp in a modified state when running on
  // non-AeroGPU adapters (e.g. when --allow-non-aerogpu is used).
  GammaRampGuard gamma_guard(dev.get());

  // Create shaders.
  ComPtr<IDirect3DVertexShader9> vs;
  hr = dev->CreateVertexShader(kVsCopyPosTex, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }

  ComPtr<IDirect3DPixelShader9> ps;
  hr = dev->CreatePixelShader(kPsCopyTexMulC0, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  ComPtr<IDirect3DPixelShader9> ps_copy_tex;
  hr = dev->CreatePixelShader(kPsCopyTex, ps_copy_tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader(copy_tex)", hr);
  }

  // Create vertex declaration (pos + tex).
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

  // Create VB for a full-screen quad.
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

  // A second VB that draws only a small quad in the top-left quadrant; used to
  // validate D3DSBT_PIXELSTATE behavior (pixel-state blocks should not restore
  // vertex bindings).
  const VertexPosTex verts_tl[4] = {
      {-1.0f,  0.5f, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
      {-1.0f,  1.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      {-0.5f,  0.5f, 0.0f, 1.0f, 1.0f, 1.0f, 0.0f, 1.0f},
      {-0.5f,  1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  ComPtr<IDirect3DVertexBuffer9> vb_tl;
  hr = dev->CreateVertexBuffer(sizeof(verts_tl),
                               D3DUSAGE_WRITEONLY | D3DUSAGE_DYNAMIC,
                               0,
                               D3DPOOL_DEFAULT,
                               vb_tl.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer(vb_tl)", hr);
  }

  void* vb_ptr = NULL;
  hr = vb->Lock(0, sizeof(verts), &vb_ptr, D3DLOCK_DISCARD);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("VertexBuffer Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts, sizeof(verts));
  vb->Unlock();

  vb_ptr = NULL;
  hr = vb_tl->Lock(0, sizeof(verts_tl), &vb_ptr, D3DLOCK_DISCARD);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("VertexBuffer Lock(vb_tl)", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts_tl, sizeof(verts_tl));
  vb_tl->Unlock();

  // Create textures.
  ComPtr<IDirect3DTexture9> tex_a;
  hr = CreateSolidTexture(dev.get(), 0xFFFFFFFFu /*white*/, tex_a.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateSolidTexture A", hr);
  }

  ComPtr<IDirect3DTexture9> tex_b;
  hr = CreateSolidTexture(dev.get(), 0xFF0000FFu /*blue*/, tex_b.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateSolidTexture B", hr);
  }

  // StateBlock record.
  hr = dev->BeginStateBlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginStateBlock", hr);
  }

  // Record additional cached-only legacy state into the block so we can validate
  // ApplyStateBlock restores it.
  D3DGAMMARAMP gamma_a;
  FillGammaRamp(&gamma_a, /*base=*/1);
  dev->SetGammaRamp(0, 0, &gamma_a);

  D3DCLIPSTATUS9 clip_a;
  ZeroMemory(&clip_a, sizeof(clip_a));
  clip_a.ClipUnion = 0x00000011u;
  clip_a.ClipIntersection = 0x00000022u;
  hr = dev->SetClipStatus(&clip_a);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetClipStatus(record)", hr);
  }

  bool palette_ok = true;
  PALETTEENTRY pal_a[256];
  FillPaletteEntries(pal_a, /*seed=*/5);
  PALETTEENTRY pal_b[256];
  FillPaletteEntries(pal_b, /*seed=*/77);
  hr = dev->SetPaletteEntries(0, pal_a);
  if (FAILED(hr)) {
    // Some runtimes/drivers may reject palette APIs when palettized textures are
    // not supported. Treat this as a supported skip.
    aerogpu_test::PrintfStdout("INFO: %s: skipping palette stateblock checks (SetPaletteEntries hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
    palette_ok = false;
  } else {
    hr = dev->SetCurrentTexturePalette(0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: skipping palette stateblock checks (SetCurrentTexturePalette hr=0x%08lX)",
          kTestName,
          (unsigned long)hr);
      palette_ok = false;
    }
  }

  hr = dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(CULLMODE)", hr);
  }

  hr = dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(ZENABLE)", hr);
  }

  hr = dev->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(ZWRITEENABLE)", hr);
  }

  D3DVIEWPORT9 vp_full;
  vp_full.X = 0;
  vp_full.Y = 0;
  vp_full.Width = kWidth;
  vp_full.Height = kHeight;
  vp_full.MinZ = 0.0f;
  vp_full.MaxZ = 1.0f;
  hr = dev->SetViewport(&vp_full);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport(full)", hr);
  }

  hr = dev->SetVertexDeclaration(vdecl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexDeclaration", hr);
  }

  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }

  hr = dev->SetVertexShader(vs.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader", hr);
  }

  hr = dev->SetPixelShader(ps.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader", hr);
  }

  hr = dev->SetTexture(0, tex_a.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture A", hr);
  }

  hr = dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MINFILTER)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MAGFILTER)", hr);
  }

  float c0_green[4] = {0.0f, 1.0f, 0.0f, 1.0f};
  hr = dev->SetPixelShaderConstantF(0, c0_green, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(green)", hr);
  }

  ComPtr<IDirect3DStateBlock9> sb;
  hr = dev->EndStateBlock(sb.put());
  if (FAILED(hr) || !sb) {
    return reporter.FailHresult("EndStateBlock", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate state away from recorded values.
  float c0_white[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  D3DGAMMARAMP gamma_b;
  FillGammaRamp(&gamma_b, /*base=*/7);
  dev->SetGammaRamp(0, 0, &gamma_b);

  D3DCLIPSTATUS9 clip_b;
  ZeroMemory(&clip_b, sizeof(clip_b));
  clip_b.ClipUnion = 0x000000AAu;
  clip_b.ClipIntersection = 0x000000BBu;
  hr = dev->SetClipStatus(&clip_b);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetClipStatus(mutate)", hr);
  }

  if (palette_ok) {
    hr = dev->SetPaletteEntries(1, pal_b);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPaletteEntries(mutate)", hr);
    }
    hr = dev->SetCurrentTexturePalette(1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetCurrentTexturePalette(mutate)", hr);
    }
  }

  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B", hr);
  }
  // Disable color writes and unbind the VB to ensure ApplyStateBlock restores
  // these bindings/states.
  hr = dev->SetRenderState(D3DRS_COLORWRITEENABLE, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(COLORWRITEENABLE=0 mutate)", hr);
  }
  hr = dev->SetStreamSource(0, NULL, 0, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(NULL mutate)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_white, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(white)", hr);
  }
  D3DVIEWPORT9 vp_small;
  vp_small.X = 0;
  vp_small.Y = 0;
  vp_small.Width = 1;
  vp_small.Height = 1;
  vp_small.MinZ = 0.0f;
  vp_small.MaxZ = 1.0f;
  hr = dev->SetViewport(&vp_small);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport(small mutate)", hr);
  }

  // Apply should restore tex_a and c0_green.
  hr = sb->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply", hr);
  }

  // Validate legacy state was restored.
  D3DGAMMARAMP got_gamma;
  ZeroMemory(&got_gamma, sizeof(got_gamma));
  dev->GetGammaRamp(0, &got_gamma);
  if (!GammaRampEqual(got_gamma, gamma_a)) {
    return reporter.Fail("GetGammaRamp mismatch after Apply");
  }

  D3DCLIPSTATUS9 got_clip;
  ZeroMemory(&got_clip, sizeof(got_clip));
  hr = dev->GetClipStatus(&got_clip);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetClipStatus(after Apply)", hr);
  }
  if (got_clip.ClipUnion != clip_a.ClipUnion || got_clip.ClipIntersection != clip_a.ClipIntersection) {
    return reporter.Fail("GetClipStatus mismatch after Apply: got {union=0x%08lX inter=0x%08lX} expected {union=0x%08lX inter=0x%08lX}",
                         (unsigned long)got_clip.ClipUnion,
                         (unsigned long)got_clip.ClipIntersection,
                         (unsigned long)clip_a.ClipUnion,
                         (unsigned long)clip_a.ClipIntersection);
  }

  if (palette_ok) {
    PALETTEENTRY got_pal[256];
    ZeroMemory(got_pal, sizeof(got_pal));
    hr = dev->GetPaletteEntries(0, got_pal);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPaletteEntries(after Apply)", hr);
    }
    if (!PaletteEntriesEqual(got_pal, pal_a)) {
      return reporter.Fail("GetPaletteEntries mismatch after Apply");
    }
    UINT got_cur = 0xFFFFFFFFu;
    hr = dev->GetCurrentTexturePalette(&got_cur);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetCurrentTexturePalette(after Apply)", hr);
    }
    if (got_cur != 0) {
      return reporter.Fail("GetCurrentTexturePalette mismatch after Apply: got=%u expected=0",
                           (unsigned)got_cur);
    }
  }

  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after Apply)", hr);
  }

  D3DCOLOR px = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after Apply)", hr);
  }
  const D3DCOLOR expected_green = 0xFF00FF00u;
  if ((px & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch after Apply: got=0x%08X expected=0x%08X", (unsigned)px, (unsigned)expected_green);
  }

  // Exercise ApplyStateBlock while Begin/EndStateBlock recording is active.
  //
  // Some apps use this as a way to "clone" an existing state block.
  // In this scenario, Apply may be a no-op (state already matches), but the
  // invoked Apply must still record the applied bindings/states into the
  // in-progress recording.
  ComPtr<IDirect3DStateBlock9> sb_from_apply;
  hr = dev->BeginStateBlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginStateBlock (nested)", hr);
  }
  hr = sb->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (nested)", hr);
  }
  hr = dev->EndStateBlock(sb_from_apply.put());
  if (FAILED(hr) || !sb_from_apply) {
    return reporter.FailHresult("EndStateBlock (nested)", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate shader/texture, then Apply the newly recorded block. If the nested
  // recording missed shader bindings, we'd keep `ps_copy_tex` and render white
  // instead of green.
  hr = dev->SetPixelShader(ps_copy_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader(copy_tex mutate)", hr);
  }
  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B (nested mutate)", hr);
  }

  hr = sb_from_apply->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (from nested)", hr);
  }
  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after nested Apply)", hr);
  }
  px = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after nested Apply)", hr);
  }
  if ((px & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch after nested Apply: got=0x%08X expected=0x%08X", (unsigned)px, (unsigned)expected_green);
  }

  // ValidateDevice should not hard-fail for the supported shader pipeline.
  DWORD validate_passes = 0;
  hr = dev->ValidateDevice(&validate_passes);
  if (FAILED(hr)) {
    return reporter.FailHresult("ValidateDevice", hr);
  }
  if (validate_passes == 0) {
    return reporter.Fail("ValidateDevice returned 0 passes");
  }

  // Exercise CreateStateBlock (DDI-backed) as well as Begin/End.
  ComPtr<IDirect3DStateBlock9> sb_created;
  hr = dev->CreateStateBlock(D3DSBT_ALL, sb_created.put());
  if (FAILED(hr) || !sb_created) {
    return reporter.FailHresult("CreateStateBlock(D3DSBT_ALL)", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate state away (shader/texture/constant/viewport/VB), then Apply and verify we
  // get back the captured (green) result.
  hr = dev->SetPixelShader(ps_copy_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader(copy_tex mutate 2)", hr);
  }
  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B (mutate 2)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_white, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(white mutate 2)", hr);
  }
  hr = dev->SetViewport(&vp_small);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport(small mutate 3)", hr);
  }
  hr = dev->SetRenderState(D3DRS_COLORWRITEENABLE, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(COLORWRITEENABLE=0 mutate 2)", hr);
  }
  hr = dev->SetStreamSource(0, NULL, 0, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(NULL mutate 2)", hr);
  }
  // Mutate cached-only legacy state too, so we can validate Apply restores it.
  dev->SetGammaRamp(0, 0, &gamma_b);
  hr = dev->SetClipStatus(&clip_b);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetClipStatus(mutate 2)", hr);
  }
  if (palette_ok) {
    hr = dev->SetPaletteEntries(0, pal_b);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPaletteEntries(mutate 2)", hr);
    }
    hr = dev->SetCurrentTexturePalette(1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetCurrentTexturePalette(mutate 2)", hr);
    }
  }

  hr = sb_created->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (created)", hr);
  }
  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after CreateStateBlock Apply)", hr);
  }
  px = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after CreateStateBlock Apply)", hr);
  }
  if ((px & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch after CreateStateBlock Apply: got=0x%08X expected=0x%08X", (unsigned)px, (unsigned)expected_green);
  }

  // Validate cached-only legacy state was restored by Apply.
  ZeroMemory(&got_gamma, sizeof(got_gamma));
  dev->GetGammaRamp(0, &got_gamma);
  if (!GammaRampEqual(got_gamma, gamma_a)) {
    return reporter.Fail("GetGammaRamp mismatch after CreateStateBlock Apply");
  }
  ZeroMemory(&got_clip, sizeof(got_clip));
  hr = dev->GetClipStatus(&got_clip);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetClipStatus(after CreateStateBlock Apply)", hr);
  }
  if (got_clip.ClipUnion != clip_a.ClipUnion || got_clip.ClipIntersection != clip_a.ClipIntersection) {
    return reporter.Fail("GetClipStatus mismatch after CreateStateBlock Apply");
  }
  if (palette_ok) {
    PALETTEENTRY got_pal[256];
    ZeroMemory(got_pal, sizeof(got_pal));
    hr = dev->GetPaletteEntries(0, got_pal);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPaletteEntries(after CreateStateBlock Apply)", hr);
    }
    if (!PaletteEntriesEqual(got_pal, pal_a)) {
      return reporter.Fail("GetPaletteEntries mismatch after CreateStateBlock Apply");
    }
    UINT got_cur = 0xFFFFFFFFu;
    hr = dev->GetCurrentTexturePalette(&got_cur);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetCurrentTexturePalette(after CreateStateBlock Apply)", hr);
    }
    if (got_cur != 0) {
      return reporter.Fail("GetCurrentTexturePalette mismatch after CreateStateBlock Apply: got=%u expected=0",
                           (unsigned)got_cur);
    }
  }

  // Exercise D3DSBT_PIXELSTATE: it should restore pixel state (texture/PS
  // constants) but should not touch the currently-bound vertex buffer.
  ComPtr<IDirect3DStateBlock9> sb_pixel;
  hr = dev->CreateStateBlock(D3DSBT_PIXELSTATE, sb_pixel.put());
  if (FAILED(hr) || !sb_pixel) {
    return reporter.FailHresult("CreateStateBlock(D3DSBT_PIXELSTATE)", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate vertex binding to the small top-left VB, and mutate pixel state to
  // blue (blue texture * white constant).
  hr = dev->SetStreamSource(0, vb_tl.get(), 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(vb_tl pixelstate mutate)", hr);
  }
  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B (pixelstate mutate)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_white, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(white pixelstate mutate)", hr);
  }
  // Mutate pixel-state gamma ramp/palettes as well.
  dev->SetGammaRamp(0, 0, &gamma_b);
  if (palette_ok) {
    hr = dev->SetPaletteEntries(0, pal_b);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPaletteEntries(pixelstate mutate)", hr);
    }
    hr = dev->SetCurrentTexturePalette(1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetCurrentTexturePalette(pixelstate mutate)", hr);
    }
  }

  hr = sb_pixel->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (pixelstate)", hr);
  }
  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after pixelstate Apply)", hr);
  }

  // Center pixel should remain black (quad doesn't cover it), and top-left pixel
  // should be green (pixel state restored).
  D3DCOLOR px_center = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px_center);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after pixelstate Apply)", hr);
  }
  const D3DCOLOR expected_black = 0xFF000000u;
  if ((px_center & 0x00FFFFFFu) != (expected_black & 0x00FFFFFFu)) {
    return reporter.Fail("center pixel mismatch after pixelstate Apply: got=0x%08X expected=0x%08X",
                         (unsigned)px_center,
                         (unsigned)expected_black);
  }

  D3DCOLOR px_tl = 0;
  hr = ReadBackbufferPixelXY(dev.get(), 5, 5, &px_tl);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixelXY(5,5) (after pixelstate Apply)", hr);
  }
  if ((px_tl & 0x00FFFFFFu) != (expected_green & 0x00FFFFFFu)) {
    return reporter.Fail("top-left pixel mismatch after pixelstate Apply: got=0x%08X expected=0x%08X",
                         (unsigned)px_tl,
                         (unsigned)expected_green);
  }

  // PIXELSTATE blocks should restore gamma ramp and palette state.
  ZeroMemory(&got_gamma, sizeof(got_gamma));
  dev->GetGammaRamp(0, &got_gamma);
  if (!GammaRampEqual(got_gamma, gamma_a)) {
    return reporter.Fail("GetGammaRamp mismatch after PIXELSTATE Apply");
  }
  if (palette_ok) {
    PALETTEENTRY got_pal[256];
    ZeroMemory(got_pal, sizeof(got_pal));
    hr = dev->GetPaletteEntries(0, got_pal);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPaletteEntries(after PIXELSTATE Apply)", hr);
    }
    if (!PaletteEntriesEqual(got_pal, pal_a)) {
      return reporter.Fail("GetPaletteEntries mismatch after PIXELSTATE Apply");
    }
    UINT got_cur = 0xFFFFFFFFu;
    hr = dev->GetCurrentTexturePalette(&got_cur);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetCurrentTexturePalette(after PIXELSTATE Apply)", hr);
    }
    if (got_cur != 0) {
      return reporter.Fail("GetCurrentTexturePalette mismatch after PIXELSTATE Apply: got=%u expected=0",
                           (unsigned)got_cur);
    }
  }

  // Restore the full-screen VB for subsequent tests (vertex-state and later
  // Capture/Apply phases expect to validate center pixels).
  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(vb restore after pixelstate)", hr);
  }

  // Exercise D3DSBT_VERTEXSTATE: it should restore VB bindings (so draw works),
  // but should NOT override pixel-state (texture/PS constants).
  ComPtr<IDirect3DStateBlock9> sb_vertex;
  hr = dev->CreateStateBlock(D3DSBT_VERTEXSTATE, sb_vertex.put());
  if (FAILED(hr) || !sb_vertex) {
    return reporter.FailHresult("CreateStateBlock(D3DSBT_VERTEXSTATE)", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate vertex state: unbind VB (draw should be broken unless vertex state is restored).
  hr = dev->SetStreamSource(0, NULL, 0, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(NULL mutate for vertexstate)", hr);
  }
  // Mutate cached vertex-state clip status too.
  hr = dev->SetClipStatus(&clip_b);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetClipStatus(vertexstate mutate)", hr);
  }

  // Mutate pixel state: make output blue (blue texture * white constant).
  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B (vertexstate pixel mutate)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_white, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(white vertexstate pixel mutate)", hr);
  }

  hr = sb_vertex->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (vertexstate)", hr);
  }
  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after vertexstate Apply)", hr);
  }
  px = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after vertexstate Apply)", hr);
  }
  const D3DCOLOR expected_blue = 0xFF0000FFu;
  if ((px & 0x00FFFFFFu) != (expected_blue & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch after vertexstate Apply: got=0x%08X expected=0x%08X", (unsigned)px, (unsigned)expected_blue);
  }
  ZeroMemory(&got_clip, sizeof(got_clip));
  hr = dev->GetClipStatus(&got_clip);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetClipStatus(after VERTEXSTATE Apply)", hr);
  }
  if (got_clip.ClipUnion != clip_a.ClipUnion || got_clip.ClipIntersection != clip_a.ClipIntersection) {
    return reporter.Fail("GetClipStatus mismatch after VERTEXSTATE Apply");
  }

  // Capture should update the existing block to the current device state.
  float c0_red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
  hr = dev->SetTexture(0, tex_a.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture A (pre-Capture)", hr);
  }
  // Ensure we capture a sane state for render-state + VB bindings.
  hr = dev->SetRenderState(D3DRS_COLORWRITEENABLE, 0xFu);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(COLORWRITEENABLE=0xF pre-Capture)", hr);
  }
  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(vb pre-Capture)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_red, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(red)", hr);
  }

  hr = sb->Capture();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Capture", hr);
  }

  // Mutate away again, then apply; we should get red.
  hr = dev->SetTexture(0, tex_b.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture B (post-Capture mutate)", hr);
  }
  hr = dev->SetRenderState(D3DRS_COLORWRITEENABLE, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(COLORWRITEENABLE=0 post-Capture mutate)", hr);
  }
  hr = dev->SetStreamSource(0, NULL, 0, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(NULL post-Capture mutate)", hr);
  }
  hr = dev->SetPixelShaderConstantF(0, c0_green, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF(green mutate)", hr);
  }
  hr = dev->SetViewport(&vp_small);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport(small mutate 2)", hr);
  }

  hr = sb->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply (after Capture)", hr);
  }

  hr = DrawQuad(dev.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad (after Capture+Apply)", hr);
  }

  px = 0;
  hr = ReadBackbufferPixel(dev.get(), NULL, NULL, &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferPixel (after Capture+Apply)", hr);
  }
  const D3DCOLOR expected_red = 0xFFFF0000u;
  if ((px & 0x00FFFFFFu) != (expected_red & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch after Capture+Apply: got=0x%08X expected=0x%08X", (unsigned)px, (unsigned)expected_red);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExStateBlockSanity(argc, argv);
}
