#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

#include <cstring>

using aerogpu_test::ComPtr;

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

static bool MatrixEqual(const D3DMATRIX& a, const D3DMATRIX& b) {
  return std::memcmp(&a, &b, sizeof(D3DMATRIX)) == 0;
}

static int RunD3D9GetStateRoundtrip(int argc, char** argv) {
  const char* kTestName = "d3d9_get_state_roundtrip";
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
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9GetStateRoundtrip",
                                              L"AeroGPU D3D9 Get* State Roundtrip",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr) || !d3d) {
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr) || !dev) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  // Basic adapter sanity check to avoid false PASS when AeroGPU isn't active.
  {
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
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  // ---------------------------------------------------------------------------
  // Already-implemented getters: RenderState / SamplerState / Viewport.
  // ---------------------------------------------------------------------------
  {
    const DWORD z_enable = TRUE;
    hr = dev->SetRenderState(D3DRS_ZENABLE, z_enable);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_ZENABLE)", hr);
    }
    DWORD got = 0;
    hr = dev->GetRenderState(D3DRS_ZENABLE, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderState(D3DRS_ZENABLE)", hr);
    }
    if (got != z_enable) {
      return reporter.Fail("GetRenderState(D3DRS_ZENABLE) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)z_enable);
    }

    const DWORD cull_mode = (DWORD)D3DCULL_CW;
    hr = dev->SetRenderState(D3DRS_CULLMODE, cull_mode);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_CULLMODE)", hr);
    }
    got = 0;
    hr = dev->GetRenderState(D3DRS_CULLMODE, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderState(D3DRS_CULLMODE)", hr);
    }
    if (got != cull_mode) {
      return reporter.Fail("GetRenderState(D3DRS_CULLMODE) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)cull_mode);
    }
  }

  {
    const DWORD addr_u = (DWORD)D3DTADDRESS_CLAMP;
    hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, addr_u);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(D3DSAMP_ADDRESSU)", hr);
    }
    DWORD got = 0;
    hr = dev->GetSamplerState(0, D3DSAMP_ADDRESSU, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetSamplerState(D3DSAMP_ADDRESSU)", hr);
    }
    if (got != addr_u) {
      return reporter.Fail("GetSamplerState(D3DSAMP_ADDRESSU) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)addr_u);
    }

    const DWORD min_filter = (DWORD)D3DTEXF_LINEAR;
    hr = dev->SetSamplerState(0, D3DSAMP_MINFILTER, min_filter);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(D3DSAMP_MINFILTER)", hr);
    }
    got = 0;
    hr = dev->GetSamplerState(0, D3DSAMP_MINFILTER, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetSamplerState(D3DSAMP_MINFILTER)", hr);
    }
    if (got != min_filter) {
      return reporter.Fail("GetSamplerState(D3DSAMP_MINFILTER) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)min_filter);
    }
  }

  {
    D3DVIEWPORT9 vp;
    ZeroMemory(&vp, sizeof(vp));
    vp.X = 10;
    vp.Y = 20;
    vp.Width = 128;
    vp.Height = 64;
    vp.MinZ = 0.25f;
    vp.MaxZ = 0.75f;
    hr = dev->SetViewport(&vp);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport", hr);
    }

    D3DVIEWPORT9 got;
    ZeroMemory(&got, sizeof(got));
    hr = dev->GetViewport(&got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetViewport", hr);
    }

    if (got.X != vp.X || got.Y != vp.Y || got.Width != vp.Width || got.Height != vp.Height ||
        got.MinZ != vp.MinZ || got.MaxZ != vp.MaxZ) {
      return reporter.Fail(
          "GetViewport mismatch: got={X=%lu Y=%lu W=%lu H=%lu MinZ=%.3f MaxZ=%.3f} expected={X=%lu Y=%lu W=%lu H=%lu MinZ=%.3f MaxZ=%.3f}",
          (unsigned long)got.X,
          (unsigned long)got.Y,
          (unsigned long)got.Width,
          (unsigned long)got.Height,
          (double)got.MinZ,
          (double)got.MaxZ,
          (unsigned long)vp.X,
          (unsigned long)vp.Y,
          (unsigned long)vp.Width,
          (unsigned long)vp.Height,
          (double)vp.MinZ,
          (double)vp.MaxZ);
    }
  }

  // ---------------------------------------------------------------------------
  // Fixed-function caching: Transform / TextureStageState.
  // ---------------------------------------------------------------------------
  D3DMATRIX world_a;
  ZeroMemory(&world_a, sizeof(world_a));
  world_a._11 = 1.0f;
  world_a._12 = 2.0f;
  world_a._13 = 3.0f;
  world_a._14 = 4.0f;
  world_a._21 = 5.0f;
  world_a._22 = 6.0f;
  world_a._23 = 7.0f;
  world_a._24 = 8.0f;
  world_a._31 = 9.0f;
  world_a._32 = 10.0f;
  world_a._33 = 11.0f;
  world_a._34 = 12.0f;
  world_a._41 = 13.0f;
  world_a._42 = 14.0f;
  world_a._43 = 15.0f;
  world_a._44 = 16.0f;

  hr = dev->SetTransform(D3DTS_WORLD, &world_a);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(D3DTS_WORLD)", hr);
  }

  D3DMATRIX world_got;
  ZeroMemory(&world_got, sizeof(world_got));
  hr = dev->GetTransform(D3DTS_WORLD, &world_got);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTransform(D3DTS_WORLD)", hr);
  }
  if (!MatrixEqual(world_a, world_got)) {
    return reporter.Fail("GetTransform(D3DTS_WORLD) mismatch");
  }

  {
    const DWORD colorop_a = (DWORD)D3DTOP_ADD;
    hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, colorop_a);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(D3DTSS_COLOROP)", hr);
    }
    got = 0;
    hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTextureStageState(D3DTSS_COLOROP)", hr);
    }
    if (got != colorop_a) {
      return reporter.Fail("GetTextureStageState(D3DTSS_COLOROP) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)colorop_a);
    }

    const DWORD colorarg1_a = (DWORD)D3DTA_DIFFUSE;
    hr = dev->SetTextureStageState(0, D3DTSS_COLORARG1, colorarg1_a);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(D3DTSS_COLORARG1)", hr);
    }
    got = 0;
    hr = dev->GetTextureStageState(0, D3DTSS_COLORARG1, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTextureStageState(D3DTSS_COLORARG1)", hr);
    }
    if (got != colorarg1_a) {
      return reporter.Fail("GetTextureStageState(D3DTSS_COLORARG1) mismatch: got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)colorarg1_a);
    }
  }

  // ---------------------------------------------------------------------------
  // StateBlock round-trip: record state, clobber, apply, validate.
  // ---------------------------------------------------------------------------
  ComPtr<IDirect3DStateBlock9> sb;
  hr = dev->BeginStateBlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginStateBlock", hr);
  }

  const DWORD z_enable_sb = (DWORD)D3DZB_FALSE;
  hr = dev->SetRenderState(D3DRS_ZENABLE, z_enable_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_ZENABLE) (stateblock)", hr);
  }

  const DWORD addr_u_sb = (DWORD)D3DTADDRESS_MIRROR;
  hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, addr_u_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(D3DSAMP_ADDRESSU) (stateblock)", hr);
  }

  D3DVIEWPORT9 vp_sb;
  ZeroMemory(&vp_sb, sizeof(vp_sb));
  vp_sb.X = 3;
  vp_sb.Y = 4;
  vp_sb.Width = 63;
  vp_sb.Height = 45;
  vp_sb.MinZ = 0.0f;
  vp_sb.MaxZ = 0.5f;
  hr = dev->SetViewport(&vp_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetViewport (stateblock)", hr);
  }

  const DWORD colorop_sb = (DWORD)D3DTOP_SUBTRACT;
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, colorop_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(D3DTSS_COLOROP) (stateblock)", hr);
  }

  D3DMATRIX world_sb = world_a;
  world_sb._11 = 111.0f;
  world_sb._22 = 222.0f;
  hr = dev->SetTransform(D3DTS_WORLD, &world_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(D3DTS_WORLD) (stateblock)", hr);
  }

  hr = dev->EndStateBlock(sb.put());
  if (FAILED(hr) || !sb) {
    return reporter.FailHresult("EndStateBlock", hr);
  }

  // Clobber state.
  {
    hr = dev->SetRenderState(D3DRS_ZENABLE, (DWORD)D3DZB_TRUE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_ZENABLE) (clobber)", hr);
    }

    hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, (DWORD)D3DTADDRESS_CLAMP);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(D3DSAMP_ADDRESSU) (clobber)", hr);
    }

    D3DVIEWPORT9 vp_clobber;
    ZeroMemory(&vp_clobber, sizeof(vp_clobber));
    vp_clobber.X = 9;
    vp_clobber.Y = 8;
    vp_clobber.Width = 7;
    vp_clobber.Height = 6;
    vp_clobber.MinZ = 0.25f;
    vp_clobber.MaxZ = 1.0f;
    hr = dev->SetViewport(&vp_clobber);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport (clobber)", hr);
    }

    D3DMATRIX world_b = world_a;
    world_b._11 = -1.0f;
    world_b._22 = -2.0f;
    hr = dev->SetTransform(D3DTS_WORLD, &world_b);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform(D3DTS_WORLD) (clobber)", hr);
    }
    hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, (DWORD)D3DTOP_MODULATE2X);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(D3DTSS_COLOROP) (clobber)", hr);
    }
  }

  hr = sb->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock::Apply", hr);
  }

  // Validate restored values.
  {
    DWORD got = 0;
    hr = dev->GetRenderState(D3DRS_ZENABLE, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderState(D3DRS_ZENABLE) (after Apply)", hr);
    }
    if (got != z_enable_sb) {
      return reporter.Fail("stateblock restore mismatch: ZENABLE got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)z_enable_sb);
    }

    got = 0;
    hr = dev->GetSamplerState(0, D3DSAMP_ADDRESSU, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetSamplerState(D3DSAMP_ADDRESSU) (after Apply)", hr);
    }
    if (got != addr_u_sb) {
      return reporter.Fail("stateblock restore mismatch: ADDRESSU got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)addr_u_sb);
    }

    D3DVIEWPORT9 got_vp;
    ZeroMemory(&got_vp, sizeof(got_vp));
    hr = dev->GetViewport(&got_vp);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetViewport (after Apply)", hr);
    }
    if (got_vp.X != vp_sb.X || got_vp.Y != vp_sb.Y ||
        got_vp.Width != vp_sb.Width || got_vp.Height != vp_sb.Height ||
        got_vp.MinZ != vp_sb.MinZ || got_vp.MaxZ != vp_sb.MaxZ) {
      return reporter.Fail(
          "stateblock restore mismatch: Viewport got={X=%lu Y=%lu W=%lu H=%lu MinZ=%.3f MaxZ=%.3f} "
          "expected={X=%lu Y=%lu W=%lu H=%lu MinZ=%.3f MaxZ=%.3f}",
          (unsigned long)got_vp.X,
          (unsigned long)got_vp.Y,
          (unsigned long)got_vp.Width,
          (unsigned long)got_vp.Height,
          (double)got_vp.MinZ,
          (double)got_vp.MaxZ,
          (unsigned long)vp_sb.X,
          (unsigned long)vp_sb.Y,
          (unsigned long)vp_sb.Width,
          (unsigned long)vp_sb.Height,
          (double)vp_sb.MinZ,
          (double)vp_sb.MaxZ);
    }

    got = 0;
    hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTextureStageState(D3DTSS_COLOROP) (after Apply)", hr);
    }
    if (got != colorop_sb) {
      return reporter.Fail("stateblock restore mismatch: COLOROP got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)colorop_sb);
    }

    D3DMATRIX got_world;
    ZeroMemory(&got_world, sizeof(got_world));
    hr = dev->GetTransform(D3DTS_WORLD, &got_world);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTransform(D3DTS_WORLD) (after Apply)", hr);
    }
    if (!MatrixEqual(got_world, world_sb)) {
      return reporter.Fail("stateblock restore mismatch: WORLD matrix mismatch");
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9GetStateRoundtrip(argc, argv);
}
