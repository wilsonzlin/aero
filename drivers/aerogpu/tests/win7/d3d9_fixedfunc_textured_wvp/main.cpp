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

static int RunD3D9FixedFuncTexturedWvp(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_textured_wvp";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncTexturedWvp",
                                              L"AeroGPU D3D9 FixedFunc Textured WVP",
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

  // Ensure a known viewport (some runtimes may leave it uninitialized until the
  // first Present; make this test self-contained).
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

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);

  // Create a 2x2 texture with distinct colors.
  //
  // Use a SYSTEMMEM staging texture + UpdateTexture so this works reliably on
  // D3D9Ex (which does not support D3DPOOL_MANAGED resources).
  ComPtr<IDirect3DTexture9> sys_tex;
  hr = dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture (SYSTEMMEM)", hr);
  }

  D3DLOCKED_RECT tlr;
  ZeroMemory(&tlr, sizeof(tlr));
  hr = sys_tex->LockRect(0, &tlr, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DTexture9::LockRect", hr);
  }
  uint8_t* base = (uint8_t*)tlr.pBits;
  uint32_t* row0 = (uint32_t*)base;
  uint32_t* row1 = (uint32_t*)(base + tlr.Pitch);

  // D3DFMT_A8R8G8B8 stores pixels as AARRGGBB in memory (little-endian BGRA bytes).
  row0[0] = D3DCOLOR_XRGB(255, 0, 0);      // top-left: red
  row0[1] = D3DCOLOR_XRGB(0, 255, 0);      // top-right: green
  row1[0] = D3DCOLOR_XRGB(0, 0, 255);      // bottom-left: blue
  row1[1] = D3DCOLOR_XRGB(255, 255, 0);    // bottom-right: yellow

  sys_tex->UnlockRect(0);

  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(2, 2, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture (DEFAULT)", hr);
  }

  hr = dev->UpdateTexture(sys_tex.get(), tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::UpdateTexture", hr);
  }

  hr = dev->SetTexture(0, tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTexture", hr);
  }

  // Force point sampling so the expected texel is unambiguous.
  dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);

  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
  dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
  dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);

  // Place a quad around NDC origin via WORLD/VIEW/PROJECTION transforms.
  //
  // The quad's vertex positions are initially on the left side of clip space.
  // WORLD + VIEW shift it rightwards, but not enough to reach the center. The
  // PROJECTION matrix then applies an additional X scale + translation.
  //
  // This means the center pixel samples the quad *only* when the fixed-function
  // fallback correctly applies the full WVP matrix in the correct order. If any
  // of WORLD/VIEW/PROJECTION is ignored (or if PROJECTION is applied first),
  // the center pixel stays at the clear color.
  D3DMATRIX world;
  ZeroMemory(&world, sizeof(world));
  world._11 = 1.0f;
  world._22 = 1.0f;
  world._33 = 1.0f;
  world._44 = 1.0f;
  world._41 = 0.2f; // +X

  D3DMATRIX view;
  ZeroMemory(&view, sizeof(view));
  view._11 = 1.0f;
  view._22 = 1.0f;
  view._33 = 1.0f;
  view._44 = 1.0f;
  view._41 = 0.38f; // +X

  D3DMATRIX proj;
  ZeroMemory(&proj, sizeof(proj));
  proj._11 = 0.5f; // X scale
  proj._22 = 1.0f;
  proj._33 = 1.0f;
  proj._44 = 1.0f;
  proj._41 = 0.1f; // +X

  dev->SetTransform(D3DTS_WORLD, &world);
  dev->SetTransform(D3DTS_VIEW, &view);
  dev->SetTransform(D3DTS_PROJECTION, &proj);

  const DWORD kWhite = D3DCOLOR_XRGB(255, 255, 255);
  Vertex v[4];
  v[0].x = -0.9f; v[0].y = 0.1f;  v[0].z = 0.5f; v[0].color = kWhite; v[0].u = 0.5f; v[0].v = 0.5f; // top-left
  v[1].x = -0.7f; v[1].y = 0.1f;  v[1].z = 0.5f; v[1].color = kWhite; v[1].u = 1.0f; v[1].v = 0.5f; // top-right
  v[2].x = -0.9f; v[2].y = -0.1f; v[2].z = 0.5f; v[2].color = kWhite; v[2].u = 0.5f; v[2].v = 1.0f; // bottom-left
  v[3].x = -0.7f; v[3].y = -0.1f; v[3].z = 0.5f; v[3].color = kWhite; v[3].u = 1.0f; v[3].v = 1.0f; // bottom-right

  const DWORD kClear = D3DCOLOR_XRGB(0, 0, 0);
  const uint32_t expected = D3DCOLOR_XRGB(255, 255, 0); // yellow (bottom-right texel)

  auto DrawAndValidateCenterPixel = [&](const char* label, const wchar_t* dump_leaf) -> int {
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClear, 1.0f, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
    }

    hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLESTRIP, 2, v, sizeof(Vertex));
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
    }

    // Read back the backbuffer (before PresentEx: DISCARD makes contents undefined after present).
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
    hr = dev->CreateOffscreenPlainSurface(desc.Width,
                                          desc.Height,
                                          desc.Format,
                                          D3DPOOL_SYSTEMMEM,
                                          sysmem.put(),
                                          NULL);
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

    if ((center & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
      if (dump && dump_leaf) {
        std::string err;
        const std::wstring bmp_path =
            aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), dump_leaf);
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
      }
      sysmem->UnlockRect();
      return reporter.Fail("pixel mismatch (%s): center=0x%08lX expected 0x%08lX",
                           label ? label : "?",
                           (unsigned long)center,
                           (unsigned long)expected);
    }

    if ((corner & 0x00FFFFFFu) != (kClear & 0x00FFFFFFu)) {
      if (dump && dump_leaf) {
        std::string err;
        const std::wstring bmp_path =
            aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), dump_leaf);
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
      }
      sysmem->UnlockRect();
      return reporter.Fail("pixel mismatch (%s): corner(5,5)=0x%08lX expected clear=0x%08lX",
                           label ? label : "?",
                           (unsigned long)corner,
                           (unsigned long)kClear);
    }

    sysmem->UnlockRect();
    return 0;
  };

  // ---------------------------------------------------------------------------
  // Path 1: SetVertexDeclaration(POSITION float3 @0, COLOR0 D3DCOLOR @12, TEX0 float2 @16)
  // ---------------------------------------------------------------------------
  const D3DVERTEXELEMENT9 elems[] = {
      {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {0, 12, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
      {0, 16, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> decl;
  hr = dev->CreateVertexDeclaration(elems, decl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::CreateVertexDeclaration", hr);
  }
  hr = dev->SetVertexDeclaration(decl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetVertexDeclaration", hr);
  }
  int rc = DrawAndValidateCenterPixel("vertex_decl", L"d3d9_fixedfunc_textured_wvp_vdecl.bmp");
  if (rc != 0) {
    return rc;
  }

  // ---------------------------------------------------------------------------
  // Path 2: SetFVF(XYZ|DIFFUSE|TEX1)
  // ---------------------------------------------------------------------------
  hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }
  rc = DrawAndValidateCenterPixel("fvf", L"d3d9_fixedfunc_textured_wvp_fvf.bmp");
  if (rc != 0) {
    return rc;
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9FixedFuncTexturedWvp(argc, argv);
  Sleep(30);
  return rc;
}
