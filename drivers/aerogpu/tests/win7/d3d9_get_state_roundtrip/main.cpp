#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

#include <cstring>

using aerogpu_test::ComPtr;

static bool NearlyEqual(float a, float b, float eps) {
  float d = a - b;
  if (d < 0.0f) {
    d = -d;
  }
  return d <= eps;
}

static bool MaterialNearlyEqual(const D3DMATERIAL9& a, const D3DMATERIAL9& b, float eps) {
  return NearlyEqual(a.Diffuse.r, b.Diffuse.r, eps) &&
         NearlyEqual(a.Diffuse.g, b.Diffuse.g, eps) &&
         NearlyEqual(a.Diffuse.b, b.Diffuse.b, eps) &&
         NearlyEqual(a.Diffuse.a, b.Diffuse.a, eps) &&
         NearlyEqual(a.Ambient.r, b.Ambient.r, eps) &&
         NearlyEqual(a.Ambient.g, b.Ambient.g, eps) &&
         NearlyEqual(a.Ambient.b, b.Ambient.b, eps) &&
         NearlyEqual(a.Ambient.a, b.Ambient.a, eps) &&
         NearlyEqual(a.Specular.r, b.Specular.r, eps) &&
         NearlyEqual(a.Specular.g, b.Specular.g, eps) &&
         NearlyEqual(a.Specular.b, b.Specular.b, eps) &&
         NearlyEqual(a.Specular.a, b.Specular.a, eps) &&
         NearlyEqual(a.Emissive.r, b.Emissive.r, eps) &&
         NearlyEqual(a.Emissive.g, b.Emissive.g, eps) &&
         NearlyEqual(a.Emissive.b, b.Emissive.b, eps) &&
         NearlyEqual(a.Emissive.a, b.Emissive.a, eps) &&
         NearlyEqual(a.Power, b.Power, eps);
}

static bool LightNearlyEqual(const D3DLIGHT9& a, const D3DLIGHT9& b, float eps) {
  return (a.Type == b.Type) &&
         NearlyEqual(a.Diffuse.r, b.Diffuse.r, eps) &&
         NearlyEqual(a.Diffuse.g, b.Diffuse.g, eps) &&
         NearlyEqual(a.Diffuse.b, b.Diffuse.b, eps) &&
         NearlyEqual(a.Diffuse.a, b.Diffuse.a, eps) &&
         NearlyEqual(a.Position.x, b.Position.x, eps) &&
         NearlyEqual(a.Position.y, b.Position.y, eps) &&
         NearlyEqual(a.Position.z, b.Position.z, eps) &&
         NearlyEqual(a.Range, b.Range, eps) &&
         NearlyEqual(a.Attenuation0, b.Attenuation0, eps) &&
         NearlyEqual(a.Attenuation1, b.Attenuation1, eps) &&
         NearlyEqual(a.Attenuation2, b.Attenuation2, eps);
}

static bool ClipStatusEqual(const D3DCLIPSTATUS9& a, const D3DCLIPSTATUS9& b) {
  return (a.ClipUnion == b.ClipUnion) &&
         (a.ClipIntersection == b.ClipIntersection) &&
         (a.Extents.x1 == b.Extents.x1) &&
         (a.Extents.y1 == b.Extents.y1) &&
         (a.Extents.x2 == b.Extents.x2) &&
         (a.Extents.y2 == b.Extents.y2);
}

static bool GammaRampEqual(const D3DGAMMARAMP& a, const D3DGAMMARAMP& b) {
  return std::memcmp(&a, &b, sizeof(D3DGAMMARAMP)) == 0;
}

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

static bool MatrixNearlyEqual(const D3DMATRIX& a, const D3DMATRIX& b, float eps) {
  const float* fa = reinterpret_cast<const float*>(&a);
  const float* fb = reinterpret_cast<const float*>(&b);
  for (int i = 0; i < 16; ++i) {
    if (!NearlyEqual(fa[i], fb[i], eps)) {
      return false;
    }
  }
  return true;
}

