#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <cstring>

using aerogpu_test::ComPtr;

static void DumpBgraBackbuffer(const char* test_name,
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

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
};

static HRESULT DrawFullscreenQuad(IDirect3DDevice9Ex* dev, int width, int height, DWORD color) {
  if (!dev) {
    return E_INVALIDARG;
  }

  Vertex quad[6];
  const float z = 0.5f;
  const float rhw = 1.0f;
  // D3D9 pixel center convention: use a -0.5 offset so the quad reliably covers
  // pixel (0,0) through (W-1,H-1). Without this, edge pixels can be missed,
  // causing false PASS results when validating scissor clipping.
  const float left = -0.5f;
  const float top = -0.5f;
  const float right = (float)width - 0.5f;
  const float bottom = (float)height - 0.5f;

  // Triangle 0: (left,top) (right,top) (right,bottom)
  quad[0].x = left;
  quad[0].y = top;
  quad[0].z = z;
  quad[0].rhw = rhw;
  quad[0].color = color;
  quad[1].x = right;
  quad[1].y = top;
  quad[1].z = z;
  quad[1].rhw = rhw;
  quad[1].color = color;
  quad[2].x = right;
  quad[2].y = bottom;
  quad[2].z = z;
  quad[2].rhw = rhw;
  quad[2].color = color;

  // Triangle 1: (left,top) (right,bottom) (left,bottom)
  quad[3].x = left;
  quad[3].y = top;
  quad[3].z = z;
  quad[3].rhw = rhw;
  quad[3].color = color;
  quad[4].x = right;
  quad[4].y = bottom;
  quad[4].z = z;
  quad[4].rhw = rhw;
  quad[4].color = color;
  quad[5].x = left;
  quad[5].y = bottom;
  quad[5].z = z;
  quad[5].rhw = rhw;
  quad[5].color = color;

  HRESULT hr = dev->BeginScene();
  if (FAILED(hr)) {
    return hr;
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    return hr;
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 2, quad, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return hr;
  }

  hr = dev->EndScene();
  return hr;
}

static int ValidateCenterAndCorner(aerogpu_test::TestReporter& reporter,
                                   const char* test_name,
                                   IDirect3DDevice9Ex* dev,
                                   bool dump,
                                   const wchar_t* dump_bmp_name,
                                   uint32_t expected_center,
                                   uint32_t expected_corner) {
  if (!dev) {
    return reporter.Fail("ValidateCenterAndCorner: device is null");
  }

  // Read back the backbuffer. Do this before PresentEx: with D3DSWAPEFFECT_DISCARD the contents
  // after Present are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  HRESULT hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
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

  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    DumpBgraBackbuffer(test_name,
                       &reporter,
                       dump,
                       dump_bmp_name,
                       lr.pBits,
                       (int)lr.Pitch,
                       (int)desc.Width,
                       (int)desc.Height);
    sysmem->UnlockRect();
    return reporter.Fail("pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                         (unsigned long)center,
                         (unsigned long)expected_center,
                         (unsigned long)corner,
                         (unsigned long)expected_corner);
  }

  sysmem->UnlockRect();
  return 0;
}

static int RunD3D9ExScissorSanity(int argc, char** argv) {
  const char* kTestName = "d3d9ex_scissor_sanity";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExScissorSanity",
                                              L"AeroGPU D3D9Ex Scissor Sanity",
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
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

  // Draw a full-screen quad while scissor testing is enabled, and validate that
  // pixels outside the scissor rect remain at the clear color.
  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kBlue = D3DCOLOR_XRGB(0, 0, 255);
  const uint32_t kExpectedRedBgra = 0xFFFF0000u;
  const uint32_t kExpectedBlueBgra = 0xFF0000FFu;

  // Ensure the scissor rect is established while scissor testing is disabled.
  RECT scissor;
  scissor.left = kWidth / 4;
  scissor.top = kHeight / 4;
  scissor.right = kWidth * 3 / 4;
  scissor.bottom = kHeight * 3 / 4;

  // ---------------------------------------------------------------------------
  // Scenario 0: enable scissor before setting a scissor rect. The default rect is
  // expected to behave like a viewport-sized/full-target scissor (i.e. not clip
  // everything).
  // ---------------------------------------------------------------------------
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE) (scenario 0)", hr);
  }
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear (scenario 0)", hr);
  }
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE) (scenario 0)", hr);
  }
  hr = DrawFullscreenQuad(dev.get(), kWidth, kHeight, kBlue);
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawFullscreenQuad (scenario 0)", hr);
  }
  {
    int rc = ValidateCenterAndCorner(reporter,
                                    kTestName,
                                    dev.get(),
                                    dump,
                                    L"d3d9ex_scissor_sanity_default.bmp",
                                    kExpectedBlueBgra,
                                    kExpectedBlueBgra);
    if (rc != 0) {
      return rc;
    }
  }

  // ---------------------------------------------------------------------------
  // Scenario A: set scissor rect while disabled, then enable scissor and verify
  // clipping.
  // ---------------------------------------------------------------------------
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE) (scenario A)", hr);
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear (scenario A)", hr);
  }

  hr = dev->SetScissorRect(&scissor);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetScissorRect (scenario A)", hr);
  }
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE) (scenario A)", hr);
  }

  hr = DrawFullscreenQuad(dev.get(), kWidth, kHeight, kBlue);
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawFullscreenQuad (scenario A)", hr);
  }
  {
    int rc = ValidateCenterAndCorner(reporter,
                                    kTestName,
                                    dev.get(),
                                    dump,
                                    L"d3d9ex_scissor_sanity_direct.bmp",
                                    kExpectedBlueBgra,
                                    kExpectedRedBgra);
    if (rc != 0) {
      return rc;
    }
  }

  // ---------------------------------------------------------------------------
  // Scenario B: validate scissor clipping when scissor state is restored via
  // state block Apply().
  // ---------------------------------------------------------------------------
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE) (scenario B)", hr);
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear (scenario B)", hr);
  }

  hr = dev->SetScissorRect(&scissor);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetScissorRect (scenario B baseline)", hr);
  }
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, TRUE) (scenario B baseline)", hr);
  }

  ComPtr<IDirect3DStateBlock9> sb_all;
  hr = dev->CreateStateBlock(D3DSBT_ALL, sb_all.put());
  if (FAILED(hr) || !sb_all) {
    return reporter.FailHresult("CreateStateBlock(D3DSBT_ALL) (scenario B)", FAILED(hr) ? hr : E_FAIL);
  }

  // Clobber scissor state, then restore via Apply().
  RECT scissor_clobber;
  scissor_clobber.left = 0;
  scissor_clobber.top = 0;
  scissor_clobber.right = kWidth;
  scissor_clobber.bottom = kHeight;
  hr = dev->SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(D3DRS_SCISSORTESTENABLE, FALSE) (scenario B clobber)", hr);
  }
  hr = dev->SetScissorRect(&scissor_clobber);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetScissorRect (scenario B clobber)", hr);
  }

  hr = sb_all->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock::Apply (scenario B)", hr);
  }

  hr = DrawFullscreenQuad(dev.get(), kWidth, kHeight, kBlue);
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawFullscreenQuad (scenario B)", hr);
  }
  {
    int rc = ValidateCenterAndCorner(reporter,
                                    kTestName,
                                    dev.get(),
                                    dump,
                                    L"d3d9ex_scissor_sanity_stateblock.bmp",
                                    kExpectedBlueBgra,
                                    kExpectedRedBgra);
    if (rc != 0) {
      return rc;
    }
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExScissorSanity(argc, argv);
}
