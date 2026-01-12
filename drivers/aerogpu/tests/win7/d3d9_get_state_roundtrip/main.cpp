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

  // Clip plane round-trip (fixed-function cached state).
  if (caps.MaxUserClipPlanes >= 1) {
    const float plane_set[4] = {1.25f, -2.5f, 3.75f, -4.0f};
    hr = dev->SetClipPlane(0, plane_set);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetClipPlane(0)", hr);
    }
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
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetClipPlane (MaxUserClipPlanes=%lu)",
                               kTestName,
                               (unsigned long)caps.MaxUserClipPlanes);
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

  // Fixed-function lighting/material caching.
  if (caps.MaxActiveLights >= 1) {
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
    if (std::memcmp(&got_mat, &mat, sizeof(mat)) != 0) {
      return reporter.Fail("GetMaterial mismatch");
    }

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
      return reporter.FailHresult("SetLight(0)", hr);
    }
    D3DLIGHT9 got_light;
    ZeroMemory(&got_light, sizeof(got_light));
    hr = dev->GetLight(0, &got_light);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetLight(0)", hr);
    }
    if (std::memcmp(&got_light, &light, sizeof(light)) != 0) {
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
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: skipping Set/GetMaterial/Light (MaxActiveLights=%lu)",
                               kTestName,
                               (unsigned long)caps.MaxActiveLights);
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
  const int vs_i_sb[4] = {101, 102, 103, 104};
  const BOOL ps_b_sb[2] = {TRUE, FALSE};
  const UINT stream_freq_sb = 13;
  const UINT stream_freq_clobber = 1;
  bool stream_freq_test = false;

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

    D3DMATRIX got_world;
    ZeroMemory(&got_world, sizeof(got_world));
    hr = dev->GetTransform(D3DTS_WORLD, &got_world);
    if (FAILED(hr)) {
      return reporter.FailHresult("GetTransform(D3DTS_WORLD) (after Apply)", hr);
    }
    if (!MatrixEqual(got_world, world_sb)) {
      return reporter.Fail("stateblock restore mismatch: WORLD matrix mismatch");
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
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9GetStateRoundtrip(argc, argv);
}
