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
      // GetGammaRamp is a void method; assume it succeeds.
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

static bool NearlyEqual(float a, float b, float eps) {
  float d = a - b;
  if (d < 0.0f) {
    d = -d;
  }
  return d <= eps;
}

static int RunD3D9ExGettersSanity(int argc, char** argv) {
  const char* kTestName = "d3d9ex_getters_sanity";
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

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExGettersSanity",
                                              L"AeroGPU D3D9Ex Getters Sanity",
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  // Avoid leaving the desktop gamma ramp in a modified state when running on
  // non-AeroGPU adapters (e.g. when --allow-non-aerogpu is used).
  GammaRampGuard gamma_guard(dev.get());

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

  // --- Viewport ---
  D3DVIEWPORT9 vp;
  ZeroMemory(&vp, sizeof(vp));
  vp.X = 1;
  vp.Y = 2;
  vp.Width = 123;
  vp.Height = 77;
  vp.MinZ = 0.25f;
  vp.MaxZ = 0.75f;
  hr = dev->SetViewport(&vp);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport", hr);
  }

  D3DVIEWPORT9 got_vp;
  ZeroMemory(&got_vp, sizeof(got_vp));
  hr = dev->GetViewport(&got_vp);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetViewport", hr);
  }
  if (got_vp.X != vp.X || got_vp.Y != vp.Y ||
      got_vp.Width != vp.Width || got_vp.Height != vp.Height ||
      !NearlyEqual(got_vp.MinZ, vp.MinZ, 1e-6f) ||
      !NearlyEqual(got_vp.MaxZ, vp.MaxZ, 1e-6f)) {
    return reporter.Fail("GetViewport mismatch: got {X=%lu Y=%lu W=%lu H=%lu MinZ=%.6f MaxZ=%.6f} "
                         "expected {X=%lu Y=%lu W=%lu H=%lu MinZ=%.6f MaxZ=%.6f}",
                         (unsigned long)got_vp.X,
                         (unsigned long)got_vp.Y,
                         (unsigned long)got_vp.Width,
                         (unsigned long)got_vp.Height,
                         got_vp.MinZ,
                         got_vp.MaxZ,
                         (unsigned long)vp.X,
                         (unsigned long)vp.Y,
                         (unsigned long)vp.Width,
                         (unsigned long)vp.Height,
                         vp.MinZ,
                         vp.MaxZ);
  }

  // --- Scissor rect ---
  RECT scissor;
  scissor.left = 10;
  scissor.top = 20;
  scissor.right = 30;
  scissor.bottom = 40;

  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE)", hr);
  }
  hr = dev->SetScissorRect(&scissor);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetScissorRect", hr);
  }

  RECT got_scissor;
  ZeroMemory(&got_scissor, sizeof(got_scissor));
  hr = dev->GetScissorRect(&got_scissor);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetScissorRect", hr);
  }
  if (got_scissor.left != scissor.left || got_scissor.top != scissor.top ||
      got_scissor.right != scissor.right || got_scissor.bottom != scissor.bottom) {
    return reporter.Fail("GetScissorRect mismatch: got {%ld,%ld,%ld,%ld} expected {%ld,%ld,%ld,%ld}",
                         got_scissor.left, got_scissor.top, got_scissor.right, got_scissor.bottom,
                         scissor.left, scissor.top, scissor.right, scissor.bottom);
  }

  // --- Render state ---
  hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE)", hr);
  }
  DWORD rs_value = 0;
  hr = dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &rs_value);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderState(D3DRS_ALPHABLENDENABLE)", hr);
  }
  if (rs_value != TRUE) {
    return reporter.Fail("GetRenderState(D3DRS_ALPHABLENDENABLE) returned %lu expected %lu",
                         (unsigned long)rs_value,
                         (unsigned long)TRUE);
  }

  // --- Sampler state ---
  hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(stage0, ADDRESSU)", hr);
  }
  DWORD samp_value = 0;
  hr = dev->GetSamplerState(0, D3DSAMP_ADDRESSU, &samp_value);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetSamplerState(stage0, ADDRESSU)", hr);
  }
  if (samp_value != D3DTADDRESS_CLAMP) {
    return reporter.Fail("GetSamplerState(stage0, ADDRESSU) returned %lu expected %lu",
                         (unsigned long)samp_value,
                         (unsigned long)D3DTADDRESS_CLAMP);
  }

  // --- Texture binding ---
  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture", hr);
  }

  // --- Resource priority ---
  // Validate SetPriority returns the previous value and GetPriority returns the
  // latest value. Do not assume a particular default priority, since runtimes
  // can differ.
  {
    const DWORD old0 = tex->SetPriority(7);
    const DWORD old1 = tex->SetPriority(9);
    const DWORD got_prio = tex->GetPriority();
    if (old1 != 7) {
      return reporter.Fail("SetPriority mismatch: old1=%lu expected 7 (old0=%lu)",
                           (unsigned long)old1,
                           (unsigned long)old0);
    }
    if (got_prio != 9) {
      return reporter.Fail("GetPriority mismatch: got=%lu expected 9",
                           (unsigned long)got_prio);
    }
  }

  // --- AutoGenFilterType ---
  //
  // Some runtimes validate that SetAutoGenFilterType is only used with textures
  // created with D3DUSAGE_AUTOGENMIPMAP. If the call is rejected, try again with
  // an AUTOGENMIPMAP texture. If that still fails, treat as a supported skip.
  {
    HRESULT set_hr = tex->SetAutoGenFilterType(D3DTEXF_POINT);
    IDirect3DBaseTexture9* filter_tex = tex.get();
    ComPtr<IDirect3DTexture9> autogen_tex;
    if (set_hr == D3DERR_INVALIDCALL) {
      hr = dev->CreateTexture(16,
                              16,
                              0, // full chain (autogen)
                              D3DUSAGE_AUTOGENMIPMAP,
                              D3DFMT_A8R8G8B8,
                              D3DPOOL_DEFAULT,
                              autogen_tex.put(),
                              NULL);
      if (SUCCEEDED(hr) && autogen_tex) {
        filter_tex = autogen_tex.get();
        set_hr = filter_tex->SetAutoGenFilterType(D3DTEXF_POINT);
      }
    }

    if (FAILED(set_hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetAutoGenFilterType (hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)set_hr);
    } else {
      D3DTEXTUREFILTERTYPE got_filter = (D3DTEXTUREFILTERTYPE)0;
      hr = filter_tex->GetAutoGenFilterType(&got_filter);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetAutoGenFilterType", hr);
      }
      if (got_filter != D3DTEXF_POINT) {
        return reporter.Fail("GetAutoGenFilterType mismatch: got=%lu expected %lu",
                             (unsigned long)got_filter,
                             (unsigned long)D3DTEXF_POINT);
      }
    }
  }

  // --- Gamma ramp ---
  {
    D3DGAMMARAMP ramp;
    ZeroMemory(&ramp, sizeof(ramp));
    for (UINT i = 0; i < 256; ++i) {
      const DWORD raw = i * 257u;
      // Bias so we don't match the default identity ramp.
      const DWORD biased = (raw + 13u > 0xFFFFu) ? 0xFFFFu : (raw + 13u);
      const WORD v = (WORD)biased;
      ramp.red[i] = v;
      ramp.green[i] = v;
      ramp.blue[i] = v;
    }
    dev->SetGammaRamp(0, 0, &ramp);

    D3DGAMMARAMP got_ramp;
    ZeroMemory(&got_ramp, sizeof(got_ramp));
    dev->GetGammaRamp(0, &got_ramp);
    if (memcmp(&got_ramp, &ramp, sizeof(ramp)) != 0) {
      return reporter.Fail("GetGammaRamp mismatch after SetGammaRamp");
    }
  }

  // --- Clip status ---
  {
    D3DCLIPSTATUS9 clip;
    ZeroMemory(&clip, sizeof(clip));
    clip.ClipUnion = 0x00000011u;
    clip.ClipIntersection = 0x00000022u;
    hr = dev->SetClipStatus(&clip);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetClipStatus", hr);
    }
    D3DCLIPSTATUS9 got_clip;
    ZeroMemory(&got_clip, sizeof(got_clip));
    hr = dev->GetClipStatus(&got_clip);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetClipStatus", hr);
    }
    if (got_clip.ClipUnion != clip.ClipUnion || got_clip.ClipIntersection != clip.ClipIntersection) {
      return reporter.Fail("GetClipStatus mismatch: got {union=0x%08lX inter=0x%08lX} expected {union=0x%08lX inter=0x%08lX}",
                           (unsigned long)got_clip.ClipUnion,
                           (unsigned long)got_clip.ClipIntersection,
                           (unsigned long)clip.ClipUnion,
                           (unsigned long)clip.ClipIntersection);
    }
  }

  // --- Palettes ---
  //
  // Palettized texture support is runtime/adapter dependent. If palette APIs are
  // rejected, treat as a supported skip.
  {
    PALETTEENTRY pal[256];
    ZeroMemory(pal, sizeof(pal));
    for (UINT i = 0; i < 256; ++i) {
      pal[i].peRed = (BYTE)i;
      pal[i].peGreen = (BYTE)(i * 3);
      pal[i].peBlue = (BYTE)(i * 7);
      pal[i].peFlags = 0;
    }
    hr = dev->SetPaletteEntries(0, pal);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping palette APIs (SetPaletteEntries hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      hr = dev->SetCurrentTexturePalette(0);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping palette APIs (SetCurrentTexturePalette hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
      } else {
        PALETTEENTRY got_pal[256];
        ZeroMemory(got_pal, sizeof(got_pal));
        hr = dev->GetPaletteEntries(0, got_pal);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetPaletteEntries", hr);
        }
        if (memcmp(got_pal, pal, sizeof(pal)) != 0) {
          return reporter.Fail("GetPaletteEntries mismatch");
        }
        UINT got_cur = 0xFFFFFFFFu;
        hr = dev->GetCurrentTexturePalette(&got_cur);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetCurrentTexturePalette", hr);
        }
        if (got_cur != 0) {
          return reporter.Fail("GetCurrentTexturePalette mismatch: got=%u expected=0",
                               (unsigned)got_cur);
        }
      }
    }
  }

  hr = dev->SetTexture(0, tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture(0)", hr);
  }
  ComPtr<IDirect3DBaseTexture9> got_tex0;
  hr = dev->GetTexture(0, got_tex0.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTexture(0)", hr);
  }
  if (got_tex0.get() != tex.get()) {
    return reporter.Fail("GetTexture(0) mismatch: got %p expected %p", got_tex0.get(), tex.get());
  }

  ComPtr<IDirect3DBaseTexture9> got_tex1;
  hr = dev->GetTexture(1, got_tex1.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTexture(1)", hr);
  }
  if (got_tex1.get() != NULL) {
    return reporter.Fail("GetTexture(1) expected NULL but got %p", got_tex1.get());
  }

  // --- Stream source ---
  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }
  const UINT kStreamOffset = 16;
  const UINT kStreamStride = 32;
  hr = dev->SetStreamSource(0, vb.get(), kStreamOffset, kStreamStride);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(0)", hr);
  }
  ComPtr<IDirect3DVertexBuffer9> got_vb;
  UINT got_offset = 0;
  UINT got_stride = 0;
  hr = dev->GetStreamSource(0, got_vb.put(), &got_offset, &got_stride);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetStreamSource(0)", hr);
  }
  if (got_vb.get() != vb.get() || got_offset != kStreamOffset || got_stride != kStreamStride) {
    return reporter.Fail("GetStreamSource mismatch: got {vb=%p off=%u stride=%u} expected {vb=%p off=%u stride=%u}",
                         got_vb.get(),
                         (unsigned)got_offset,
                         (unsigned)got_stride,
                         vb.get(),
                         (unsigned)kStreamOffset,
                         (unsigned)kStreamStride);
  }

  // Also validate "not bound" behavior for an unused stream.
  ComPtr<IDirect3DVertexBuffer9> got_vb1;
  UINT got_offset1 = 0xFFFFFFFFu;
  UINT got_stride1 = 0xFFFFFFFFu;
  hr = dev->GetStreamSource(1, got_vb1.put(), &got_offset1, &got_stride1);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetStreamSource(1)", hr);
  }
  if (got_vb1.get() != NULL || got_offset1 != 0 || got_stride1 != 0) {
    return reporter.Fail("GetStreamSource(1) expected {NULL,0,0} but got {vb=%p off=%u stride=%u}",
                         got_vb1.get(),
                         (unsigned)got_offset1,
                         (unsigned)got_stride1);
  }

  // --- Indices ---
  ComPtr<IDirect3DIndexBuffer9> ib;
  hr = dev->CreateIndexBuffer(256, 0, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateIndexBuffer", hr);
  }
  hr = dev->SetIndices(ib.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetIndices", hr);
  }
  ComPtr<IDirect3DIndexBuffer9> got_ib;
  hr = dev->GetIndices(got_ib.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetIndices", hr);
  }
  if (got_ib.get() != ib.get()) {
    return reporter.Fail("GetIndices mismatch: got %p expected %p", got_ib.get(), ib.get());
  }

  // --- Vertex declaration ---
  const D3DVERTEXELEMENT9 elems[] = {
      {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {0, 12, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> decl;
  hr = dev->CreateVertexDeclaration(elems, decl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }
  hr = dev->SetVertexDeclaration(decl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexDeclaration", hr);
  }
  ComPtr<IDirect3DVertexDeclaration9> got_decl;
  hr = dev->GetVertexDeclaration(got_decl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetVertexDeclaration", hr);
  }
  if (got_decl.get() != decl.get()) {
    return reporter.Fail("GetVertexDeclaration mismatch: got %p expected %p", got_decl.get(), decl.get());
  }

  // --- FVF ---
  const DWORD kFvf = D3DFVF_XYZRHW | D3DFVF_DIFFUSE;
  hr = dev->SetFVF(kFvf);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetFVF", hr);
  }
  DWORD got_fvf = 0;
  hr = dev->GetFVF(&got_fvf);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetFVF", hr);
  }
  if (got_fvf != kFvf) {
    return reporter.Fail("GetFVF mismatch: got 0x%08lX expected 0x%08lX",
                         (unsigned long)got_fvf,
                         (unsigned long)kFvf);
  }

  // --- Shaders (NULL bindings) ---
  hr = dev->SetVertexShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader(NULL)", hr);
  }
  hr = dev->SetPixelShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader(NULL)", hr);
  }
  IDirect3DVertexShader9* got_vs = NULL;
  hr = dev->GetVertexShader(&got_vs);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetVertexShader", hr);
  }
  if (got_vs != NULL) {
    if (got_vs) {
      got_vs->Release();
    }
    return reporter.Fail("GetVertexShader expected NULL but got %p", got_vs);
  }
  IDirect3DPixelShader9* got_ps = NULL;
  hr = dev->GetPixelShader(&got_ps);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetPixelShader", hr);
  }
  if (got_ps != NULL) {
    if (got_ps) {
      got_ps->Release();
    }
    return reporter.Fail("GetPixelShader expected NULL but got %p", got_ps);
  }

  // --- Shader float constants ---
  float vs_consts[8];
  vs_consts[0] = 1.0f;
  vs_consts[1] = 2.0f;
  vs_consts[2] = 3.0f;
  vs_consts[3] = 4.0f;
  vs_consts[4] = 5.0f;
  vs_consts[5] = 6.0f;
  vs_consts[6] = 7.0f;
  vs_consts[7] = 8.0f;
  hr = dev->SetVertexShaderConstantF(5, vs_consts, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShaderConstantF", hr);
  }

  float got_vs_consts[8];
  for (int i = 0; i < 8; ++i) {
    got_vs_consts[i] = -123.0f;
  }
  hr = dev->GetVertexShaderConstantF(5, got_vs_consts, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetVertexShaderConstantF", hr);
  }
  for (int i = 0; i < 8; ++i) {
    if (got_vs_consts[i] != vs_consts[i]) {
      return reporter.Fail("GetVertexShaderConstantF mismatch at idx=%d got=%f expected=%f",
                           i, got_vs_consts[i], vs_consts[i]);
    }
  }

  float ps_consts[4];
  ps_consts[0] = 9.0f;
  ps_consts[1] = 10.0f;
  ps_consts[2] = 11.0f;
  ps_consts[3] = 12.0f;
  hr = dev->SetPixelShaderConstantF(0, ps_consts, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantF", hr);
  }

  float got_ps_consts[4];
  for (int i = 0; i < 4; ++i) {
    got_ps_consts[i] = -456.0f;
  }
  hr = dev->GetPixelShaderConstantF(0, got_ps_consts, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetPixelShaderConstantF", hr);
  }
  for (int i = 0; i < 4; ++i) {
    if (got_ps_consts[i] != ps_consts[i]) {
      return reporter.Fail("GetPixelShaderConstantF mismatch at idx=%d got=%f expected=%f",
                           i, got_ps_consts[i], ps_consts[i]);
    }
  }

  // --- Shader int constants ---
  int vsi[8] = {1, 2, 3, 4, 5, 6, 7, 8};
  hr = dev->SetVertexShaderConstantI(7, vsi, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShaderConstantI", hr);
  }
  int got_vsi[8];
  for (int i = 0; i < 8; ++i) {
    got_vsi[i] = 0x12345678;
  }
  hr = dev->GetVertexShaderConstantI(7, got_vsi, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetVertexShaderConstantI", hr);
  }
  for (int i = 0; i < 8; ++i) {
    if (got_vsi[i] != vsi[i]) {
      return reporter.Fail("GetVertexShaderConstantI mismatch at idx=%d got=%d expected=%d",
                           i, got_vsi[i], vsi[i]);
    }
  }

  int psi[4] = {9, 10, 11, 12};
  hr = dev->SetPixelShaderConstantI(0, psi, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantI", hr);
  }
  int got_psi[4];
  for (int i = 0; i < 4; ++i) {
    got_psi[i] = 0x76543210;
  }
  hr = dev->GetPixelShaderConstantI(0, got_psi, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetPixelShaderConstantI", hr);
  }
  for (int i = 0; i < 4; ++i) {
    if (got_psi[i] != psi[i]) {
      return reporter.Fail("GetPixelShaderConstantI mismatch at idx=%d got=%d expected=%d",
                           i, got_psi[i], psi[i]);
    }
  }

  // --- Shader bool constants ---
  BOOL vsb[4] = {TRUE, FALSE, TRUE, TRUE};
  hr = dev->SetVertexShaderConstantB(3, vsb, 4);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShaderConstantB", hr);
  }
  BOOL got_vsb[4] = {FALSE, FALSE, FALSE, FALSE};
  hr = dev->GetVertexShaderConstantB(3, got_vsb, 4);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetVertexShaderConstantB", hr);
  }
  for (int i = 0; i < 4; ++i) {
    if ((got_vsb[i] ? 1 : 0) != (vsb[i] ? 1 : 0)) {
      return reporter.Fail("GetVertexShaderConstantB mismatch at idx=%d got=%d expected=%d",
                           i, got_vsb[i] ? 1 : 0, vsb[i] ? 1 : 0);
    }
  }

  BOOL psb[2] = {FALSE, TRUE};
  hr = dev->SetPixelShaderConstantB(0, psb, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantB", hr);
  }
  BOOL got_psb[2] = {TRUE, TRUE};
  hr = dev->GetPixelShaderConstantB(0, got_psb, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetPixelShaderConstantB", hr);
  }
  for (int i = 0; i < 2; ++i) {
    if ((got_psb[i] ? 1 : 0) != (psb[i] ? 1 : 0)) {
      return reporter.Fail("GetPixelShaderConstantB mismatch at idx=%d got=%d expected=%d",
                           i, got_psb[i] ? 1 : 0, psb[i] ? 1 : 0);
    }
  }

  // --- Fixed-function material ---
  D3DMATERIAL9 mat;
  ZeroMemory(&mat, sizeof(mat));
  mat.Diffuse.r = 0.1f;
  mat.Diffuse.g = 0.2f;
  mat.Diffuse.b = 0.3f;
  mat.Diffuse.a = 0.4f;
  mat.Ambient.r = 0.5f;
  mat.Ambient.g = 0.6f;
  mat.Ambient.b = 0.7f;
  mat.Ambient.a = 0.8f;
  mat.Specular.r = 0.9f;
  mat.Specular.g = 0.25f;
  mat.Specular.b = 0.75f;
  mat.Specular.a = 1.0f;
  mat.Emissive.r = 0.0f;
  mat.Emissive.g = 0.125f;
  mat.Emissive.b = 0.25f;
  mat.Emissive.a = 0.375f;
  mat.Power = 3.5f;

  hr = dev->SetMaterial(&mat);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetMaterial", hr);
  }

  D3DMATERIAL9 got_mat;
  ZeroMemory(&got_mat, sizeof(got_mat));
  hr = dev->GetMaterial(&got_mat);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetMaterial", hr);
  }

  const float kMatEps = 1e-6f;
  if (!NearlyEqual(got_mat.Diffuse.r, mat.Diffuse.r, kMatEps) ||
      !NearlyEqual(got_mat.Diffuse.g, mat.Diffuse.g, kMatEps) ||
      !NearlyEqual(got_mat.Diffuse.b, mat.Diffuse.b, kMatEps) ||
      !NearlyEqual(got_mat.Diffuse.a, mat.Diffuse.a, kMatEps) ||
      !NearlyEqual(got_mat.Ambient.r, mat.Ambient.r, kMatEps) ||
      !NearlyEqual(got_mat.Ambient.g, mat.Ambient.g, kMatEps) ||
      !NearlyEqual(got_mat.Ambient.b, mat.Ambient.b, kMatEps) ||
      !NearlyEqual(got_mat.Ambient.a, mat.Ambient.a, kMatEps) ||
      !NearlyEqual(got_mat.Specular.r, mat.Specular.r, kMatEps) ||
      !NearlyEqual(got_mat.Specular.g, mat.Specular.g, kMatEps) ||
      !NearlyEqual(got_mat.Specular.b, mat.Specular.b, kMatEps) ||
      !NearlyEqual(got_mat.Specular.a, mat.Specular.a, kMatEps) ||
      !NearlyEqual(got_mat.Emissive.r, mat.Emissive.r, kMatEps) ||
      !NearlyEqual(got_mat.Emissive.g, mat.Emissive.g, kMatEps) ||
      !NearlyEqual(got_mat.Emissive.b, mat.Emissive.b, kMatEps) ||
      !NearlyEqual(got_mat.Emissive.a, mat.Emissive.a, kMatEps) ||
      !NearlyEqual(got_mat.Power, mat.Power, kMatEps)) {
    return reporter.Fail("GetMaterial mismatch");
  }

  // --- Fixed-function lights ---
  D3DLIGHT9 light;
  ZeroMemory(&light, sizeof(light));
  light.Type = D3DLIGHT_POINT;
  light.Diffuse.r = 0.25f;
  light.Diffuse.g = 0.5f;
  light.Diffuse.b = 0.75f;
  light.Diffuse.a = 1.0f;
  light.Position.x = 1.0f;
  light.Position.y = 2.0f;
  light.Position.z = 3.0f;
  light.Range = 100.0f;
  light.Attenuation0 = 1.0f;
  light.Attenuation1 = 0.0f;
  light.Attenuation2 = 0.0f;

  hr = dev->SetLight(0, &light);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetLight(0)", hr);
  }

  D3DLIGHT9 got_light;
  ZeroMemory(&got_light, sizeof(got_light));
  hr = dev->GetLight(0, &got_light);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetLight(0)", hr);
  }

  if (got_light.Type != light.Type ||
      !NearlyEqual(got_light.Diffuse.r, light.Diffuse.r, kMatEps) ||
      !NearlyEqual(got_light.Diffuse.g, light.Diffuse.g, kMatEps) ||
      !NearlyEqual(got_light.Diffuse.b, light.Diffuse.b, kMatEps) ||
      !NearlyEqual(got_light.Diffuse.a, light.Diffuse.a, kMatEps) ||
      !NearlyEqual(got_light.Position.x, light.Position.x, kMatEps) ||
      !NearlyEqual(got_light.Position.y, light.Position.y, kMatEps) ||
      !NearlyEqual(got_light.Position.z, light.Position.z, kMatEps) ||
      !NearlyEqual(got_light.Range, light.Range, kMatEps) ||
      !NearlyEqual(got_light.Attenuation0, light.Attenuation0, kMatEps) ||
      !NearlyEqual(got_light.Attenuation1, light.Attenuation1, kMatEps) ||
      !NearlyEqual(got_light.Attenuation2, light.Attenuation2, kMatEps)) {
    return reporter.Fail("GetLight mismatch");
  }

  hr = dev->LightEnable(0, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("LightEnable(0, TRUE)", hr);
  }
  BOOL got_light_en = FALSE;
  hr = dev->GetLightEnable(0, &got_light_en);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetLightEnable(0)", hr);
  }
  if (!got_light_en) {
    return reporter.Fail("GetLightEnable(0) expected TRUE");
  }

  hr = dev->LightEnable(0, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("LightEnable(0, FALSE)", hr);
  }
  got_light_en = TRUE;
  hr = dev->GetLightEnable(0, &got_light_en);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetLightEnable(0) after disable", hr);
  }
  if (got_light_en) {
    return reporter.Fail("GetLightEnable(0) expected FALSE after disable");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExGettersSanity(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}
