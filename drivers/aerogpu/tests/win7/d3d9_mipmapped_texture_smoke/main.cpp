#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <algorithm>
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

// Vertex shader (vs_2_0):
//   mov oPos, v0
//   mov oT0, v1
//   end
static const DWORD kVsCopyPosTex[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, 0x400F0000u, 0x10E40000u, // mov oPos, v0
    0x03000001u, 0x600F0000u, 0x10E40001u, // mov oT0, v1
    0x0000FFFFu, // end
};

// Pixel shader (ps_2_0):
//   texld r0, t0, s0
//   mov oC0, r0
//   end
static const DWORD kPsCopyTex[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, 0x000F0000u, 0x30E40000u, 0x20E40800u, // texld r0, t0, s0
    0x03000001u, 0x000F0800u, 0x00E40000u, // mov oC0, r0
    0x0000FFFFu, // end
};

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             pp,
                             NULL,
                             out_dev);
  }
  return hr;
}

static void DumpBackbufferBmpIfEnabled(const char* test_name,
                                       aerogpu_test::TestReporter* reporter,
                                       bool dump,
                                       const wchar_t* bmp_name,
                                       const void* data,
                                       int row_pitch,
                                       int width,
                                       int height) {
  if (!dump || !bmp_name || !data || width <= 0 || height <= 0 || row_pitch <= 0) {
    return;
  }
  std::string err;
  const std::wstring bmp_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), bmp_name);
  if (aerogpu_test::WriteBmp32BGRA(bmp_path, width, height, data, row_pitch, &err)) {
    if (reporter) {
      reporter->AddArtifactPathW(bmp_path);
    }
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name ? test_name : "<null>", err.c_str());
  }
}

static HRESULT ReadBackbufferCenterPixel(IDirect3DDevice9Ex* dev,
                                          bool dump,
                                          aerogpu_test::TestReporter* reporter,
                                          const wchar_t* dump_bmp_name,
                                          D3DCOLOR* out_pixel) {
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
  if (FAILED(hr)) {
    return hr;
  }

  // Sample a pixel slightly off-center so we don't accidentally hit the shared
  // diagonal edge of our fullscreen triangle strip (which can be sensitive to
  // rasterization edge rules if culling/state changes).
  int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  if (desc.Width > 1) {
    cx = std::min(cx + 4, (int)desc.Width - 1);
  }
  const uint32_t px = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  *out_pixel = (D3DCOLOR)px;

  DumpBackbufferBmpIfEnabled("d3d9_mipmapped_texture_smoke",
                             reporter,
                             dump,
                             dump_bmp_name,
                             lr.pBits,
                             (int)lr.Pitch,
                             (int)desc.Width,
                             (int)desc.Height);

  sys->UnlockRect();
  return S_OK;
}

