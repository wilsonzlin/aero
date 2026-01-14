#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static int RunD3D9CapsSmoke(int argc, char** argv) {
  const char* kTestName = "d3d9_caps_smoke";
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
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9CapsSmoke",
                                              L"AeroGPU D3D9 Caps Smoke",
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

  D3DCAPS9 caps;
  ZeroMemory(&caps, sizeof(caps));
  hr = dev->GetDeviceCaps(&caps);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::GetDeviceCaps", hr);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: caps summary: VS=0x%08lX PS=0x%08lX MaxTex=%lux%lu Caps=0x%08lX Caps2=0x%08lX DevCaps=0x%08lX",
      kTestName,
      (unsigned long)caps.VertexShaderVersion,
      (unsigned long)caps.PixelShaderVersion,
      (unsigned long)caps.MaxTextureWidth,
      (unsigned long)caps.MaxTextureHeight,
      (unsigned long)caps.Caps,
      (unsigned long)caps.Caps2,
      (unsigned long)caps.DevCaps);
  aerogpu_test::PrintfStdout(
      "INFO: %s: caps bits: PrimitiveMiscCaps=0x%08lX RasterCaps=0x%08lX ZCmpCaps=0x%08lX AlphaCmpCaps=0x%08lX",
      kTestName,
      (unsigned long)caps.PrimitiveMiscCaps,
      (unsigned long)caps.RasterCaps,
      (unsigned long)caps.ZCmpCaps,
      (unsigned long)caps.AlphaCmpCaps);

  if ((caps.Caps2 & D3DCAPS2_CANRENDERWINDOWED) == 0) {
    return reporter.Fail("Caps2 missing D3DCAPS2_CANRENDERWINDOWED");
  }
  if ((caps.Caps2 & D3DCAPS2_CANSHARERESOURCE) == 0) {
    return reporter.Fail("Caps2 missing D3DCAPS2_CANSHARERESOURCE");
  }
  if (caps.VertexShaderVersion < D3DVS_VERSION(2, 0)) {
    return reporter.Fail("VertexShaderVersion too low: got 0x%08lX need >= 2.0", (unsigned long)caps.VertexShaderVersion);
  }
  if (caps.PixelShaderVersion < D3DPS_VERSION(2, 0)) {
    return reporter.Fail("PixelShaderVersion too low: got 0x%08lX need >= 2.0", (unsigned long)caps.PixelShaderVersion);
  }

  if ((caps.DevCaps & D3DDEVCAPS_HWTRANSFORMANDLIGHT) == 0) {
    return reporter.Fail("DevCaps missing D3DDEVCAPS_HWTRANSFORMANDLIGHT");
  }

  if ((caps.DevCaps & D3DDEVCAPS_RTPATCHES) == 0) {
    return reporter.Fail("DevCaps missing D3DDEVCAPS_RTPATCHES");
  }
  const DWORD unsupported_patch_caps = D3DDEVCAPS_NPATCHES | D3DDEVCAPS_QUINTICRTPATCHES;
  if ((caps.DevCaps & unsupported_patch_caps) != 0) {
    return reporter.Fail("DevCaps unexpectedly advertises unsupported patch caps: DevCaps=0x%08lX",
                         (unsigned long)caps.DevCaps);
  }
  if (caps.MaxNpatchTessellationLevel <= 0.0f) {
    return reporter.Fail("MaxNpatchTessellationLevel must be > 0 when patch caps are advertised (got %.2f)",
                         (double)caps.MaxNpatchTessellationLevel);
  }

  if ((caps.RasterCaps & D3DPRASTERCAPS_SCISSORTEST) == 0) {
    return reporter.Fail("RasterCaps missing D3DPRASTERCAPS_SCISSORTEST");
  }
  // StretchRect filtering supports only min/mag point/linear (no mip filtering).
  const DWORD stretchrect_required =
      D3DPTFILTERCAPS_MINFPOINT | D3DPTFILTERCAPS_MINFLINEAR |
      D3DPTFILTERCAPS_MAGFPOINT | D3DPTFILTERCAPS_MAGFLINEAR;
  if ((caps.StretchRectFilterCaps & stretchrect_required) != stretchrect_required) {
    return reporter.Fail("StretchRectFilterCaps missing point+linear min/mag filtering (got 0x%08lX)",
                         (unsigned long)caps.StretchRectFilterCaps);
  }
  const DWORD stretchrect_mip_caps = D3DPTFILTERCAPS_MIPFPOINT | D3DPTFILTERCAPS_MIPFLINEAR;
  if ((caps.StretchRectFilterCaps & stretchrect_mip_caps) != 0) {
    return reporter.Fail("StretchRectFilterCaps unexpectedly advertises mip filtering (got 0x%08lX)",
                         (unsigned long)caps.StretchRectFilterCaps);
  }

  // Fixed-function texture stage operation caps must include the minimal stage0
  // combiner ops that the UMD's fixed-function fallback supports.
  const DWORD required_texop_caps =
      D3DTEXOPCAPS_DISABLE |
      D3DTEXOPCAPS_SELECTARG1 |
      D3DTEXOPCAPS_SELECTARG2 |
      D3DTEXOPCAPS_MODULATE;
  if ((caps.TextureOpCaps & required_texop_caps) != required_texop_caps) {
    return reporter.Fail("TextureOpCaps missing required ops (got 0x%08lX)", (unsigned long)caps.TextureOpCaps);
  }

  if ((caps.ZCmpCaps & D3DPCMPCAPS_ALWAYS) == 0) {
    return reporter.Fail("ZCmpCaps missing D3DPCMPCAPS_ALWAYS");
  }
  if ((caps.AlphaCmpCaps & D3DPCMPCAPS_ALWAYS) == 0) {
    return reporter.Fail("AlphaCmpCaps missing D3DPCMPCAPS_ALWAYS");
  }

  if ((caps.TextureCaps & D3DPTEXTURECAPS_POW2) != 0) {
    return reporter.Fail("TextureCaps unexpectedly includes D3DPTEXTURECAPS_POW2 (NPOT required)");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  return RunD3D9CapsSmoke(argc, argv);
}
