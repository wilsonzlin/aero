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

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  DWORD color;
};

static void FillTriangle(Vertex* v,
                         float x0, float y0,
                         float x1, float y1,
                         float x2, float y2,
                         DWORD color) {
  v[0].x = x0;
  v[0].y = y0;
  v[0].z = 0.5f;
  v[0].rhw = 1.0f;
  v[0].color = color;

  v[1].x = x1;
  v[1].y = y1;
  v[1].z = 0.5f;
  v[1].rhw = 1.0f;
  v[1].color = color;

  v[2].x = x2;
  v[2].y = y2;
  v[2].z = 0.5f;
  v[2].rhw = 1.0f;
  v[2].color = color;
}

static int RunD3D9DynamicVbLockSemantics(int argc, char** argv) {
  const char* kTestName = "d3d9_dynamic_vb_lock_semantics";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--json[=PATH]] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--require-vid=0x####] [--require-did=0x####]",
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9DynamicVbLockSemantics",
                                              L"AeroGPU D3D9 dynamic VB lock semantics",
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

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  const DWORD kFvf = D3DFVF_XYZRHW | D3DFVF_DIFFUSE;
  hr = dev->SetFVF(kFvf);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  // Dynamic vertex buffer large enough for three triangles (phase 2).
  const UINT kMaxVerts = 9;
  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(sizeof(Vertex) * kMaxVerts,
                               D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY,
                               kFvf,
                               D3DPOOL_DEFAULT,
                               vb.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }

  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }

  // Cache the real backbuffer so we can restore it after offscreen rendering.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetBackBuffer", hr);
  }

  // ---------------------------------------------------------------------------
  // Phase 1: repeated DISCARD updates + draws without intermediate submission.
  // ---------------------------------------------------------------------------
  const DWORD colors[] = {
      D3DCOLOR_XRGB(255, 0, 0),    // red
      D3DCOLOR_XRGB(0, 255, 0),    // green
      D3DCOLOR_XRGB(0, 0, 255),    // blue
      D3DCOLOR_XRGB(255, 255, 0),  // yellow
      D3DCOLOR_XRGB(255, 0, 255),  // magenta
      D3DCOLOR_XRGB(0, 255, 255),  // cyan
      D3DCOLOR_XRGB(255, 128, 0),  // orange
      D3DCOLOR_XRGB(128, 0, 255),  // purple-ish
  };
  const UINT kIterations = (UINT)aerogpu_test::ARRAYSIZE(colors);

  std::vector<ComPtr<IDirect3DSurface9> > phase1_rts;
  phase1_rts.resize(kIterations);

  for (UINT i = 0; i < kIterations; ++i) {
    hr = dev->CreateRenderTarget(kWidth,
                                 kHeight,
                                 D3DFMT_X8R8G8B8,
                                 D3DMULTISAMPLE_NONE,
                                 0,
                                 FALSE,
                                 phase1_rts[i].put(),
                                 NULL);
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateRenderTarget", hr);
    }
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }

  for (UINT i = 0; i < kIterations; ++i) {
    hr = dev->SetRenderTarget(0, phase1_rts[i].get());
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("SetRenderTarget", hr);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 0), 1.0f, 0);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("Clear", hr);
    }

    Vertex* v = NULL;
    hr = vb->Lock(0, sizeof(Vertex) * 3, (void**)&v, D3DLOCK_DISCARD);
    if (FAILED(hr) || !v) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DVertexBuffer9::Lock(DISCARD)", hr);
    }

    FillTriangle(v,
                 (float)kWidth * 0.25f, (float)kHeight * 0.25f,
                 (float)kWidth * 0.75f, (float)kHeight * 0.25f,
                 (float)kWidth * 0.50f, (float)kHeight * 0.75f,
                 colors[i]);

    hr = vb->Unlock();
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DVertexBuffer9::Unlock", hr);
    }

    hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 1);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("DrawPrimitive", hr);
    }
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }

  // Restore backbuffer as RT0 for PresentEx.
  dev->SetRenderTarget(0, backbuffer.get());

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(kWidth,
                                        kHeight,
                                        D3DFMT_X8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  for (UINT i = 0; i < kIterations; ++i) {
    hr = dev->GetRenderTargetData(phase1_rts[i].get(), sysmem.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderTargetData (phase1)", hr);
    }

    D3DLOCKED_RECT lr;
    ZeroMemory(&lr, sizeof(lr));
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (FAILED(hr)) {
      return reporter.FailHresult("LockRect (phase1)", hr);
    }

    const int cx = kWidth / 2;
    const int cy = kHeight / 2;
    const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
    sysmem->UnlockRect();

    const uint32_t expected = (uint32_t)colors[i];
    if ((pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
      return reporter.Fail("phase1 pixel mismatch at iter=%u: got=0x%08lX expected=0x%08lX",
                           (unsigned)i,
                           (unsigned long)pixel,
                           (unsigned long)expected);
    }
  }

  // ---------------------------------------------------------------------------
  // Phase 2: NOOVERWRITE appends must preserve previously written vertices.
  // ---------------------------------------------------------------------------
  ComPtr<IDirect3DSurface9> phase2_rt;
  hr = dev->CreateRenderTarget(kWidth,
                               kHeight,
                               D3DFMT_X8R8G8B8,
                               D3DMULTISAMPLE_NONE,
                               0,
                               FALSE,
                               phase2_rt.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTarget (phase2)", hr);
  }

  // Fill 3 triangles in one VB: first DISCARD, then NOOVERWRITE appends.
  {
    Vertex* v = NULL;
    hr = vb->Lock(0, sizeof(Vertex) * 3, (void**)&v, D3DLOCK_DISCARD);
    if (FAILED(hr) || !v) {
      return reporter.FailHresult("Lock(DISCARD) (phase2)", hr);
    }
    FillTriangle(v,
                 20.0f, 60.0f,
                 80.0f, 60.0f,
                 50.0f, 180.0f,
                 D3DCOLOR_XRGB(255, 0, 0));
    vb->Unlock();

    hr = vb->Lock(sizeof(Vertex) * 3, sizeof(Vertex) * 3, (void**)&v, D3DLOCK_NOOVERWRITE);
    if (FAILED(hr) || !v) {
      return reporter.FailHresult("Lock(NOOVERWRITE) (phase2, tri2)", hr);
    }
    FillTriangle(v,
                 90.0f, 60.0f,
                 160.0f, 60.0f,
                 125.0f, 180.0f,
                 D3DCOLOR_XRGB(0, 255, 0));
    vb->Unlock();

    hr = vb->Lock(sizeof(Vertex) * 6, sizeof(Vertex) * 3, (void**)&v, D3DLOCK_NOOVERWRITE);
    if (FAILED(hr) || !v) {
      return reporter.FailHresult("Lock(NOOVERWRITE) (phase2, tri3)", hr);
    }
    FillTriangle(v,
                 170.0f, 60.0f,
                 240.0f, 60.0f,
                 205.0f, 180.0f,
                 D3DCOLOR_XRGB(0, 0, 255));
    vb->Unlock();
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene (phase2)", hr);
  }
  hr = dev->SetRenderTarget(0, phase2_rt.get());
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("SetRenderTarget (phase2)", hr);
  }
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 0), 1.0f, 0);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("Clear (phase2)", hr);
  }
  hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 3);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawPrimitive (phase2)", hr);
  }
  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene (phase2)", hr);
  }

  dev->SetRenderTarget(0, backbuffer.get());

  hr = dev->GetRenderTargetData(phase2_rt.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData (phase2)", hr);
  }
  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter.FailHresult("LockRect (phase2)", hr);
  }
  const uint32_t px0 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 50, 100);
  const uint32_t px1 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 125, 100);
  const uint32_t px2 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 205, 100);
  sysmem->UnlockRect();

  const uint32_t exp0 = (uint32_t)D3DCOLOR_XRGB(255, 0, 0);
  const uint32_t exp1 = (uint32_t)D3DCOLOR_XRGB(0, 255, 0);
  const uint32_t exp2 = (uint32_t)D3DCOLOR_XRGB(0, 0, 255);
  if ((px0 & 0x00FFFFFFu) != (exp0 & 0x00FFFFFFu) ||
      (px1 & 0x00FFFFFFu) != (exp1 & 0x00FFFFFFu) ||
      (px2 & 0x00FFFFFFu) != (exp2 & 0x00FFFFFFu)) {
    return reporter.Fail("phase2 pixel mismatch: left=0x%08lX exp=0x%08lX mid=0x%08lX exp=0x%08lX right=0x%08lX exp=0x%08lX",
                         (unsigned long)px0, (unsigned long)exp0,
                         (unsigned long)px1, (unsigned long)exp1,
                         (unsigned long)px2, (unsigned long)exp2);
  }

  // ---------------------------------------------------------------------------
  // Phase 3: dynamic index buffer DISCARD + NOOVERWRITE overlap must not corrupt
  // previously-recorded draws.
  // ---------------------------------------------------------------------------
  //
  // This uses the fixed-function XYZ|DIFFUSE path (non-pretransformed vertices)
  // because AeroGPU's fixed-function XYZRHW indexed draws expand indices into a
  // temporary vertex stream and do not exercise the GPU index-buffer binding.
  {
    const DWORD kFvfXyzDiffuse = D3DFVF_XYZ | D3DFVF_DIFFUSE;
    hr = dev->SetFVF(kFvfXyzDiffuse);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF(XYZ|DIFFUSE) (phase3)", hr);
    }

    // Vertex buffer contains two identical clip-space triangles (red/green) plus
    // an offscreen sentinel triangle that forces the fixed-function CPU transform
    // path to upload the full vertex range regardless of which test triangle is
    // selected by indices.
    ComPtr<IDirect3DVertexBuffer9> vb_xyz;
    const UINT kVtxCount = 9;
    hr = dev->CreateVertexBuffer(sizeof(VertexXyzDiffuse) * kVtxCount,
                                 D3DUSAGE_WRITEONLY,
                                 kFvfXyzDiffuse,
                                 D3DPOOL_DEFAULT,
                                 vb_xyz.put(),
                                 NULL);
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateVertexBuffer (phase3)", hr);
    }

    VertexXyzDiffuse* v = NULL;
    hr = vb_xyz->Lock(0, 0, (void**)&v, 0);
    if (FAILED(hr) || !v) {
      return reporter.FailHresult("vb_xyz->Lock (phase3)", hr);
    }

    // Sentinel vertices (fully outside clip space: x>1 and y>1).
    v[0].x = 2.0f;
    v[0].y = 2.0f;
    v[0].z = 0.5f;
    v[0].color = D3DCOLOR_XRGB(0, 0, 0);
    v[7].x = 2.5f;
    v[7].y = 2.0f;
    v[7].z = 0.5f;
    v[7].color = D3DCOLOR_XRGB(0, 0, 0);
    v[8].x = 2.0f;
    v[8].y = 2.5f;
    v[8].z = 0.5f;
    v[8].color = D3DCOLOR_XRGB(0, 0, 0);

    // Red triangle at indices 1..3 (clip-space, covers screen center).
    v[1].x = -1.0f;
    v[1].y = -1.0f;
    v[1].z = 0.5f;
    v[1].color = D3DCOLOR_XRGB(255, 0, 0);
    v[2].x = 1.0f;
    v[2].y = -1.0f;
    v[2].z = 0.5f;
    v[2].color = D3DCOLOR_XRGB(255, 0, 0);
    v[3].x = 0.0f;
    v[3].y = 1.0f;
    v[3].z = 0.5f;
    v[3].color = D3DCOLOR_XRGB(255, 0, 0);

    // Green triangle at indices 4..6 (same shape, different color).
    v[4].x = -1.0f;
    v[4].y = -1.0f;
    v[4].z = 0.5f;
    v[4].color = D3DCOLOR_XRGB(0, 255, 0);
    v[5].x = 1.0f;
    v[5].y = -1.0f;
    v[5].z = 0.5f;
    v[5].color = D3DCOLOR_XRGB(0, 255, 0);
    v[6].x = 0.0f;
    v[6].y = 1.0f;
    v[6].z = 0.5f;
    v[6].color = D3DCOLOR_XRGB(0, 255, 0);

    hr = vb_xyz->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("vb_xyz->Unlock (phase3)", hr);
    }

    ComPtr<IDirect3DIndexBuffer9> ib;
    const UINT kIndexCount = 6; // 2 triangles
    hr = dev->CreateIndexBuffer(sizeof(WORD) * kIndexCount,
                                D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY,
                                D3DFMT_INDEX16,
                                D3DPOOL_DEFAULT,
                                ib.put(),
                                NULL);
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateIndexBuffer (phase3)", hr);
    }

    hr = dev->SetStreamSource(0, vb_xyz.get(), 0, sizeof(VertexXyzDiffuse));
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(vb_xyz) (phase3)", hr);
    }
    hr = dev->SetIndices(ib.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetIndices (phase3)", hr);
    }

    ComPtr<IDirect3DSurface9> ib_rts[2];
    for (int i = 0; i < 2; ++i) {
      hr = dev->CreateRenderTarget(kWidth,
                                   kHeight,
                                   D3DFMT_X8R8G8B8,
                                   D3DMULTISAMPLE_NONE,
                                   0,
                                   FALSE,
                                   ib_rts[i].put(),
                                   NULL);
      if (FAILED(hr)) {
        return reporter.FailHresult("CreateRenderTarget (phase3)", hr);
      }
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("BeginScene (phase3)", hr);
    }

    // First draw uses DISCARD; second draw uses NOOVERWRITE on the same bytes.
    // Correct behavior requires the NOOVERWRITE lock to fall back to DISCARD
    // (rename) so the first draw's indices remain intact.
    for (int iter = 0; iter < 2; ++iter) {
      hr = dev->SetRenderTarget(0, ib_rts[iter].get());
      if (FAILED(hr)) {
        dev->EndScene();
        return reporter.FailHresult("SetRenderTarget (phase3)", hr);
      }
      hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 0), 1.0f, 0);
      if (FAILED(hr)) {
        dev->EndScene();
        return reporter.FailHresult("Clear (phase3)", hr);
      }

      WORD* idx = NULL;
      const DWORD flags = (iter == 0) ? D3DLOCK_DISCARD : D3DLOCK_NOOVERWRITE;
      hr = ib->Lock(0, sizeof(WORD) * kIndexCount, (void**)&idx, flags);
      if (FAILED(hr) || !idx) {
        dev->EndScene();
        return reporter.FailHresult("IDirect3DIndexBuffer9::Lock (phase3)", hr);
      }

      // Sentinel triangle: (0,7,8) (offscreen, clipped).
      idx[0] = 0;
      idx[1] = 7;
      idx[2] = 8;
      // Test triangle: either red (1,2,3) or green (4,5,6).
      const WORD base = (iter == 0) ? 1 : 4;
      idx[3] = base;
      idx[4] = base + 1;
      idx[5] = base + 2;

      hr = ib->Unlock();
      if (FAILED(hr)) {
        dev->EndScene();
        return reporter.FailHresult("IDirect3DIndexBuffer9::Unlock (phase3)", hr);
      }

      hr = dev->DrawIndexedPrimitive(D3DPT_TRIANGLELIST,
                                     0,           // BaseVertexIndex
                                     0,           // MinVertexIndex
                                     kVtxCount,   // NumVertices
                                     0,           // StartIndex
                                     2);          // PrimitiveCount
      if (FAILED(hr)) {
        dev->EndScene();
        return reporter.FailHresult("DrawIndexedPrimitive (phase3)", hr);
      }
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("EndScene (phase3)", hr);
    }

    dev->SetRenderTarget(0, backbuffer.get());

    for (int iter = 0; iter < 2; ++iter) {
      hr = dev->GetRenderTargetData(ib_rts[iter].get(), sysmem.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("GetRenderTargetData (phase3)", hr);
      }
      D3DLOCKED_RECT lr;
      ZeroMemory(&lr, sizeof(lr));
      hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
      if (FAILED(hr)) {
        return reporter.FailHresult("LockRect (phase3)", hr);
      }
      const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, kWidth / 2, kHeight / 2);
      sysmem->UnlockRect();

      const uint32_t expected = (uint32_t)((iter == 0) ? D3DCOLOR_XRGB(255, 0, 0) : D3DCOLOR_XRGB(0, 255, 0));
      if ((pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
        return reporter.Fail("phase3 pixel mismatch at iter=%d: got=0x%08lX expected=0x%08lX",
                             iter,
                             (unsigned long)pixel,
                             (unsigned long)expected);
      }
    }
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9DynamicVbLockSemantics(argc, argv);
  Sleep(30);
  return rc;
}