static HRESULT FillTextureLevelSolid(IDirect3DTexture9* tex, UINT level, UINT width, UINT height, D3DCOLOR argb) {
  if (!tex || width == 0 || height == 0) {
    return E_INVALIDARG;
  }
  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  HRESULT hr = tex->LockRect(level, &lr, NULL, 0);
  if (FAILED(hr)) {
    return hr;
  }

  for (UINT y = 0; y < height; ++y) {
    uint8_t* row = (uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
    for (UINT x = 0; x < width; ++x) {
      ((D3DCOLOR*)row)[x] = argb;
    }
  }

  return tex->UnlockRect(level);
}

static HRESULT DrawQuad(IDirect3DDevice9Ex* dev, IDirect3DVertexBuffer9* vb, D3DCOLOR clear_color) {
  if (!dev || !vb) {
    return E_INVALIDARG;
  }

  HRESULT hr = dev->SetStreamSource(0, vb, 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear_color, 1.0f, 0);
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

static int RunD3D9MipmappedTextureSmoke(int argc, char** argv) {
  const char* kTestName = "d3d9_mipmapped_texture_smoke";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9MipTextureSmoke",
                                              L"AeroGPU D3D9 mipmapped texture smoke",
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
  if (FAILED(hr) || !dev) {
    return reporter.FailHresult("CreateDeviceEx", FAILED(hr) ? hr : E_FAIL);
  }

  // Create shaders.
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

  // Fullscreen quad in clip space (w=1), with texcoords (u,v,0,1).
  const VertexPosTex verts_full[4] = {
      {-1.0f, -1.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
      {-1.0f,  1.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      { 1.0f, -1.0f, 0.0f, 1.0f, 1.0f, 1.0f, 0.0f, 1.0f},
      { 1.0f,  1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  // Small centered quad to force minification (LOD > 1, clamps to mip 1).
  const float half_px = (16.0f / (float)kWidth); // half size in normalized [0..1] window coords
  const float half_ndc = half_px * 2.0f; // NDC is [-1,1]
  const VertexPosTex verts_small[4] = {
      {-half_ndc, -half_ndc, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
      {-half_ndc,  half_ndc, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      { half_ndc, -half_ndc, 0.0f, 1.0f, 1.0f, 1.0f, 0.0f, 1.0f},
      { half_ndc,  half_ndc, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  ComPtr<IDirect3DVertexBuffer9> vb_full;
  hr = dev->CreateVertexBuffer(sizeof(verts_full), D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, vb_full.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer(vb_full)", hr);
  }
  ComPtr<IDirect3DVertexBuffer9> vb_small;
  hr = dev->CreateVertexBuffer(sizeof(verts_small), D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, vb_small.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer(vb_small)", hr);
  }

  void* vb_ptr = NULL;
  hr = vb_full->Lock(0, sizeof(verts_full), &vb_ptr, 0);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("vb_full Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts_full, sizeof(verts_full));
  vb_full->Unlock();

  vb_ptr = NULL;
  hr = vb_small->Lock(0, sizeof(verts_small), &vb_ptr, 0);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("vb_small Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts_small, sizeof(verts_small));
  vb_small->Unlock();

  // Create a mipmapped texture. This previously failed on Win7/WDDM with E_NOTIMPL.
  const UINT kTexW = 128;
  const UINT kTexH = 128;
  const UINT kLevels = 2;
  const DWORD kUsage = D3DUSAGE_DYNAMIC;
  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(kTexW,
                          kTexH,
                          kLevels,
                          kUsage,
                          D3DFMT_A8R8G8B8,
                          D3DPOOL_DEFAULT,
                          tex.put(),
                          NULL);
  if (FAILED(hr) || !tex) {
    return reporter.FailHresult("CreateTexture(Levels=2, DEFAULT)", FAILED(hr) ? hr : E_FAIL);
  }

  // Validate that both mip levels are lockable and have the expected pitches.
  D3DLOCKED_RECT lr0;
  ZeroMemory(&lr0, sizeof(lr0));
  hr = tex->LockRect(0, &lr0, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("LockRect(level0)", hr);
  }
  const UINT expected_pitch0 = kTexW * 4;
  if ((UINT)lr0.Pitch != expected_pitch0) {
    tex->UnlockRect(0);
    return reporter.Fail("unexpected pitch for level0: got %d expected %u", (int)lr0.Pitch, (unsigned)expected_pitch0);
  }
  tex->UnlockRect(0);

  D3DLOCKED_RECT lr1;
  ZeroMemory(&lr1, sizeof(lr1));
  // Lock a small non-zero sub-rect so the underlying DDI lock offset is inside
  // the mip level, not exactly at its base. The Pitch must still match the full
  // mip row pitch.
  RECT mip1_rect;
  mip1_rect.left = 1;
  mip1_rect.top = 1;
  mip1_rect.right = 2;
  mip1_rect.bottom = 2;
  hr = tex->LockRect(1, &lr1, &mip1_rect, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("LockRect(level1)", hr);
  }
  const UINT expected_pitch1 = (kTexW / 2) * 4;
  if ((UINT)lr1.Pitch != expected_pitch1) {
    tex->UnlockRect(1);
    return reporter.Fail("unexpected pitch for level1: got %d expected %u", (int)lr1.Pitch, (unsigned)expected_pitch1);
  }
  tex->UnlockRect(1);

  const D3DCOLOR kMip0Color = D3DCOLOR_XRGB(255, 0, 0);
  const D3DCOLOR kMip1Color = D3DCOLOR_XRGB(0, 255, 0);
  hr = FillTextureLevelSolid(tex.get(), 0, kTexW, kTexH, kMip0Color);
  if (FAILED(hr)) {
    return reporter.FailHresult("FillTextureLevelSolid(level0)", hr);
  }
  hr = FillTextureLevelSolid(tex.get(), 1, kTexW / 2, kTexH / 2, kMip1Color);
  if (FAILED(hr)) {
    return reporter.FailHresult("FillTextureLevelSolid(level1)", hr);
  }

  // Bind pipeline state.
  hr = dev->SetVertexDeclaration(vdecl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexDeclaration", hr);
  }
  hr = dev->SetVertexShader(vs.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader", hr);
  }
  hr = dev->SetPixelShader(ps.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader", hr);
  }
  hr = dev->SetTexture(0, tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture", hr);
  }

  hr = dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MINFILTER)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MAGFILTER)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MIPFILTER)", hr);
  }

  // Make quad rendering deterministic: triangle strips alternate winding, so
  // default culling can drop half the quad.
  hr = dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_CULLMODE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_ZENABLE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_ZWRITEENABLE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE)", hr);
  }

  // Draw fullscreen quad -> should sample mip0 (magnification).
  hr = DrawQuad(dev.get(), vb_full.get(), D3DCOLOR_XRGB(0, 0, 0));
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad(fullscreen)", hr);
  }
  D3DCOLOR px = 0;
  hr = ReadBackbufferCenterPixel(dev.get(), dump, &reporter, L"d3d9_mipmapped_texture_smoke_full.bmp", &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferCenterPixel(full)", hr);
  }
  if ((px & 0x00FFFFFFu) != (kMip0Color & 0x00FFFFFFu)) {
    return reporter.Fail("fullscreen sample mismatch: got 0x%08lX expected 0x%08lX",
                         (unsigned long)px,
                         (unsigned long)kMip0Color);
  }

  // Draw small quad -> should sample mip1 (minification; clamps to last mip).
  hr = DrawQuad(dev.get(), vb_small.get(), D3DCOLOR_XRGB(0, 0, 0));
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawQuad(small)", hr);
  }
  px = 0;
  hr = ReadBackbufferCenterPixel(dev.get(), dump, &reporter, L"d3d9_mipmapped_texture_smoke_small.bmp", &px);
  if (FAILED(hr)) {
    return reporter.FailHresult("ReadBackbufferCenterPixel(small)", hr);
  }
  if ((px & 0x00FFFFFFu) != (kMip1Color & 0x00FFFFFFu)) {
    return reporter.Fail("mip1 sample mismatch: got 0x%08lX expected 0x%08lX",
                         (unsigned long)px,
                         (unsigned long)kMip1Color);
  }

  // ---------------------------------------------------------------------------
  // Systemmem staging + UpdateTexture path
  // ---------------------------------------------------------------------------
  //
  // This covers the common D3D9 texture upload workflow:
  //   - fill a SYSTEMMEM mip chain via LockRect(level)
  //   - UpdateTexture into a DEFAULT-pool mip chain
  //   - render + validate a sampled pixel
  //
  // Additionally, LockRect calls use a non-zero sub-rect (when possible) so the
  // underlying DDI Lock offset falls *inside* the mip level; the expected Pitch
  // is still the full mip row pitch.
  const UINT kSysTexW = 8;
  const UINT kSysTexH = 8;
  const UINT kSysLevels = 4;
  const D3DFORMAT kSysFmt = D3DFMT_X8R8G8B8;

  // Avoid using a non-literal array bound so the test stays buildable with older
  // MSVC toolchains (VS2010 build scripts).
  const D3DCOLOR kSysMipColors[4] = {
      D3DCOLOR_XRGB(0xCC, 0x00, 0xCC), // mip0: purple
      D3DCOLOR_XRGB(0x00, 0xCC, 0xCC), // mip1: cyan
      D3DCOLOR_XRGB(0xCC, 0xCC, 0x00), // mip2: yellow
      D3DCOLOR_XRGB(0xCC, 0xCC, 0xCC), // mip3: grey
  };

  ComPtr<IDirect3DTexture9> sys_tex;
  hr = dev->CreateTexture(kSysTexW, kSysTexH, kSysLevels, 0, kSysFmt, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr) || !sys_tex) {
    return reporter.FailHresult("CreateTexture(SYSTEMMEM mipchain)", FAILED(hr) ? hr : E_FAIL);
  }

  for (UINT level = 0; level < kSysLevels; ++level) {
    UINT w = kSysTexW >> level;
    UINT h = kSysTexH >> level;
    if (w == 0) w = 1;
    if (h == 0) h = 1;

    const UINT expected_pitch = w * 4u;

    // Lock a 1x1 sub-rect at (1,1) when possible to ensure the lock offset is
    // inside the mip level, not at its base.
    RECT r;
    r.left = (w > 1) ? 1 : 0;
    r.top = (h > 1) ? 1 : 0;
    r.right = r.left + 1;
    r.bottom = r.top + 1;

    D3DLOCKED_RECT lr;
    ZeroMemory(&lr, sizeof(lr));
    hr = sys_tex->LockRect(level, &lr, &r, 0);
    if (FAILED(hr) || !lr.pBits) {
      return reporter.FailHresult(aerogpu_test::FormatString("LockRect(SYSTEMMEM level=%u)", (unsigned)level).c_str(),
                                  FAILED(hr) ? hr : E_FAIL);
    }
    if ((UINT)lr.Pitch != expected_pitch) {
      sys_tex->UnlockRect(level);
      return reporter.Fail("SYSTEMMEM LockRect Pitch mismatch level=%u: got %d expected %u",
                           (unsigned)level,
                           (int)lr.Pitch,
                           (unsigned)expected_pitch);
    }
    sys_tex->UnlockRect(level);

    hr = FillTextureLevelSolid(sys_tex.get(), level, w, h, kSysMipColors[level]);
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("FillTextureLevelSolid(SYSTEMMEM level=%u)", (unsigned)level).c_str(), hr);
    }
  }

  ComPtr<IDirect3DTexture9> sys_upload_tex;
  hr = dev->CreateTexture(kSysTexW, kSysTexH, kSysLevels, 0, kSysFmt, D3DPOOL_DEFAULT, sys_upload_tex.put(), NULL);
  if (FAILED(hr) || !sys_upload_tex) {
    return reporter.FailHresult("CreateTexture(DEFAULT mipchain for UpdateTexture)", FAILED(hr) ? hr : E_FAIL);
  }
  hr = dev->UpdateTexture(sys_tex.get(), sys_upload_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("UpdateTexture(SYSTEMMEM->DEFAULT mipchain)", hr);
  }

  hr = dev->SetTexture(0, sys_upload_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture(sys_upload_tex)", hr);
  }

  // Make this check deterministic: force the sampler to treat mip N as the base
  // level via MAXMIPLEVEL, then render with magnification so we always sample
  // exactly that base level.
  hr = dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MINFILTER)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MAGFILTER)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MIPFILTER)", hr);
  }

  // Avoid using a non-literal array bound so the test stays buildable with older
  // MSVC toolchains (VS2010 build scripts).
  const wchar_t* const kSysUpdateBmps[4] = {
      L"d3d9_mipmapped_texture_smoke_update_mip0.bmp",
      L"d3d9_mipmapped_texture_smoke_update_mip1.bmp",
      L"d3d9_mipmapped_texture_smoke_update_mip2.bmp",
      L"d3d9_mipmapped_texture_smoke_update_mip3.bmp",
  };

  for (UINT sample_level = 0; sample_level < kSysLevels; ++sample_level) {
    hr = dev->SetSamplerState(0, D3DSAMP_MAXMIPLEVEL, sample_level);
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("SetSamplerState(MAXMIPLEVEL=%u)",
                                                            (unsigned)sample_level).c_str(),
                                  hr);
    }

    hr = DrawQuad(dev.get(), vb_full.get(), D3DCOLOR_XRGB(0, 0, 0));
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("DrawQuad(UpdateTexture mip=%u)",
                                                            (unsigned)sample_level).c_str(),
                                  hr);
    }
    px = 0;
    hr = ReadBackbufferCenterPixel(dev.get(), dump, &reporter, kSysUpdateBmps[sample_level], &px);
    if (FAILED(hr)) {
      return reporter.FailHresult(aerogpu_test::FormatString("ReadBackbufferCenterPixel(UpdateTexture mip=%u)",
                                                            (unsigned)sample_level).c_str(),
                                  hr);
    }
    if ((px & 0x00FFFFFFu) != (kSysMipColors[sample_level] & 0x00FFFFFFu)) {
      return reporter.Fail("UpdateTexture sample mismatch mip=%u: got 0x%08lX expected 0x%08lX",
                           (unsigned)sample_level,
                           (unsigned long)px,
                           (unsigned long)kSysMipColors[sample_level]);
    }
  }

  // Restore default base mip level for any subsequent draws (should be a no-op,
  // but keeps the state machine tidy).
  hr = dev->SetSamplerState(0, D3DSAMP_MAXMIPLEVEL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MAXMIPLEVEL=0)", hr);
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9MipmappedTextureSmoke(argc, argv);
  Sleep(30);
  return rc;
}