static D3DMATRIX MulMat4RowMajor(const D3DMATRIX& a, const D3DMATRIX& b) {
  const float* af = reinterpret_cast<const float*>(&a);
  const float* bf = reinterpret_cast<const float*>(&b);
  D3DMATRIX out;
  float* of = reinterpret_cast<float*>(&out);
  for (int r = 0; r < 4; ++r) {
    for (int c = 0; c < 4; ++c) {
      of[r * 4 + c] =
          af[r * 4 + 0] * bf[0 * 4 + c] +
          af[r * 4 + 1] * bf[1 * 4 + c] +
          af[r * 4 + 2] * bf[2 * 4 + c] +
          af[r * 4 + 3] * bf[3 * 4 + c];
    }
  }
  return out;
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

  D3DCAPS9 caps;
  ZeroMemory(&caps, sizeof(caps));
  hr = dev->GetDeviceCaps(&caps);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetDeviceCaps", hr);
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

  // MultiplyTransform round-trip: ensure the computed matrix is observable via GetTransform.
  {
    D3DMATRIX base;
    ZeroMemory(&base, sizeof(base));
    base._11 = 2.0f;
    base._22 = 3.0f;
    base._33 = 4.0f;
    base._44 = 1.0f;
    base._41 = 10.0f;
    base._42 = 20.0f;
    base._43 = 30.0f;

    hr = dev->SetTransform(D3DTS_WORLD, &base);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform(D3DTS_WORLD) (MultiplyTransform base)", hr);
    }

    D3DMATRIX mul;
    ZeroMemory(&mul, sizeof(mul));
    mul._11 = 1.0f;
    mul._22 = 1.0f;
    mul._33 = 1.0f;
    mul._44 = 1.0f;
    mul._41 = 1.5f;
    mul._42 = -2.5f;
    mul._43 = 0.25f;

    hr = dev->MultiplyTransform(D3DTS_WORLD, &mul);
    if (FAILED(hr)) {
      return reporter.FailHresult("MultiplyTransform(D3DTS_WORLD)", hr);
    }

    D3DMATRIX got;
    ZeroMemory(&got, sizeof(got));
    hr = dev->GetTransform(D3DTS_WORLD, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTransform(D3DTS_WORLD) (after MultiplyTransform)", hr);
    }

    D3DMATRIX expected = MulMat4RowMajor(base, mul);
    if (!MatrixNearlyEqual(got, expected, 1e-6f)) {
      return reporter.Fail("MultiplyTransform/GetTransform mismatch");
    }
  }

  // Clip plane round-trip (fixed-function cached state). Some D3D9 runtimes
  // reject SetClipPlane up-front when MaxUserClipPlanes is 0; treat that as a
  // supported skip rather than a failure.
  {
    const float plane_set[4] = {1.25f, -2.5f, 3.75f, -4.0f};
    hr = dev->SetClipPlane(0, plane_set);
    if (FAILED(hr)) {
      if (hr == D3DERR_INVALIDCALL && caps.MaxUserClipPlanes == 0) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetClipPlane (MaxUserClipPlanes=%lu hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)caps.MaxUserClipPlanes,
                                   (unsigned long)hr);
      } else {
        return reporter.FailHresult("SetClipPlane(0)", hr);
      }
    } else {
      float plane_got[4] = {};
      hr = dev->GetClipPlane(0, plane_got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetClipPlane(0)", hr);
      }
      for (int i = 0; i < 4; ++i) {
        if (!NearlyEqual(plane_got[i], plane_set[i], 1e-6f)) {
          return reporter.Fail("GetClipPlane mismatch at element %d: got=%f expected=%f",
                               i,
                               (double)plane_got[i],
                               (double)plane_set[i]);
        }
      }
    }
  }

  // Clip status round-trip (cached state). Some runtimes may reject this legacy
  // fixed-function path; treat D3DERR_INVALIDCALL as a supported skip.
  {
    D3DCLIPSTATUS9 set_cs;
    ZeroMemory(&set_cs, sizeof(set_cs));
    set_cs.ClipUnion = 0x3;
    set_cs.ClipIntersection = 0x1;
    set_cs.Extents.x1 = 1;
    set_cs.Extents.y1 = 2;
    set_cs.Extents.x2 = 3;
    set_cs.Extents.y2 = 4;

    hr = dev->SetClipStatus(&set_cs);
    if (FAILED(hr)) {
      if (hr == D3DERR_INVALIDCALL) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetClipStatus (Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
      } else {
        return reporter.FailHresult("SetClipStatus", hr);
      }
    } else {
      D3DCLIPSTATUS9 got_cs;
      ZeroMemory(&got_cs, sizeof(got_cs));
      hr = dev->GetClipStatus(&got_cs);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetClipStatus", hr);
      }
      if (!ClipStatusEqual(got_cs, set_cs)) {
        return reporter.Fail("GetClipStatus mismatch: got={Union=0x%08lX Inter=0x%08lX Ext={%ld,%ld,%ld,%ld}} "
                             "expected={Union=0x%08lX Inter=0x%08lX Ext={%ld,%ld,%ld,%ld}}",
                             (unsigned long)got_cs.ClipUnion,
                             (unsigned long)got_cs.ClipIntersection,
                             (long)got_cs.Extents.x1,
                             (long)got_cs.Extents.y1,
                             (long)got_cs.Extents.x2,
                             (long)got_cs.Extents.y2,
                             (unsigned long)set_cs.ClipUnion,
                             (unsigned long)set_cs.ClipIntersection,
                             (long)set_cs.Extents.x1,
                             (long)set_cs.Extents.y1,
                             (long)set_cs.Extents.x2,
                             (long)set_cs.Extents.y2);
      }
    }
  }

  // StreamSourceFreq (cached state).
  {
    const UINT kStream = 0;
    const UINT freq_set = 7;
    hr = dev->SetStreamSourceFreq(kStream, freq_set);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetStreamSourceFreq (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      UINT got = 0;
      hr = dev->GetStreamSourceFreq(kStream, &got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetStreamSourceFreq(0)", hr);
      }
      if (got != freq_set) {
        return reporter.Fail("GetStreamSourceFreq mismatch: got=%u expected=%u",
                             (unsigned)got,
                             (unsigned)freq_set);
      }
    }
  }

  // Software vertex processing (cached state).
  {
    hr = dev->SetSoftwareVertexProcessing(TRUE);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetSoftwareVertexProcessing (Set TRUE failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      const BOOL got = dev->GetSoftwareVertexProcessing();
      if (!got) {
        return reporter.Fail("GetSoftwareVertexProcessing mismatch: expected TRUE");
      }
    }
  }

  // N-Patch mode (cached state).
  {
    const float kMode = 3.0f;
    hr = dev->SetNPatchMode(kMode);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetNPatchMode (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      const float got = dev->GetNPatchMode();
      if (!NearlyEqual(got, kMode, 1e-6f)) {
        return reporter.Fail("GetNPatchMode mismatch: got=%f expected=%f",
                             (double)got,
                             (double)kMode);
      }
    }
  }

  // Shader constant int/bool caching.
  {
    int vals_i[8] = {10, 11, 12, 13, 20, 21, 22, 23}; // 2 x int4
    hr = dev->SetVertexShaderConstantI(7, vals_i, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexShaderConstantI", hr);
    }
    int got_i[8] = {};
    hr = dev->GetVertexShaderConstantI(7, got_i, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetVertexShaderConstantI", hr);
    }
    if (std::memcmp(got_i, vals_i, sizeof(vals_i)) != 0) {
      return reporter.Fail("GetVertexShaderConstantI mismatch");
    }

    BOOL vals_b[4] = {TRUE, FALSE, TRUE, FALSE};
    hr = dev->SetPixelShaderConstantB(3, vals_b, 4);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPixelShaderConstantB", hr);
    }
    BOOL got_b[4] = {};
    hr = dev->GetPixelShaderConstantB(3, got_b, 4);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPixelShaderConstantB", hr);
    }
    for (int i = 0; i < 4; ++i) {
      const BOOL a = vals_b[i] ? TRUE : FALSE;
      const BOOL b = got_b[i] ? TRUE : FALSE;
      if (a != b) {
        return reporter.Fail("GetPixelShaderConstantB mismatch at %d: got=%d expected=%d",
                             i,
                             (int)b,
                             (int)a);
      }
    }
  }

  // Fixed-function material caching.
  {
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
    mat.Specular.b = 0.125f;
    mat.Specular.a = 1.0f;
    mat.Emissive.r = 0.0f;
    mat.Emissive.g = 0.01f;
    mat.Emissive.b = 0.02f;
    mat.Emissive.a = 0.03f;
    mat.Power = 16.0f;

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
    if (!MaterialNearlyEqual(got_mat, mat, 1e-6f)) {
      return reporter.Fail("GetMaterial mismatch");
    }
  }

  // Fixed-function lighting caching. Some D3D9 runtimes reject SetLight up-front
  // when MaxActiveLights is 0; treat that as a supported skip rather than a
  // failure.
  {
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

    hr = dev->SetLight(0, &light);
    if (FAILED(hr)) {
      if (hr == D3DERR_INVALIDCALL && caps.MaxActiveLights == 0) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetLight (MaxActiveLights=%lu hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)caps.MaxActiveLights,
                                   (unsigned long)hr);
        // Skip LightEnable as well since it is tied to light slots.
        goto skip_light_enable;
      }
      return reporter.FailHresult("SetLight(0)", hr);
    }
    D3DLIGHT9 got_light;
    ZeroMemory(&got_light, sizeof(got_light));
    hr = dev->GetLight(0, &got_light);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetLight(0)", hr);
    }
    if (!LightNearlyEqual(got_light, light, 1e-6f)) {
      return reporter.Fail("GetLight mismatch");
    }

    hr = dev->LightEnable(0, TRUE);
    if (FAILED(hr)) {
      return reporter.FailHresult("LightEnable(0, TRUE)", hr);
    }
    BOOL enabled = FALSE;
    hr = dev->GetLightEnable(0, &enabled);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetLightEnable(0)", hr);
    }
    if (!enabled) {
      return reporter.Fail("GetLightEnable mismatch: expected enabled");
    }
  skip_light_enable:
    ;
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

  // Palette entries + current texture palette (cached state). Some runtimes may
  // reject palette state when palettized textures are unsupported; treat
  // D3DERR_INVALIDCALL as a supported skip.
  {
    const UINT kPalette = 3;
    PALETTEENTRY entries_set[256];
    for (int i = 0; i < 256; ++i) {
      entries_set[i].peRed = static_cast<BYTE>(i);
      entries_set[i].peGreen = static_cast<BYTE>(255 - i);
      entries_set[i].peBlue = static_cast<BYTE>(i ^ 0x55);
      entries_set[i].peFlags = 0;
    }

    hr = dev->SetPaletteEntries(kPalette, entries_set);
    if (FAILED(hr)) {
      if (hr == D3DERR_INVALIDCALL) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetPaletteEntries (Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
      } else {
        return reporter.FailHresult("SetPaletteEntries", hr);
      }
    } else {
      PALETTEENTRY entries_got[256];
      std::memset(entries_got, 0, sizeof(entries_got));
      hr = dev->GetPaletteEntries(kPalette, entries_got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetPaletteEntries", hr);
      }
      if (std::memcmp(entries_got, entries_set, sizeof(entries_set)) != 0) {
        return reporter.Fail("GetPaletteEntries mismatch");
      }
    }

    hr = dev->SetCurrentTexturePalette(kPalette);
    if (FAILED(hr)) {
      if (hr == D3DERR_INVALIDCALL) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetCurrentTexturePalette (Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
      } else {
        return reporter.FailHresult("SetCurrentTexturePalette", hr);
      }
    } else {
      UINT got = 0;
      hr = dev->GetCurrentTexturePalette(&got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetCurrentTexturePalette", hr);
      }
      if (got != kPalette) {
        return reporter.Fail("GetCurrentTexturePalette mismatch: got=%u expected=%u", (unsigned)got, (unsigned)kPalette);
      }
    }
  }

  // Gamma ramp round-trip (cached state). In windowed mode, some runtimes ignore
  // SetGammaRamp; treat mismatch as a supported skip.
  {
    D3DGAMMARAMP ramp_set;
    ZeroMemory(&ramp_set, sizeof(ramp_set));
    for (int i = 0; i < 256; ++i) {
      const WORD v = static_cast<WORD>(i * 257u);
      ramp_set.red[i] = v;
      ramp_set.green[i] = static_cast<WORD>((255 - i) * 257u);
      ramp_set.blue[i] = v;
    }
    dev->SetGammaRamp(0, 0, &ramp_set);
    D3DGAMMARAMP ramp_got;
    ZeroMemory(&ramp_got, sizeof(ramp_got));
    dev->GetGammaRamp(0, &ramp_got);
    if (!GammaRampEqual(ramp_got, ramp_set)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetGammaRamp (mismatch; runtime may ignore gamma ramp in windowed mode)",
                                 kTestName);
    }
  }

  // ---------------------------------------------------------------------------
  // StateBlock round-trip: record state, clobber, apply, validate.
  // ---------------------------------------------------------------------------
  const int vs_i_sb[4] = {101, 102, 103, 104};
  const BOOL ps_b_sb[2] = {TRUE, FALSE};
  const UINT stream_freq_sb = 13;
  const UINT stream_freq_clobber = 1;
  bool stream_freq_test = false;

  D3DMATERIAL9 mat_sb;
  ZeroMemory(&mat_sb, sizeof(mat_sb));
  mat_sb.Diffuse.r = 0.11f;
  mat_sb.Diffuse.g = 0.22f;
  mat_sb.Diffuse.b = 0.33f;
  mat_sb.Diffuse.a = 0.44f;
  mat_sb.Power = 8.0f;
  D3DMATERIAL9 mat_clobber = mat_sb;
  mat_clobber.Diffuse.r = 0.9f;
  mat_clobber.Diffuse.g = 0.8f;
  mat_clobber.Diffuse.b = 0.7f;
  mat_clobber.Diffuse.a = 0.6f;
  mat_clobber.Power = 32.0f;

  float clip_plane_sb[4] = {0.25f, -0.5f, 0.75f, -1.0f};
  float clip_plane_clobber[4] = {-1.0f, 2.0f, -3.0f, 4.0f};
  bool clip_plane_test = false;

  D3DLIGHT9 light_sb;
  ZeroMemory(&light_sb, sizeof(light_sb));
  light_sb.Type = D3DLIGHT_POINT;
  light_sb.Diffuse.r = 0.1f;
  light_sb.Diffuse.g = 0.2f;
  light_sb.Diffuse.b = 0.3f;
  light_sb.Diffuse.a = 1.0f;
  light_sb.Position.x = 1.0f;
  light_sb.Position.y = 2.0f;
  light_sb.Position.z = 3.0f;
  light_sb.Range = 10.0f;
  light_sb.Attenuation0 = 1.0f;
  D3DLIGHT9 light_clobber = light_sb;
  light_clobber.Diffuse.r = 0.9f;
  light_clobber.Diffuse.g = 0.8f;
  light_clobber.Diffuse.b = 0.7f;
  light_clobber.Position.x = -1.0f;
  light_clobber.Position.y = -2.0f;
  light_clobber.Position.z = -3.0f;
  bool light_test = false;

  const BOOL swvp_sb = TRUE;
  const BOOL swvp_clobber = FALSE;
  bool swvp_test = false;

  const float npatch_sb = 4.0f;
  const float npatch_clobber = 0.0f;
  bool npatch_test = false;

  D3DCLIPSTATUS9 clip_status_sb;
  ZeroMemory(&clip_status_sb, sizeof(clip_status_sb));
  clip_status_sb.ClipUnion = 0x9;
  clip_status_sb.ClipIntersection = 0x3;
  clip_status_sb.Extents.x1 = 5;
  clip_status_sb.Extents.y1 = 6;
  clip_status_sb.Extents.x2 = 7;
  clip_status_sb.Extents.y2 = 8;
  D3DCLIPSTATUS9 clip_status_clobber = clip_status_sb;
  clip_status_clobber.ClipUnion = 0x1;
  clip_status_clobber.Extents.x1 = -1;
  clip_status_clobber.Extents.y1 = -2;
  bool clip_status_test = false;

  const UINT palette_idx_sb = 7;
  const UINT palette_idx_clobber = 1;
  PALETTEENTRY palette_entries_sb[256];
  PALETTEENTRY palette_entries_clobber[256];
  for (int i = 0; i < 256; ++i) {
    palette_entries_sb[i].peRed = static_cast<BYTE>(i);
    palette_entries_sb[i].peGreen = static_cast<BYTE>(i ^ 0x3c);
    palette_entries_sb[i].peBlue = static_cast<BYTE>(255 - i);
    palette_entries_sb[i].peFlags = 0;

    palette_entries_clobber[i].peRed = static_cast<BYTE>(255 - i);
    palette_entries_clobber[i].peGreen = static_cast<BYTE>(i);
    palette_entries_clobber[i].peBlue = static_cast<BYTE>(i ^ 0xa5);
    palette_entries_clobber[i].peFlags = 0;
  }
  bool palette_test = false;
  bool current_palette_test = false;

  D3DGAMMARAMP gamma_ramp_sb;
  ZeroMemory(&gamma_ramp_sb, sizeof(gamma_ramp_sb));
  D3DGAMMARAMP gamma_ramp_clobber;
  ZeroMemory(&gamma_ramp_clobber, sizeof(gamma_ramp_clobber));
  for (int i = 0; i < 256; ++i) {
    gamma_ramp_sb.red[i] = static_cast<WORD>(i * 257u);
    gamma_ramp_sb.green[i] = static_cast<WORD>(i * 257u);
    gamma_ramp_sb.blue[i] = static_cast<WORD>(i * 257u);

    gamma_ramp_clobber.red[i] = static_cast<WORD>((255 - i) * 257u);
    gamma_ramp_clobber.green[i] = static_cast<WORD>((255 - i) * 257u);
    gamma_ramp_clobber.blue[i] = static_cast<WORD>((255 - i) * 257u);
  }
  bool gamma_ramp_test = false;

  // A handful of "binding" states that should participate in StateBlocks.
  RECT scissor_rect_sb;
  scissor_rect_sb.left = 5;
  scissor_rect_sb.top = 6;
  scissor_rect_sb.right = 50;
  scissor_rect_sb.bottom = 60;
  RECT scissor_rect_clobber;
  scissor_rect_clobber.left = 10;
  scissor_rect_clobber.top = 12;
  scissor_rect_clobber.right = 70;
  scissor_rect_clobber.bottom = 80;

  ComPtr<IDirect3DTexture9> tex_sb;
  hr = dev->CreateTexture(16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex_sb.put(), NULL);
  if (FAILED(hr) || !tex_sb) {
    return reporter.FailHresult("CreateTexture (stateblock tex_sb)", hr);
  }
  ComPtr<IDirect3DTexture9> tex_clobber;
  hr = dev->CreateTexture(16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex_clobber.put(), NULL);
  if (FAILED(hr) || !tex_clobber) {
    return reporter.FailHresult("CreateTexture (stateblock tex_clobber)", hr);
  }

  ComPtr<IDirect3DVertexBuffer9> vb_sb;
  hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb_sb.put(), NULL);
  if (FAILED(hr) || !vb_sb) {
    return reporter.FailHresult("CreateVertexBuffer (stateblock vb_sb)", hr);
  }
  ComPtr<IDirect3DVertexBuffer9> vb_clobber;
  hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb_clobber.put(), NULL);
  if (FAILED(hr) || !vb_clobber) {
    return reporter.FailHresult("CreateVertexBuffer (stateblock vb_clobber)", hr);
  }
  const UINT stream_offset_sb = 16;
  const UINT stream_stride_sb = 32;
  const UINT stream_offset_clobber = 0;
  const UINT stream_stride_clobber = 16;

  ComPtr<IDirect3DIndexBuffer9> ib_sb;
  hr = dev->CreateIndexBuffer(256, 0, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib_sb.put(), NULL);
  if (FAILED(hr) || !ib_sb) {
    return reporter.FailHresult("CreateIndexBuffer (stateblock ib_sb)", hr);
  }
  ComPtr<IDirect3DIndexBuffer9> ib_clobber;
  hr = dev->CreateIndexBuffer(256, 0, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib_clobber.put(), NULL);
  if (FAILED(hr) || !ib_clobber) {
    return reporter.FailHresult("CreateIndexBuffer (stateblock ib_clobber)", hr);
  }

  const DWORD fvf_sb = D3DFVF_XYZRHW | D3DFVF_DIFFUSE;
  const D3DVERTEXELEMENT9 decl_elems_clobber[] = {
      {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {0, 12, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> decl_clobber;
  hr = dev->CreateVertexDeclaration(decl_elems_clobber, decl_clobber.put());
  if (FAILED(hr) || !decl_clobber) {
    return reporter.FailHresult("CreateVertexDeclaration (stateblock decl_clobber)", hr);
  }

  ComPtr<IDirect3DSurface9> rt_sb;
  hr = dev->CreateRenderTarget(kWidth, kHeight, D3DFMT_X8R8G8B8, D3DMULTISAMPLE_NONE, 0, FALSE, rt_sb.put(), NULL);
  if (FAILED(hr) || !rt_sb) {
    return reporter.FailHresult("CreateRenderTarget (stateblock rt_sb)", hr);
  }
  ComPtr<IDirect3DSurface9> rt_clobber;
  hr = dev->CreateRenderTarget(kWidth, kHeight, D3DFMT_X8R8G8B8, D3DMULTISAMPLE_NONE, 0, FALSE, rt_clobber.put(), NULL);
  if (FAILED(hr) || !rt_clobber) {
    return reporter.FailHresult("CreateRenderTarget (stateblock rt_clobber)", hr);
  }

  ComPtr<IDirect3DSurface9> ds_sb;
  hr = dev->CreateDepthStencilSurface(kWidth,
                                      kHeight,
                                      D3DFMT_D24S8,
                                      D3DMULTISAMPLE_NONE,
                                      0,
                                      FALSE,
                                      ds_sb.put(),
                                      NULL);
  if (FAILED(hr) || !ds_sb) {
    return reporter.FailHresult("CreateDepthStencilSurface (stateblock ds_sb)", hr);
  }
  ComPtr<IDirect3DSurface9> ds_clobber;
  hr = dev->CreateDepthStencilSurface(kWidth,
                                      kHeight,
                                      D3DFMT_D24S8,
                                      D3DMULTISAMPLE_NONE,
                                      0,
                                      FALSE,
                                      ds_clobber.put(),
                                      NULL);
  if (FAILED(hr) || !ds_clobber) {
    return reporter.FailHresult("CreateDepthStencilSurface (stateblock ds_clobber)", hr);
  }

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

  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE) (stateblock)", hr);
  }
  hr = dev->SetScissorRect(&scissor_rect_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetScissorRect (stateblock)", hr);
  }

  hr = dev->SetTexture(0, tex_sb.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture(0) (stateblock)", hr);
  }

  hr = dev->SetRenderTarget(0, rt_sb.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderTarget(0) (stateblock)", hr);
  }

  hr = dev->SetDepthStencilSurface(ds_sb.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetDepthStencilSurface (stateblock)", hr);
  }

  hr = dev->SetStreamSource(0, vb_sb.get(), stream_offset_sb, stream_stride_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(0) (stateblock)", hr);
  }

  hr = dev->SetIndices(ib_sb.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetIndices (stateblock)", hr);
  }

  hr = dev->SetFVF(fvf_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetFVF (stateblock)", hr);
  }

  hr = dev->SetPaletteEntries(palette_idx_sb, palette_entries_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetPaletteEntries (Set in stateblock failed hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
  } else {
    palette_test = true;
  }

  hr = dev->SetCurrentTexturePalette(palette_idx_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: skipping StateBlock Set/GetCurrentTexturePalette (Set in stateblock failed hr=0x%08lX)",
        kTestName,
        (unsigned long)hr);
  } else {
    current_palette_test = true;
  }

  dev->SetGammaRamp(0, 0, &gamma_ramp_sb);

  D3DMATRIX world_sb = world_a;
  world_sb._11 = 111.0f;
  world_sb._22 = 222.0f;
  hr = dev->SetTransform(D3DTS_WORLD, &world_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(D3DTS_WORLD) (stateblock)", hr);
  }

  hr = dev->SetMaterial(&mat_sb);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetMaterial (stateblock)", hr);
  }

  hr = dev->SetClipPlane(0, clip_plane_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetClipPlane (Set in stateblock failed hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
  } else {
    clip_plane_test = true;
  }

  hr = dev->SetClipStatus(&clip_status_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetClipStatus (Set in stateblock failed hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
  } else {
    clip_status_test = true;
  }

  hr = dev->SetLight(0, &light_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetLight (Set in stateblock failed hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
  } else {
    hr = dev->LightEnable(0, TRUE);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock LightEnable (Enable in stateblock failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      light_test = true;
    }
  }

  hr = dev->SetSoftwareVertexProcessing(swvp_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: skipping StateBlock Set/GetSoftwareVertexProcessing (Set in stateblock failed hr=0x%08lX)",
        kTestName,
        (unsigned long)hr);
  } else {
    swvp_test = true;
  }

  hr = dev->SetNPatchMode(npatch_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetNPatchMode (Set in stateblock failed hr=0x%08lX)",
                               kTestName,
                               (unsigned long)hr);
  } else {
    npatch_test = true;
  }

  hr = dev->SetVertexShaderConstantI(10, vs_i_sb, 1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShaderConstantI (stateblock)", hr);
  }
  hr = dev->SetPixelShaderConstantB(7, ps_b_sb, 2);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShaderConstantB (stateblock)", hr);
  }

  hr = dev->SetStreamSourceFreq(0, stream_freq_sb);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: skipping StateBlock Set/GetStreamSourceFreq (Set in stateblock failed hr=0x%08lX)",
        kTestName,
        (unsigned long)hr);
  } else {
    stream_freq_test = true;
  }

  hr = dev->EndStateBlock(sb.put());
  if (FAILED(hr) || !sb) {
    return reporter.FailHresult("EndStateBlock", hr);
  }

  // Validate we can observe the gamma ramp in this runtime configuration; some
  // windowed runtimes ignore SetGammaRamp.
  {
    D3DGAMMARAMP got;
    ZeroMemory(&got, sizeof(got));
    dev->GetGammaRamp(0, &got);
    if (!GammaRampEqual(got, gamma_ramp_sb)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: skipping StateBlock Set/GetGammaRamp (runtime may ignore gamma ramp in windowed mode)",
          kTestName);
      gamma_ramp_test = false;
    } else {
      gamma_ramp_test = true;
    }
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

    hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE) (clobber)", hr);
    }
    hr = dev->SetScissorRect(&scissor_rect_clobber);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetScissorRect (clobber)", hr);
    }

    hr = dev->SetTexture(0, tex_clobber.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTexture(0) (clobber)", hr);
    }

    hr = dev->SetRenderTarget(0, rt_clobber.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderTarget(0) (clobber)", hr);
    }

    hr = dev->SetDepthStencilSurface(ds_clobber.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetDepthStencilSurface (clobber)", hr);
    }

    hr = dev->SetStreamSource(0, vb_clobber.get(), stream_offset_clobber, stream_stride_clobber);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(0) (clobber)", hr);
    }

    hr = dev->SetIndices(ib_clobber.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetIndices (clobber)", hr);
    }

    hr = dev->SetVertexDeclaration(decl_clobber.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexDeclaration (clobber)", hr);
    }

    if (palette_test) {
      hr = dev->SetPaletteEntries(palette_idx_sb, palette_entries_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetPaletteEntries (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        palette_test = false;
      }
    }

    if (current_palette_test) {
      hr = dev->SetCurrentTexturePalette(palette_idx_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping StateBlock Set/GetCurrentTexturePalette (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        current_palette_test = false;
      }
    }

    if (gamma_ramp_test) {
      dev->SetGammaRamp(0, 0, &gamma_ramp_clobber);
    }

    hr = dev->SetMaterial(&mat_clobber);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetMaterial (clobber)", hr);
    }

    if (clip_plane_test) {
      hr = dev->SetClipPlane(0, clip_plane_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetClipPlane (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        clip_plane_test = false;
      }
    }

    if (clip_status_test) {
      hr = dev->SetClipStatus(&clip_status_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetClipStatus (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        clip_status_test = false;
      }
    }

    if (light_test) {
      hr = dev->SetLight(0, &light_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetLight (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        light_test = false;
      } else {
        hr = dev->LightEnable(0, FALSE);
        if (FAILED(hr)) {
          aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock LightEnable (clobber disable failed hr=0x%08lX)",
                                     kTestName,
                                     (unsigned long)hr);
          light_test = false;
        }
      }
    }

    if (swvp_test) {
      hr = dev->SetSoftwareVertexProcessing(swvp_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping StateBlock Set/GetSoftwareVertexProcessing (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        swvp_test = false;
      }
    }

    if (npatch_test) {
      hr = dev->SetNPatchMode(npatch_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: skipping StateBlock Set/GetNPatchMode (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        npatch_test = false;
      }
    }

    const int vs_i_clobber[4] = {-1, -2, -3, -4};
    hr = dev->SetVertexShaderConstantI(10, vs_i_clobber, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexShaderConstantI (clobber)", hr);
    }
    const BOOL ps_b_clobber[2] = {FALSE, TRUE};
    hr = dev->SetPixelShaderConstantB(7, ps_b_clobber, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPixelShaderConstantB (clobber)", hr);
    }

    if (stream_freq_test) {
      hr = dev->SetStreamSourceFreq(0, stream_freq_clobber);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping StateBlock Set/GetStreamSourceFreq (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        stream_freq_test = false;
      }
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

    got = 0;
    hr = dev->GetRenderState(D3DRS_SCISSORTESTENABLE, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderState(D3DRS_SCISSORTESTENABLE) (after Apply)", hr);
    }
    if (got != TRUE) {
      return reporter.Fail("stateblock restore mismatch: SCISSORTESTENABLE got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)TRUE);
    }

    RECT got_scissor;
    ZeroMemory(&got_scissor, sizeof(got_scissor));
    hr = dev->GetScissorRect(&got_scissor);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetScissorRect (after Apply)", hr);
    }
    if (got_scissor.left != scissor_rect_sb.left || got_scissor.top != scissor_rect_sb.top ||
        got_scissor.right != scissor_rect_sb.right || got_scissor.bottom != scissor_rect_sb.bottom) {
      return reporter.Fail("stateblock restore mismatch: ScissorRect");
    }

    ComPtr<IDirect3DBaseTexture9> got_tex;
    hr = dev->GetTexture(0, got_tex.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTexture(0) (after Apply)", hr);
    }
    if (got_tex.get() != tex_sb.get()) {
      return reporter.Fail("stateblock restore mismatch: Texture(0) got=%p expected=%p", got_tex.get(), tex_sb.get());
    }

    ComPtr<IDirect3DSurface9> got_rt;
    hr = dev->GetRenderTarget(0, got_rt.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderTarget(0) (after Apply)", hr);
    }
    if (got_rt.get() != rt_sb.get()) {
      return reporter.Fail("stateblock restore mismatch: RenderTarget(0) got=%p expected=%p", got_rt.get(), rt_sb.get());
    }

    ComPtr<IDirect3DSurface9> got_ds;
    hr = dev->GetDepthStencilSurface(got_ds.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetDepthStencilSurface (after Apply)", hr);
    }
    if (got_ds.get() != ds_sb.get()) {
      return reporter.Fail("stateblock restore mismatch: DepthStencilSurface got=%p expected=%p", got_ds.get(), ds_sb.get());
    }

    ComPtr<IDirect3DVertexBuffer9> got_vb;
    UINT got_offset = 0;
    UINT got_stride = 0;
    hr = dev->GetStreamSource(0, got_vb.put(), &got_offset, &got_stride);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetStreamSource(0) (after Apply)", hr);
    }
    if (got_vb.get() != vb_sb.get() || got_offset != stream_offset_sb || got_stride != stream_stride_sb) {
      return reporter.Fail(
          "stateblock restore mismatch: StreamSource(0) got={vb=%p off=%u stride=%u} expected={vb=%p off=%u stride=%u}",
          got_vb.get(),
          (unsigned)got_offset,
          (unsigned)got_stride,
          vb_sb.get(),
          (unsigned)stream_offset_sb,
          (unsigned)stream_stride_sb);
    }

    ComPtr<IDirect3DIndexBuffer9> got_ib;
    hr = dev->GetIndices(got_ib.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetIndices (after Apply)", hr);
    }
    if (got_ib.get() != ib_sb.get()) {
      return reporter.Fail("stateblock restore mismatch: Indices got=%p expected=%p", got_ib.get(), ib_sb.get());
    }

    got = 0;
    hr = dev->GetFVF(&got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetFVF (after Apply)", hr);
    }
    if (got != fvf_sb) {
      return reporter.Fail("stateblock restore mismatch: FVF got=0x%08lX expected=0x%08lX",
                           (unsigned long)got,
                           (unsigned long)fvf_sb);
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

    D3DMATERIAL9 got_mat;
    ZeroMemory(&got_mat, sizeof(got_mat));
    hr = dev->GetMaterial(&got_mat);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetMaterial (after Apply)", hr);
    }
    if (!MaterialNearlyEqual(got_mat, mat_sb, 1e-6f)) {
      return reporter.Fail("stateblock restore mismatch: Material");
    }

    if (clip_plane_test) {
      float got_plane[4] = {};
      hr = dev->GetClipPlane(0, got_plane);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetClipPlane (after Apply)", hr);
      }
      for (int i = 0; i < 4; ++i) {
        if (!NearlyEqual(got_plane[i], clip_plane_sb[i], 1e-6f)) {
          return reporter.Fail("stateblock restore mismatch: ClipPlane[%d] got=%f expected=%f",
                               i,
                               (double)got_plane[i],
                               (double)clip_plane_sb[i]);
        }
      }
    }

    if (light_test) {
      D3DLIGHT9 got_light;
      ZeroMemory(&got_light, sizeof(got_light));
      hr = dev->GetLight(0, &got_light);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetLight (after Apply)", hr);
      }
      if (!LightNearlyEqual(got_light, light_sb, 1e-6f)) {
        return reporter.Fail("stateblock restore mismatch: Light");
      }
      BOOL got_enabled = FALSE;
      hr = dev->GetLightEnable(0, &got_enabled);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetLightEnable (after Apply)", hr);
      }
      if (!got_enabled) {
        return reporter.Fail("stateblock restore mismatch: LightEnable expected TRUE");
      }
    }

    if (swvp_test) {
      const BOOL got = dev->GetSoftwareVertexProcessing();
      if ((got ? TRUE : FALSE) != (swvp_sb ? TRUE : FALSE)) {
        return reporter.Fail("stateblock restore mismatch: SoftwareVertexProcessing got=%d expected=%d",
                             got ? 1 : 0,
                             swvp_sb ? 1 : 0);
      }
    }

    if (npatch_test) {
      const float got = dev->GetNPatchMode();
      if (!NearlyEqual(got, npatch_sb, 1e-6f)) {
        return reporter.Fail("stateblock restore mismatch: NPatchMode got=%f expected=%f",
                             (double)got,
                             (double)npatch_sb);
      }
    }

    int got_i[4] = {};
    hr = dev->GetVertexShaderConstantI(10, got_i, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetVertexShaderConstantI (after Apply)", hr);
    }
    if (std::memcmp(got_i, vs_i_sb, sizeof(vs_i_sb)) != 0) {
      return reporter.Fail("stateblock restore mismatch: VertexShaderConstantI");
    }

    BOOL got_b[2] = {};
    hr = dev->GetPixelShaderConstantB(7, got_b, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPixelShaderConstantB (after Apply)", hr);
    }
    for (int i = 0; i < 2; ++i) {
      const BOOL a = ps_b_sb[i] ? TRUE : FALSE;
      const BOOL b = got_b[i] ? TRUE : FALSE;
      if (a != b) {
        return reporter.Fail("stateblock restore mismatch: PixelShaderConstantB[%d] got=%d expected=%d",
                             i,
                             (int)b,
                             (int)a);
      }
    }

    if (stream_freq_test) {
      UINT got_freq = 0;
      hr = dev->GetStreamSourceFreq(0, &got_freq);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetStreamSourceFreq (after Apply)", hr);
      }
      if (got_freq != stream_freq_sb) {
        return reporter.Fail("stateblock restore mismatch: StreamSourceFreq got=%u expected=%u",
                             (unsigned)got_freq,
                             (unsigned)stream_freq_sb);
      }
    }

    if (clip_status_test) {
      D3DCLIPSTATUS9 got_cs;
      ZeroMemory(&got_cs, sizeof(got_cs));
      hr = dev->GetClipStatus(&got_cs);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetClipStatus (after Apply)", hr);
      }
      if (!ClipStatusEqual(got_cs, clip_status_sb)) {
        return reporter.Fail("stateblock restore mismatch: ClipStatus");
      }
    }

    if (palette_test) {
      PALETTEENTRY got_entries[256];
      std::memset(got_entries, 0, sizeof(got_entries));
      hr = dev->GetPaletteEntries(palette_idx_sb, got_entries);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetPaletteEntries (after Apply)", hr);
      }
      if (std::memcmp(got_entries, palette_entries_sb, sizeof(palette_entries_sb)) != 0) {
        return reporter.Fail("stateblock restore mismatch: PaletteEntries");
      }
    }

    if (current_palette_test) {
      UINT got = 0;
      hr = dev->GetCurrentTexturePalette(&got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetCurrentTexturePalette (after Apply)", hr);
      }
      if (got != palette_idx_sb) {
        return reporter.Fail("stateblock restore mismatch: CurrentTexturePalette got=%u expected=%u",
                             (unsigned)got,
                             (unsigned)palette_idx_sb);
      }
    }

    if (gamma_ramp_test) {
      D3DGAMMARAMP got;
      ZeroMemory(&got, sizeof(got));
      dev->GetGammaRamp(0, &got);
      if (!GammaRampEqual(got, gamma_ramp_sb)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping StateBlock GetGammaRamp validate (mismatch; runtime may ignore gamma ramp in windowed mode)",
            kTestName);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // CreateStateBlock + Capture round-trip: verify cached-only fixed-function state
  // is captured/applied via the Create/Capture/Apply path (not just Begin/End).
  // ---------------------------------------------------------------------------
  {
    ComPtr<IDirect3DStateBlock9> sb_vertex;

    // Create some simple binding resources so we can validate that
    // CreateStateBlock/Capture/Apply also round-trips VB/IB/FVF-style state.
    ComPtr<IDirect3DVertexBuffer9> vb_0;
    hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb_0.put(), NULL);
    if (FAILED(hr) || !vb_0) {
      return reporter.FailHresult("CreateVertexBuffer (CreateStateBlock vertex vb_0)", hr);
    }
    ComPtr<IDirect3DVertexBuffer9> vb_1;
    hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb_1.put(), NULL);
    if (FAILED(hr) || !vb_1) {
      return reporter.FailHresult("CreateVertexBuffer (CreateStateBlock vertex vb_1)", hr);
    }
    const UINT vb_offset_0 = 16;
    const UINT vb_stride_0 = 32;
    const UINT vb_offset_1 = 0;
    const UINT vb_stride_1 = 16;

    ComPtr<IDirect3DIndexBuffer9> ib_0;
    hr = dev->CreateIndexBuffer(256, 0, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib_0.put(), NULL);
    if (FAILED(hr) || !ib_0) {
      return reporter.FailHresult("CreateIndexBuffer (CreateStateBlock vertex ib_0)", hr);
    }
    ComPtr<IDirect3DIndexBuffer9> ib_1;
    hr = dev->CreateIndexBuffer(256, 0, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib_1.put(), NULL);
    if (FAILED(hr) || !ib_1) {
      return reporter.FailHresult("CreateIndexBuffer (CreateStateBlock vertex ib_1)", hr);
    }

    const DWORD fvf_0 = D3DFVF_XYZRHW | D3DFVF_DIFFUSE;
    const D3DVERTEXELEMENT9 decl_elems_1[] = {
        {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {0, 12, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
        D3DDECL_END(),
    };
    ComPtr<IDirect3DVertexDeclaration9> decl_1;
    hr = dev->CreateVertexDeclaration(decl_elems_1, decl_1.put());
    if (FAILED(hr) || !decl_1) {
      return reporter.FailHresult("CreateVertexDeclaration (CreateStateBlock vertex decl_1)", hr);
    }

    // Establish a baseline vertex-state config.
    D3DMATRIX world_0 = world_a;
    world_0._11 = 7.0f;
    world_0._22 = 8.0f;
    hr = dev->SetTransform(D3DTS_WORLD, &world_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform (pre CreateStateBlock)", hr);
    }

    D3DCLIPSTATUS9 clip_status_0;
    ZeroMemory(&clip_status_0, sizeof(clip_status_0));
    clip_status_0.ClipUnion = 0x11;
    clip_status_0.ClipIntersection = 0x22;
    clip_status_0.Extents.x1 = 10;
    clip_status_0.Extents.y1 = 20;
    clip_status_0.Extents.x2 = 30;
    clip_status_0.Extents.y2 = 40;
    D3DCLIPSTATUS9 clip_status_1 = clip_status_0;
    clip_status_1.ClipUnion = 0x33;
    clip_status_1.Extents.x1 = -5;
    clip_status_1.Extents.y1 = -6;
    bool clip_status_ok = false;
    hr = dev->SetClipStatus(&clip_status_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping CreateStateBlock ClipStatus (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      clip_status_ok = true;
    }

    hr = dev->SetStreamSource(0, vb_0.get(), vb_offset_0, vb_stride_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(0) (pre CreateStateBlock vertex)", hr);
    }

    hr = dev->SetIndices(ib_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetIndices (pre CreateStateBlock vertex)", hr);
    }

    hr = dev->SetFVF(fvf_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF (pre CreateStateBlock vertex)", hr);
    }

    const UINT freq_0 = 5;
    bool freq_ok = false;
    hr = dev->SetStreamSourceFreq(0, freq_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping CreateStateBlock StreamSourceFreq (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      freq_ok = true;
    }

    const BOOL swvp_0 = TRUE;
    bool swvp_ok = false;
    hr = dev->SetSoftwareVertexProcessing(swvp_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: skipping CreateStateBlock SoftwareVertexProcessing (Set failed hr=0x%08lX)",
          kTestName,
          (unsigned long)hr);
    } else {
      swvp_ok = true;
    }

    const float npatch_0 = 2.0f;
    bool npatch_ok = false;
    hr = dev->SetNPatchMode(npatch_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping CreateStateBlock NPatchMode (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      npatch_ok = true;
    }

    const int vsi_0[4] = {11, 22, 33, 44};
    hr = dev->SetVertexShaderConstantI(20, vsi_0, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexShaderConstantI (pre CreateStateBlock)", hr);
    }

    // Capture the baseline via CreateStateBlock.
    hr = dev->CreateStateBlock(D3DSBT_VERTEXSTATE, sb_vertex.put());
    if (FAILED(hr) || !sb_vertex) {
      return reporter.FailHresult("CreateStateBlock(D3DSBT_VERTEXSTATE)", hr);
    }

    // Verify CreateStateBlock captured the baseline (world_0/vsi_0/...) without
    // needing an explicit Capture() call.
    {
      D3DMATRIX world_clobber = world_0;
      world_clobber._11 = 999.0f;
      world_clobber._22 = 1000.0f;
      hr = dev->SetTransform(D3DTS_WORLD, &world_clobber);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetTransform (clobber pre Apply baseline)", hr);
      }

      const int vsi_clobber[4] = {0, 0, 0, 0};
      hr = dev->SetVertexShaderConstantI(20, vsi_clobber, 1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetVertexShaderConstantI (clobber pre Apply baseline)", hr);
      }

      if (freq_ok) {
        hr = dev->SetStreamSourceFreq(0, freq_0 + 1);
        if (FAILED(hr)) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: disabling CreateStateBlock StreamSourceFreq baseline check (clobber Set failed hr=0x%08lX)",
              kTestName,
              (unsigned long)hr);
          freq_ok = false;
        }
      }

      if (clip_status_ok) {
        hr = dev->SetClipStatus(&clip_status_1);
        if (FAILED(hr)) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: disabling CreateStateBlock ClipStatus baseline check (clobber Set failed hr=0x%08lX)",
              kTestName,
              (unsigned long)hr);
          clip_status_ok = false;
        }
      }

      hr = dev->SetStreamSource(0, vb_1.get(), vb_offset_1, vb_stride_1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetStreamSource(0) (clobber pre Apply vertex baseline)", hr);
      }

      hr = dev->SetIndices(ib_1.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("SetIndices (clobber pre Apply vertex baseline)", hr);
      }

      hr = dev->SetVertexDeclaration(decl_1.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("SetVertexDeclaration (clobber pre Apply vertex baseline)", hr);
      }

      hr = sb_vertex->Apply();
      if (FAILED(hr)) {
        return reporter.FailHresult("StateBlock::Apply (vertex baseline)", hr);
      }

      D3DMATRIX got_world;
      ZeroMemory(&got_world, sizeof(got_world));
      hr = dev->GetTransform(D3DTS_WORLD, &got_world);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetTransform (after Apply vertex baseline)", hr);
      }
      if (!MatrixEqual(got_world, world_0)) {
        return reporter.Fail("CreateStateBlock baseline mismatch: WORLD matrix");
      }

      int got_i[4] = {};
      hr = dev->GetVertexShaderConstantI(20, got_i, 1);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetVertexShaderConstantI (after Apply vertex baseline)", hr);
      }
      if (std::memcmp(got_i, vsi_0, sizeof(vsi_0)) != 0) {
        return reporter.Fail("CreateStateBlock baseline mismatch: VertexShaderConstantI");
      }

      ComPtr<IDirect3DVertexBuffer9> got_vb;
      UINT got_offset = 0;
      UINT got_stride = 0;
      hr = dev->GetStreamSource(0, got_vb.put(), &got_offset, &got_stride);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetStreamSource(0) (after Apply vertex baseline)", hr);
      }
      if (got_vb.get() != vb_0.get() || got_offset != vb_offset_0 || got_stride != vb_stride_0) {
        return reporter.Fail(
            "CreateStateBlock baseline mismatch: StreamSource(0) got={vb=%p off=%u stride=%u} expected={vb=%p off=%u stride=%u}",
            got_vb.get(),
            (unsigned)got_offset,
            (unsigned)got_stride,
            vb_0.get(),
            (unsigned)vb_offset_0,
            (unsigned)vb_stride_0);
      }

      ComPtr<IDirect3DIndexBuffer9> got_ib;
      hr = dev->GetIndices(got_ib.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("GetIndices (after Apply vertex baseline)", hr);
      }
      if (got_ib.get() != ib_0.get()) {
        return reporter.Fail("CreateStateBlock baseline mismatch: Indices got=%p expected=%p", got_ib.get(), ib_0.get());
      }

      DWORD got_fvf = 0;
      hr = dev->GetFVF(&got_fvf);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetFVF (after Apply vertex baseline)", hr);
      }
      if (got_fvf != fvf_0) {
        return reporter.Fail("CreateStateBlock baseline mismatch: FVF got=0x%08lX expected=0x%08lX",
                             (unsigned long)got_fvf,
                             (unsigned long)fvf_0);
      }

      if (freq_ok) {
        UINT got_freq = 0;
        hr = dev->GetStreamSourceFreq(0, &got_freq);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetStreamSourceFreq (after Apply vertex baseline)", hr);
        }
        if (got_freq != freq_0) {
          return reporter.Fail("CreateStateBlock baseline mismatch: StreamSourceFreq got=%u expected=%u",
                               (unsigned)got_freq,
                               (unsigned)freq_0);
        }
      }

      if (clip_status_ok) {
        D3DCLIPSTATUS9 got_cs;
        ZeroMemory(&got_cs, sizeof(got_cs));
        hr = dev->GetClipStatus(&got_cs);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetClipStatus (after Apply vertex baseline)", hr);
        }
        if (!ClipStatusEqual(got_cs, clip_status_0)) {
          return reporter.Fail("CreateStateBlock baseline mismatch: ClipStatus");
        }
      }
    }

    // Mutate state to a second configuration, then Capture() it.
    D3DMATRIX world_1 = world_0;
    world_1._11 = -1.0f;
    world_1._22 = -2.0f;
    hr = dev->SetTransform(D3DTS_WORLD, &world_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform (pre Capture)", hr);
    }

    if (clip_status_ok) {
      hr = dev->SetClipStatus(&clip_status_1);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: disabling CreateStateBlock ClipStatus check (Set pre Capture failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        clip_status_ok = false;
      }
    }

    const UINT freq_1 = 9;
    if (freq_ok) {
      hr = dev->SetStreamSourceFreq(0, freq_1);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: disabling CreateStateBlock StreamSourceFreq check (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        freq_ok = false;
      }
    }

    if (swvp_ok) {
      hr = dev->SetSoftwareVertexProcessing(FALSE);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock SoftwareVertexProcessing check (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        swvp_ok = false;
      }
    }

    const float npatch_1 = 0.0f;
    if (npatch_ok) {
      hr = dev->SetNPatchMode(npatch_1);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: disabling CreateStateBlock NPatchMode check (clobber Set failed hr=0x%08lX)",
                                   kTestName,
                                   (unsigned long)hr);
        npatch_ok = false;
      }
    }

    const int vsi_1[4] = {-1, -2, -3, -4};
    hr = dev->SetVertexShaderConstantI(20, vsi_1, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexShaderConstantI (pre Capture)", hr);
    }

    hr = dev->SetStreamSource(0, vb_1.get(), vb_offset_1, vb_stride_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(0) (pre Capture vertex)", hr);
    }

    hr = dev->SetIndices(ib_1.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetIndices (pre Capture vertex)", hr);
    }

    hr = dev->SetVertexDeclaration(decl_1.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexDeclaration (pre Capture vertex)", hr);
    }

    hr = sb_vertex->Capture();
    if (FAILED(hr)) {
      return reporter.FailHresult("StateBlock::Capture (vertex)", hr);
    }

    // Clobber again so Apply has visible effect.
    D3DMATRIX world_2 = world_0;
    world_2._11 = 123.0f;
    world_2._22 = 456.0f;
    hr = dev->SetTransform(D3DTS_WORLD, &world_2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform (pre Apply)", hr);
    }
    const int vsi_2[4] = {0, 0, 0, 0};
    hr = dev->SetVertexShaderConstantI(20, vsi_2, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetVertexShaderConstantI (pre Apply)", hr);
    }

    if (clip_status_ok) {
      hr = dev->SetClipStatus(&clip_status_0);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock ClipStatus check (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        clip_status_ok = false;
      }
    }

    hr = dev->SetStreamSource(0, vb_0.get(), vb_offset_0, vb_stride_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(0) (pre Apply vertex)", hr);
    }

    hr = dev->SetIndices(ib_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetIndices (pre Apply vertex)", hr);
    }

    hr = dev->SetFVF(fvf_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF (pre Apply vertex)", hr);
    }

    hr = sb_vertex->Apply();
    if (FAILED(hr)) {
      return reporter.FailHresult("StateBlock::Apply (vertex)", hr);
    }

    // Verify state restored to the captured (world_1 / vsi_1 / etc).
    D3DMATRIX got_world;
    ZeroMemory(&got_world, sizeof(got_world));
    hr = dev->GetTransform(D3DTS_WORLD, &got_world);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTransform (after Apply vertex)", hr);
    }
    if (!MatrixEqual(got_world, world_1)) {
      return reporter.Fail("CreateStateBlock restore mismatch: WORLD matrix");
    }

    int got_i[4] = {};
    hr = dev->GetVertexShaderConstantI(20, got_i, 1);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetVertexShaderConstantI (after Apply vertex)", hr);
    }
    if (std::memcmp(got_i, vsi_1, sizeof(vsi_1)) != 0) {
      return reporter.Fail("CreateStateBlock restore mismatch: VertexShaderConstantI");
    }

    ComPtr<IDirect3DVertexBuffer9> got_vb;
    UINT got_offset = 0;
    UINT got_stride = 0;
    hr = dev->GetStreamSource(0, got_vb.put(), &got_offset, &got_stride);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetStreamSource(0) (after Apply vertex)", hr);
    }
    if (got_vb.get() != vb_1.get() || got_offset != vb_offset_1 || got_stride != vb_stride_1) {
      return reporter.Fail(
          "CreateStateBlock restore mismatch: StreamSource(0) got={vb=%p off=%u stride=%u} expected={vb=%p off=%u stride=%u}",
          got_vb.get(),
          (unsigned)got_offset,
          (unsigned)got_stride,
          vb_1.get(),
          (unsigned)vb_offset_1,
          (unsigned)vb_stride_1);
    }

    ComPtr<IDirect3DIndexBuffer9> got_ib;
    hr = dev->GetIndices(got_ib.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetIndices (after Apply vertex)", hr);
    }
    if (got_ib.get() != ib_1.get()) {
      return reporter.Fail("CreateStateBlock restore mismatch: Indices got=%p expected=%p", got_ib.get(), ib_1.get());
    }

    DWORD got_fvf = 0xFFFFFFFFu;
    hr = dev->GetFVF(&got_fvf);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetFVF (after Apply vertex)", hr);
    }
    if (got_fvf != 0) {
      return reporter.Fail("CreateStateBlock restore mismatch: FVF got=0x%08lX expected 0",
                           (unsigned long)got_fvf);
    }

    ComPtr<IDirect3DVertexDeclaration9> got_decl;
    hr = dev->GetVertexDeclaration(got_decl.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetVertexDeclaration (after Apply vertex)", hr);
    }
    if (got_decl.get() != decl_1.get()) {
      return reporter.Fail("CreateStateBlock restore mismatch: VertexDeclaration got=%p expected=%p",
                           got_decl.get(),
                           decl_1.get());
    }

    if (freq_ok) {
      UINT got_freq = 0;
      hr = dev->GetStreamSourceFreq(0, &got_freq);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetStreamSourceFreq (after Apply vertex)", hr);
      }
      if (got_freq != freq_1) {
        return reporter.Fail("CreateStateBlock restore mismatch: StreamSourceFreq got=%u expected=%u",
                             (unsigned)got_freq,
                             (unsigned)freq_1);
      }
    }

    if (clip_status_ok) {
      D3DCLIPSTATUS9 got_cs;
      ZeroMemory(&got_cs, sizeof(got_cs));
      hr = dev->GetClipStatus(&got_cs);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetClipStatus (after Apply vertex)", hr);
      }
      if (!ClipStatusEqual(got_cs, clip_status_1)) {
        return reporter.Fail("CreateStateBlock restore mismatch: ClipStatus");
      }
    }

    if (swvp_ok) {
      const BOOL got = dev->GetSoftwareVertexProcessing();
      if (got) {
        return reporter.Fail("CreateStateBlock restore mismatch: SoftwareVertexProcessing expected FALSE");
      }
    }

    if (npatch_ok) {
      const float got = dev->GetNPatchMode();
      if (!NearlyEqual(got, npatch_1, 1e-6f)) {
        return reporter.Fail("CreateStateBlock restore mismatch: NPatchMode got=%f expected=%f",
                             (double)got,
                             (double)npatch_1);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // CreateStateBlock + Capture round-trip (pixel): verify pixel processing state
  // is captured/applied via Create/Capture/Apply.
  // ---------------------------------------------------------------------------
  {
    ComPtr<IDirect3DStateBlock9> sb_pixel;

    ComPtr<IDirect3DTexture9> tex_0;
    hr = dev->CreateTexture(16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex_0.put(), NULL);
    if (FAILED(hr) || !tex_0) {
      return reporter.FailHresult("CreateTexture (CreateStateBlock pixel tex_0)", hr);
    }
    ComPtr<IDirect3DTexture9> tex_1;
    hr = dev->CreateTexture(16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex_1.put(), NULL);
    if (FAILED(hr) || !tex_1) {
      return reporter.FailHresult("CreateTexture (CreateStateBlock pixel tex_1)", hr);
    }

    ComPtr<IDirect3DSurface9> rt_0;
    hr = dev->CreateRenderTarget(kWidth,
                                 kHeight,
                                 D3DFMT_X8R8G8B8,
                                 D3DMULTISAMPLE_NONE,
                                 0,
                                 FALSE,
                                 rt_0.put(),
                                 NULL);
    if (FAILED(hr) || !rt_0) {
      return reporter.FailHresult("CreateRenderTarget (CreateStateBlock pixel rt_0)", hr);
    }
    ComPtr<IDirect3DSurface9> rt_1;
    hr = dev->CreateRenderTarget(kWidth,
                                 kHeight,
                                 D3DFMT_X8R8G8B8,
                                 D3DMULTISAMPLE_NONE,
                                 0,
                                 FALSE,
                                 rt_1.put(),
                                 NULL);
    if (FAILED(hr) || !rt_1) {
      return reporter.FailHresult("CreateRenderTarget (CreateStateBlock pixel rt_1)", hr);
    }

    ComPtr<IDirect3DSurface9> ds_0;
    hr = dev->CreateDepthStencilSurface(kWidth,
                                        kHeight,
                                        D3DFMT_D24S8,
                                        D3DMULTISAMPLE_NONE,
                                        0,
                                        FALSE,
                                        ds_0.put(),
                                        NULL);
    if (FAILED(hr) || !ds_0) {
      return reporter.FailHresult("CreateDepthStencilSurface (CreateStateBlock pixel ds_0)", hr);
    }
    ComPtr<IDirect3DSurface9> ds_1;
    hr = dev->CreateDepthStencilSurface(kWidth,
                                        kHeight,
                                        D3DFMT_D24S8,
                                        D3DMULTISAMPLE_NONE,
                                        0,
                                        FALSE,
                                        ds_1.put(),
                                        NULL);
    if (FAILED(hr) || !ds_1) {
      return reporter.FailHresult("CreateDepthStencilSurface (CreateStateBlock pixel ds_1)", hr);
    }

    // Establish a baseline pixel-state config.
    const DWORD alphablend_0 = TRUE;
    hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, alphablend_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE) (pre CreateStateBlock pixel)", hr);
    }

    const DWORD samp_addr_u_0 = D3DTADDRESS_CLAMP;
    hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, samp_addr_u_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(0, ADDRESSU) (pre CreateStateBlock pixel)", hr);
    }

    const DWORD colorop_0 = D3DTOP_MODULATE;
    hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, colorop_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(0, COLOROP) (pre CreateStateBlock pixel)", hr);
    }

    const BOOL ps_b_0[2] = {TRUE, FALSE};
    hr = dev->SetPixelShaderConstantB(20, ps_b_0, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPixelShaderConstantB (pre CreateStateBlock pixel)", hr);
    }

    D3DVIEWPORT9 vp_0;
    ZeroMemory(&vp_0, sizeof(vp_0));
    vp_0.X = 10;
    vp_0.Y = 20;
    vp_0.Width = 64;
    vp_0.Height = 65;
    vp_0.MinZ = 0.125f;
    vp_0.MaxZ = 0.875f;
    hr = dev->SetViewport(&vp_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport (pre CreateStateBlock pixel)", hr);
    }

    RECT scissor_0;
    scissor_0.left = 3;
    scissor_0.top = 4;
    scissor_0.right = 50;
    scissor_0.bottom = 60;
    hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE) (pre CreateStateBlock pixel)", hr);
    }
    hr = dev->SetScissorRect(&scissor_0);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetScissorRect (pre CreateStateBlock pixel)", hr);
    }

    hr = dev->SetTexture(0, tex_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTexture(0) (pre CreateStateBlock pixel)", hr);
    }

    hr = dev->SetRenderTarget(0, rt_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderTarget(0) (pre CreateStateBlock pixel)", hr);
    }

    hr = dev->SetDepthStencilSurface(ds_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetDepthStencilSurface (pre CreateStateBlock pixel)", hr);
    }

    // Palette / gamma ramp are legacy cached-only state. Some runtimes may reject
    // palettes (when palettized textures are unsupported) and some windowed
    // runtimes may ignore gamma ramps.
    const UINT palette_idx_0 = 5;
    const UINT current_palette_0 = palette_idx_0;
    const UINT current_palette_1 = palette_idx_0 + 1;

    PALETTEENTRY palette_0[256];
    PALETTEENTRY palette_1[256];
    for (int i = 0; i < 256; ++i) {
      palette_0[i].peRed = static_cast<BYTE>(i);
      palette_0[i].peGreen = static_cast<BYTE>(255 - i);
      palette_0[i].peBlue = static_cast<BYTE>(i ^ 0x5a);
      palette_0[i].peFlags = 0;

      palette_1[i].peRed = static_cast<BYTE>(255 - i);
      palette_1[i].peGreen = static_cast<BYTE>(i);
      palette_1[i].peBlue = static_cast<BYTE>(i ^ 0xa5);
      palette_1[i].peFlags = 0;
    }

    bool palette_ok = false;
    hr = dev->SetPaletteEntries(palette_idx_0, palette_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping CreateStateBlock PaletteEntries (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      palette_ok = true;
    }

    bool current_palette_ok = false;
    hr = dev->SetCurrentTexturePalette(current_palette_0);
    if (FAILED(hr)) {
      aerogpu_test::PrintfStdout("INFO: %s: skipping CreateStateBlock CurrentTexturePalette (Set failed hr=0x%08lX)",
                                 kTestName,
                                 (unsigned long)hr);
    } else {
      current_palette_ok = true;
    }

    D3DGAMMARAMP gamma_0;
    ZeroMemory(&gamma_0, sizeof(gamma_0));
    D3DGAMMARAMP gamma_1;
    ZeroMemory(&gamma_1, sizeof(gamma_1));
    for (int i = 0; i < 256; ++i) {
      const WORD v0 = static_cast<WORD>(i * 257u);
      const WORD v1 = static_cast<WORD>((255 - i) * 257u);
      gamma_0.red[i] = v0;
      gamma_0.green[i] = v0;
      gamma_0.blue[i] = v0;
      gamma_1.red[i] = v1;
      gamma_1.green[i] = v1;
      gamma_1.blue[i] = v1;
    }
    bool gamma_ok = false;
    dev->SetGammaRamp(0, 0, &gamma_0);
    {
      D3DGAMMARAMP got;
      ZeroMemory(&got, sizeof(got));
      dev->GetGammaRamp(0, &got);
      if (!GammaRampEqual(got, gamma_0)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping CreateStateBlock GammaRamp (runtime may ignore gamma ramp in windowed mode)",
            kTestName);
        gamma_ok = false;
      } else {
        gamma_ok = true;
      }
    }

    // Capture the baseline via CreateStateBlock.
    hr = dev->CreateStateBlock(D3DSBT_PIXELSTATE, sb_pixel.put());
    if (FAILED(hr) || !sb_pixel) {
      return reporter.FailHresult("CreateStateBlock(D3DSBT_PIXELSTATE)", hr);
    }

    // Verify CreateStateBlock captured the baseline without needing Capture().
    {
      hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE) (clobber pre Apply pixel baseline)", hr);
      }
      hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetSamplerState(0, ADDRESSU) (clobber pre Apply pixel baseline)", hr);
      }
      hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_ADD);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetTextureStageState(0, COLOROP) (clobber pre Apply pixel baseline)", hr);
      }
      const BOOL ps_b_clobber[2] = {FALSE, TRUE};
      hr = dev->SetPixelShaderConstantB(20, ps_b_clobber, 2);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetPixelShaderConstantB (clobber pre Apply pixel baseline)", hr);
      }
      D3DVIEWPORT9 vp_clobber = vp_0;
      vp_clobber.X = 1;
      vp_clobber.Y = 2;
      vp_clobber.Width = 128;
      vp_clobber.Height = 129;
      hr = dev->SetViewport(&vp_clobber);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetViewport (clobber pre Apply pixel baseline)", hr);
      }
      RECT scissor_clobber;
      scissor_clobber.left = 7;
      scissor_clobber.top = 8;
      scissor_clobber.right = 70;
      scissor_clobber.bottom = 80;
      hr = dev->SetScissorRect(&scissor_clobber);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetScissorRect (clobber pre Apply pixel baseline)", hr);
      }

      hr = dev->SetTexture(0, tex_1.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("SetTexture(0) (clobber pre Apply pixel baseline)", hr);
      }

      hr = dev->SetRenderTarget(0, rt_1.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("SetRenderTarget(0) (clobber pre Apply pixel baseline)", hr);
      }

      hr = dev->SetDepthStencilSurface(ds_1.get());
      if (FAILED(hr)) {
        return reporter.FailHresult("SetDepthStencilSurface (clobber pre Apply pixel baseline)", hr);
      }

      if (palette_ok) {
        hr = dev->SetPaletteEntries(palette_idx_0, palette_1);
        if (FAILED(hr)) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: disabling CreateStateBlock PaletteEntries baseline check (clobber Set failed hr=0x%08lX)",
              kTestName,
              (unsigned long)hr);
          palette_ok = false;
        }
      }

      if (current_palette_ok) {
        hr = dev->SetCurrentTexturePalette(current_palette_1);
        if (FAILED(hr)) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: disabling CreateStateBlock CurrentTexturePalette baseline check (clobber Set failed hr=0x%08lX)",
              kTestName,
              (unsigned long)hr);
          current_palette_ok = false;
        }
      }

      if (gamma_ok) {
        dev->SetGammaRamp(0, 0, &gamma_1);
      }

      hr = sb_pixel->Apply();
      if (FAILED(hr)) {
        return reporter.FailHresult("StateBlock::Apply (pixel baseline)", hr);
      }

      DWORD got = 0;
      hr = dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetRenderState(D3DRS_ALPHABLENDENABLE) (after Apply pixel baseline)", hr);
      }
      if (got != alphablend_0) {
        return reporter.Fail("CreateStateBlock baseline mismatch: ALPHABLENDENABLE got=%lu expected=%lu",
                             (unsigned long)got,
                             (unsigned long)alphablend_0);
      }

      got = 0;
      hr = dev->GetSamplerState(0, D3DSAMP_ADDRESSU, &got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetSamplerState(0, ADDRESSU) (after Apply pixel baseline)", hr);
      }
      if (got != samp_addr_u_0) {
        return reporter.Fail("CreateStateBlock baseline mismatch: Sampler ADDRESSU got=%lu expected=%lu",
                             (unsigned long)got,
                             (unsigned long)samp_addr_u_0);
      }

      got = 0;
      hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetTextureStageState(0, COLOROP) (after Apply pixel baseline)", hr);
      }
      if (got != colorop_0) {
        return reporter.Fail("CreateStateBlock baseline mismatch: TextureStage COLOROP got=%lu expected=%lu",
                             (unsigned long)got,
                             (unsigned long)colorop_0);
      }

      BOOL got_b[2] = {};
      hr = dev->GetPixelShaderConstantB(20, got_b, 2);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetPixelShaderConstantB (after Apply pixel baseline)", hr);
      }
      for (int i = 0; i < 2; ++i) {
        const BOOL a = ps_b_0[i] ? TRUE : FALSE;
        const BOOL b = got_b[i] ? TRUE : FALSE;
        if (a != b) {
          return reporter.Fail("CreateStateBlock baseline mismatch: PixelShaderConstantB[%d] got=%d expected=%d",
                               i,
                               (int)b,
                               (int)a);
        }
      }

      D3DVIEWPORT9 got_vp;
      ZeroMemory(&got_vp, sizeof(got_vp));
      hr = dev->GetViewport(&got_vp);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetViewport (after Apply pixel baseline)", hr);
      }
      if (got_vp.X != vp_0.X || got_vp.Y != vp_0.Y ||
          got_vp.Width != vp_0.Width || got_vp.Height != vp_0.Height ||
          !NearlyEqual(got_vp.MinZ, vp_0.MinZ, 1e-6f) ||
          !NearlyEqual(got_vp.MaxZ, vp_0.MaxZ, 1e-6f)) {
        return reporter.Fail("CreateStateBlock baseline mismatch: Viewport");
      }

      RECT got_scissor;
      ZeroMemory(&got_scissor, sizeof(got_scissor));
      hr = dev->GetScissorRect(&got_scissor);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetScissorRect (after Apply pixel baseline)", hr);
      }
      if (got_scissor.left != scissor_0.left || got_scissor.top != scissor_0.top ||
          got_scissor.right != scissor_0.right || got_scissor.bottom != scissor_0.bottom) {
        return reporter.Fail("CreateStateBlock baseline mismatch: ScissorRect");
      }

      ComPtr<IDirect3DBaseTexture9> got_tex;
      hr = dev->GetTexture(0, got_tex.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("GetTexture(0) (after Apply pixel baseline)", hr);
      }
      if (got_tex.get() != tex_0.get()) {
        return reporter.Fail("CreateStateBlock baseline mismatch: Texture(0) got=%p expected=%p",
                             got_tex.get(),
                             tex_0.get());
      }

      ComPtr<IDirect3DSurface9> got_rt;
      hr = dev->GetRenderTarget(0, got_rt.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("GetRenderTarget(0) (after Apply pixel baseline)", hr);
      }
      if (got_rt.get() != rt_0.get()) {
        return reporter.Fail("CreateStateBlock baseline mismatch: RenderTarget(0) got=%p expected=%p",
                             got_rt.get(),
                             rt_0.get());
      }

      ComPtr<IDirect3DSurface9> got_ds;
      hr = dev->GetDepthStencilSurface(got_ds.put());
      if (FAILED(hr)) {
        return reporter.FailHresult("GetDepthStencilSurface (after Apply pixel baseline)", hr);
      }
      if (got_ds.get() != ds_0.get()) {
        return reporter.Fail("CreateStateBlock baseline mismatch: DepthStencilSurface got=%p expected=%p",
                             got_ds.get(),
                             ds_0.get());
      }

      if (palette_ok) {
        PALETTEENTRY got_pal[256];
        std::memset(got_pal, 0, sizeof(got_pal));
        hr = dev->GetPaletteEntries(palette_idx_0, got_pal);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetPaletteEntries (after Apply pixel baseline)", hr);
        }
        if (std::memcmp(got_pal, palette_0, sizeof(palette_0)) != 0) {
          return reporter.Fail("CreateStateBlock baseline mismatch: PaletteEntries");
        }
      }

      if (current_palette_ok) {
        UINT got = 0;
        hr = dev->GetCurrentTexturePalette(&got);
        if (FAILED(hr)) {
          return reporter.FailHresult("GetCurrentTexturePalette (after Apply pixel baseline)", hr);
        }
        if (got != current_palette_0) {
          return reporter.Fail("CreateStateBlock baseline mismatch: CurrentTexturePalette got=%u expected=%u",
                               (unsigned)got,
                               (unsigned)current_palette_0);
        }
      }

      if (gamma_ok) {
        D3DGAMMARAMP got;
        ZeroMemory(&got, sizeof(got));
        dev->GetGammaRamp(0, &got);
        if (!GammaRampEqual(got, gamma_0)) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: disabling CreateStateBlock GammaRamp baseline check (mismatch; runtime may ignore gamma ramp in windowed mode)",
              kTestName);
          gamma_ok = false;
        }
      }
    }

    // Mutate state to a second configuration, then Capture() it.
    const DWORD alphablend_1 = FALSE;
    hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, alphablend_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE) (pre Capture pixel)", hr);
    }
    const DWORD samp_addr_u_1 = D3DTADDRESS_MIRROR;
    hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, samp_addr_u_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(0, ADDRESSU) (pre Capture pixel)", hr);
    }
    const DWORD colorop_1 = D3DTOP_SUBTRACT;
    hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, colorop_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(0, COLOROP) (pre Capture pixel)", hr);
    }
    const BOOL ps_b_1[2] = {FALSE, TRUE};
    hr = dev->SetPixelShaderConstantB(20, ps_b_1, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPixelShaderConstantB (pre Capture pixel)", hr);
    }
    D3DVIEWPORT9 vp_1 = vp_0;
    vp_1.X = 30;
    vp_1.Y = 40;
    vp_1.Width = 32;
    vp_1.Height = 33;
    vp_1.MinZ = 0.25f;
    vp_1.MaxZ = 0.5f;
    hr = dev->SetViewport(&vp_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport (pre Capture pixel)", hr);
    }
    RECT scissor_1;
    scissor_1.left = 11;
    scissor_1.top = 12;
    scissor_1.right = 90;
    scissor_1.bottom = 100;
    hr = dev->SetScissorRect(&scissor_1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetScissorRect (pre Capture pixel)", hr);
    }

    hr = dev->SetTexture(0, tex_1.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTexture(0) (pre Capture pixel)", hr);
    }

    hr = dev->SetRenderTarget(0, rt_1.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderTarget(0) (pre Capture pixel)", hr);
    }

    hr = dev->SetDepthStencilSurface(ds_1.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetDepthStencilSurface (pre Capture pixel)", hr);
    }

    if (palette_ok) {
      hr = dev->SetPaletteEntries(palette_idx_0, palette_1);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock PaletteEntries check (Set pre Capture failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        palette_ok = false;
      }
    }

    if (current_palette_ok) {
      hr = dev->SetCurrentTexturePalette(current_palette_1);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock CurrentTexturePalette check (Set pre Capture failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        current_palette_ok = false;
      }
    }

    if (gamma_ok) {
      dev->SetGammaRamp(0, 0, &gamma_1);
    }

    hr = sb_pixel->Capture();
    if (FAILED(hr)) {
      return reporter.FailHresult("StateBlock::Capture (pixel)", hr);
    }

    // Clobber again so Apply has visible effect.
    hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, TRUE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderState(D3DRS_ALPHABLENDENABLE) (pre Apply pixel)", hr);
    }
    hr = dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_BORDER);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetSamplerState(0, ADDRESSU) (pre Apply pixel)", hr);
    }
    hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTextureStageState(0, COLOROP) (pre Apply pixel)", hr);
    }
    const BOOL ps_b_2[2] = {FALSE, FALSE};
    hr = dev->SetPixelShaderConstantB(20, ps_b_2, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetPixelShaderConstantB (pre Apply pixel)", hr);
    }
    D3DVIEWPORT9 vp_2 = vp_0;
    vp_2.X = 0;
    vp_2.Y = 0;
    vp_2.Width = 200;
    vp_2.Height = 201;
    vp_2.MinZ = 0.0f;
    vp_2.MaxZ = 1.0f;
    hr = dev->SetViewport(&vp_2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport (pre Apply pixel)", hr);
    }
    RECT scissor_2;
    scissor_2.left = 0;
    scissor_2.top = 0;
    scissor_2.right = 10;
    scissor_2.bottom = 10;
    hr = dev->SetScissorRect(&scissor_2);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetScissorRect (pre Apply pixel)", hr);
    }

    hr = dev->SetTexture(0, tex_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTexture(0) (pre Apply pixel)", hr);
    }

    hr = dev->SetRenderTarget(0, rt_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetRenderTarget(0) (pre Apply pixel)", hr);
    }

    hr = dev->SetDepthStencilSurface(ds_0.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("SetDepthStencilSurface (pre Apply pixel)", hr);
    }

    if (palette_ok) {
      hr = dev->SetPaletteEntries(palette_idx_0, palette_0);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock PaletteEntries check (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        palette_ok = false;
      }
    }

    if (current_palette_ok) {
      hr = dev->SetCurrentTexturePalette(current_palette_0);
      if (FAILED(hr)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: disabling CreateStateBlock CurrentTexturePalette check (clobber Set failed hr=0x%08lX)",
            kTestName,
            (unsigned long)hr);
        current_palette_ok = false;
      }
    }

    if (gamma_ok) {
      dev->SetGammaRamp(0, 0, &gamma_0);
    }

    hr = sb_pixel->Apply();
    if (FAILED(hr)) {
      return reporter.FailHresult("StateBlock::Apply (pixel)", hr);
    }

    // Verify state restored to the captured (alphablend_1 / ...).
    DWORD got = 0;
    hr = dev->GetRenderState(D3DRS_ALPHABLENDENABLE, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderState(D3DRS_ALPHABLENDENABLE) (after Apply pixel)", hr);
    }
    if (got != alphablend_1) {
      return reporter.Fail("CreateStateBlock restore mismatch: ALPHABLENDENABLE got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)alphablend_1);
    }

    got = 0;
    hr = dev->GetSamplerState(0, D3DSAMP_ADDRESSU, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetSamplerState(0, ADDRESSU) (after Apply pixel)", hr);
    }
    if (got != samp_addr_u_1) {
      return reporter.Fail("CreateStateBlock restore mismatch: Sampler ADDRESSU got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)samp_addr_u_1);
    }

    got = 0;
    hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTextureStageState(0, COLOROP) (after Apply pixel)", hr);
    }
    if (got != colorop_1) {
      return reporter.Fail("CreateStateBlock restore mismatch: TextureStage COLOROP got=%lu expected=%lu",
                           (unsigned long)got,
                           (unsigned long)colorop_1);
    }

    BOOL got_b[2] = {};
    hr = dev->GetPixelShaderConstantB(20, got_b, 2);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetPixelShaderConstantB (after Apply pixel)", hr);
    }
    for (int i = 0; i < 2; ++i) {
      const BOOL a = ps_b_1[i] ? TRUE : FALSE;
      const BOOL b = got_b[i] ? TRUE : FALSE;
      if (a != b) {
        return reporter.Fail("CreateStateBlock restore mismatch: PixelShaderConstantB[%d] got=%d expected=%d",
                             i,
                             (int)b,
                             (int)a);
      }
    }

    D3DVIEWPORT9 got_vp;
    ZeroMemory(&got_vp, sizeof(got_vp));
    hr = dev->GetViewport(&got_vp);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetViewport (after Apply pixel)", hr);
    }
    if (got_vp.X != vp_1.X || got_vp.Y != vp_1.Y ||
        got_vp.Width != vp_1.Width || got_vp.Height != vp_1.Height ||
        !NearlyEqual(got_vp.MinZ, vp_1.MinZ, 1e-6f) ||
        !NearlyEqual(got_vp.MaxZ, vp_1.MaxZ, 1e-6f)) {
      return reporter.Fail("CreateStateBlock restore mismatch: Viewport");
    }

    RECT got_scissor;
    ZeroMemory(&got_scissor, sizeof(got_scissor));
    hr = dev->GetScissorRect(&got_scissor);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetScissorRect (after Apply pixel)", hr);
    }
    if (got_scissor.left != scissor_1.left || got_scissor.top != scissor_1.top ||
        got_scissor.right != scissor_1.right || got_scissor.bottom != scissor_1.bottom) {
      return reporter.Fail("CreateStateBlock restore mismatch: ScissorRect");
    }

    ComPtr<IDirect3DBaseTexture9> got_tex;
    hr = dev->GetTexture(0, got_tex.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTexture(0) (after Apply pixel)", hr);
    }
    if (got_tex.get() != tex_1.get()) {
      return reporter.Fail("CreateStateBlock restore mismatch: Texture(0) got=%p expected=%p",
                           got_tex.get(),
                           tex_1.get());
    }

    ComPtr<IDirect3DSurface9> got_rt;
    hr = dev->GetRenderTarget(0, got_rt.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderTarget(0) (after Apply pixel)", hr);
    }
    if (got_rt.get() != rt_1.get()) {
      return reporter.Fail("CreateStateBlock restore mismatch: RenderTarget(0) got=%p expected=%p",
                           got_rt.get(),
                           rt_1.get());
    }

    ComPtr<IDirect3DSurface9> got_ds;
    hr = dev->GetDepthStencilSurface(got_ds.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetDepthStencilSurface (after Apply pixel)", hr);
    }
    if (got_ds.get() != ds_1.get()) {
      return reporter.Fail("CreateStateBlock restore mismatch: DepthStencilSurface got=%p expected=%p",
                           got_ds.get(),
                           ds_1.get());
    }

    if (palette_ok) {
      PALETTEENTRY got_pal[256];
      std::memset(got_pal, 0, sizeof(got_pal));
      hr = dev->GetPaletteEntries(palette_idx_0, got_pal);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetPaletteEntries (after Apply pixel)", hr);
      }
      if (std::memcmp(got_pal, palette_1, sizeof(palette_1)) != 0) {
        return reporter.Fail("CreateStateBlock restore mismatch: PaletteEntries");
      }
    }

    if (current_palette_ok) {
      UINT got = 0;
      hr = dev->GetCurrentTexturePalette(&got);
      if (FAILED(hr)) {
        return reporter.FailHresult("GetCurrentTexturePalette (after Apply pixel)", hr);
      }
      if (got != current_palette_1) {
        return reporter.Fail("CreateStateBlock restore mismatch: CurrentTexturePalette got=%u expected=%u",
                             (unsigned)got,
                             (unsigned)current_palette_1);
      }
    }

    if (gamma_ok) {
      D3DGAMMARAMP got;
      ZeroMemory(&got, sizeof(got));
      dev->GetGammaRamp(0, &got);
      if (!GammaRampEqual(got, gamma_1)) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: skipping CreateStateBlock GetGammaRamp validate (mismatch; runtime may ignore gamma ramp in windowed mode)",
            kTestName);
      }
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9GetStateRoundtrip(argc, argv);
}
