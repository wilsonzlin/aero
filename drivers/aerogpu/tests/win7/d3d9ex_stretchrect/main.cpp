#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static int RunD3D9ExStretchRect(int argc, char** argv) {
  const char* kTestName = "d3d9ex_stretchrect";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }
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
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExStretchRect",
                                              L"AeroGPU D3D9Ex StretchRect",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Direct3DCreate9Ex", hr);
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
    return aerogpu_test::FailHresult(kTestName, "IDirect3D9Ex::CreateDeviceEx", hr);
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
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return aerogpu_test::Fail(kTestName,
                                "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                "Install AeroGPU driver or pass --allow-microsoft.",
                                (unsigned)ident.VendorId,
                                (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return aerogpu_test::Fail(kTestName,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return aerogpu_test::Fail(kTestName,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return aerogpu_test::Fail(kTestName,
                                "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                                "or use --require-vid/--require-did)",
                                ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(kTestName, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetBackBuffer", hr);
  }

  D3DSURFACE_DESC bb_desc;
  ZeroMemory(&bb_desc, sizeof(bb_desc));
  hr = backbuffer->GetDesc(&bb_desc);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::GetDesc(backbuffer)", hr);
  }

  // ---------------------------------------------------------------------------
  // ColorFill + UpdateSurface + StretchRect
  // ---------------------------------------------------------------------------
  hr = dev->ColorFill(backbuffer.get(), NULL, D3DCOLOR_XRGB(0, 0, 0));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::ColorFill(backbuffer)", hr);
  }

  const int kSrcW = 64;
  const int kSrcH = 64;

  ComPtr<IDirect3DSurface9> src_sys;
  hr = dev->CreateOffscreenPlainSurface(kSrcW,
                                        kSrcH,
                                        bb_desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        src_sys.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface(src_sys)", hr);
  }

  // Fill the system-memory surface with a quadrant pattern so StretchRect scaling is easy to validate.
  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = src_sys->LockRect(&lr, NULL, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::LockRect(src_sys)", hr);
  }

  const uint32_t kRed = D3DCOLOR_XRGB(255, 0, 0);
  const uint32_t kGreen = D3DCOLOR_XRGB(0, 255, 0);
  const uint32_t kBlue = D3DCOLOR_XRGB(0, 0, 255);
  const uint32_t kWhite = D3DCOLOR_XRGB(255, 255, 255);
  for (int y = 0; y < kSrcH; ++y) {
    uint8_t* row = (uint8_t*)lr.pBits + y * lr.Pitch;
    for (int x = 0; x < kSrcW; ++x) {
      uint32_t c = 0;
      const bool left = x < (kSrcW / 2);
      const bool top = y < (kSrcH / 2);
      if (top && left) {
        c = kRed;
      } else if (top && !left) {
        c = kGreen;
      } else if (!top && left) {
        c = kBlue;
      } else {
        c = kWhite;
      }
      ((uint32_t*)row)[x] = c;
    }
  }

  src_sys->UnlockRect();

  ComPtr<IDirect3DSurface9> src_rt;
  hr = dev->CreateRenderTargetEx(kSrcW,
                                 kSrcH,
                                 bb_desc.Format,
                                 D3DMULTISAMPLE_NONE,
                                 0,
                                 FALSE,
                                 src_rt.put(),
                                 NULL,
                                 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetEx(src_rt)", hr);
  }

  hr = dev->UpdateSurface(src_sys.get(), NULL, src_rt.get(), NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::UpdateSurface", hr);
  }

  RECT dst_rect = {32, 32, 32 + 128, 32 + 128};
  hr = dev->StretchRect(src_rt.get(), NULL, backbuffer.get(), &dst_rect, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::StretchRect", hr);
  }

  // Read back the backbuffer to validate the output.
  ComPtr<IDirect3DSurface9> bb_sys;
  hr = dev->CreateOffscreenPlainSurface(bb_desc.Width,
                                        bb_desc.Height,
                                        bb_desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        bb_sys.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface(bb_sys)", hr);
  }
  hr = dev->GetRenderTargetData(backbuffer.get(), bb_sys.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "GetRenderTargetData(backbuffer)", hr);
  }

  ZeroMemory(&lr, sizeof(lr));
  hr = bb_sys->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::LockRect(bb_sys)", hr);
  }

  const uint32_t outside = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);
  const uint32_t tl = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, dst_rect.left + 20, dst_rect.top + 20);
  const uint32_t tr = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, dst_rect.left + 100, dst_rect.top + 20);
  const uint32_t bl = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, dst_rect.left + 20, dst_rect.top + 100);
  const uint32_t br = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, dst_rect.left + 100, dst_rect.top + 100);

  // Compare RGB only: X8 formats can return undefined alpha.
  const uint32_t mask = 0x00FFFFFFu;
  const uint32_t expected_outside = 0xFF000000u;
  if ((outside & mask) != (expected_outside & mask) ||
      (tl & mask) != (kRed & mask) ||
      (tr & mask) != (kGreen & mask) ||
      (bl & mask) != (kBlue & mask) ||
      (br & mask) != (kWhite & mask)) {
    if (dump) {
      std::string err;
      aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                         L"d3d9ex_stretchrect.bmp"),
                                   (int)bb_desc.Width,
                                   (int)bb_desc.Height,
                                   lr.pBits,
                                   (int)lr.Pitch,
                                   &err);
    }
    bb_sys->UnlockRect();
    return aerogpu_test::Fail(kTestName,
                              "pixel mismatch: outside=0x%08lX tl=0x%08lX tr=0x%08lX bl=0x%08lX br=0x%08lX",
                              (unsigned long)outside,
                              (unsigned long)tl,
                              (unsigned long)tr,
                              (unsigned long)bl,
                              (unsigned long)br);
  }

  bb_sys->UnlockRect();

  if (dump) {
    // Re-lock for dump (LockRect/UnlockRect can invalidate lr.pBits).
    hr = bb_sys->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                              L"d3d9ex_stretchrect.bmp"),
                                        (int)bb_desc.Width,
                                        (int)bb_desc.Height,
                                        lr.pBits,
                                        (int)lr.Pitch,
                                        &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      bb_sys->UnlockRect();
    }
  }

  // ---------------------------------------------------------------------------
  // UpdateTexture
  // ---------------------------------------------------------------------------
  const int kTexW = 32;
  const int kTexH = 32;

  ComPtr<IDirect3DTexture9> tex_sys;
  hr = dev->CreateTexture(kTexW,
                          kTexH,
                          1,
                          0,
                          bb_desc.Format,
                          D3DPOOL_SYSTEMMEM,
                          tex_sys.put(),
                          NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(sysmem)", hr);
  }

  D3DLOCKED_RECT tlr;
  ZeroMemory(&tlr, sizeof(tlr));
  hr = tex_sys->LockRect(0, &tlr, NULL, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::LockRect(sysmem)", hr);
  }
  const uint32_t kMagenta = D3DCOLOR_XRGB(255, 0, 255);
  for (int y = 0; y < kTexH; ++y) {
    uint8_t* row = (uint8_t*)tlr.pBits + y * tlr.Pitch;
    for (int x = 0; x < kTexW; ++x) {
      ((uint32_t*)row)[x] = kMagenta;
    }
  }
  tex_sys->UnlockRect(0);

  ComPtr<IDirect3DTexture9> tex_rt;
  hr = dev->CreateTexture(kTexW,
                          kTexH,
                          1,
                          D3DUSAGE_RENDERTARGET,
                          bb_desc.Format,
                          D3DPOOL_DEFAULT,
                          tex_rt.put(),
                          NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(default rendertarget)", hr);
  }

  hr = dev->UpdateTexture(tex_sys.get(), tex_rt.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::UpdateTexture", hr);
  }

  ComPtr<IDirect3DSurface9> tex_rt_surf;
  hr = tex_rt->GetSurfaceLevel(0, tex_rt_surf.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
  }

  ComPtr<IDirect3DSurface9> tex_sys_readback;
  hr = dev->CreateOffscreenPlainSurface(kTexW,
                                        kTexH,
                                        bb_desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        tex_sys_readback.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface(texture readback)", hr);
  }

  hr = dev->GetRenderTargetData(tex_rt_surf.get(), tex_sys_readback.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "GetRenderTargetData(texture)", hr);
  }

  ZeroMemory(&lr, sizeof(lr));
  hr = tex_sys_readback->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "LockRect(texture readback)", hr);
  }
  const uint32_t tex_center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, kTexW / 2, kTexH / 2);
  if ((tex_center & mask) != (kMagenta & mask)) {
    if (dump) {
      std::string err;
      aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                         L"d3d9ex_stretchrect_texture.bmp"),
                                   kTexW,
                                   kTexH,
                                   lr.pBits,
                                   (int)lr.Pitch,
                                   &err);
    }
    tex_sys_readback->UnlockRect();
    return aerogpu_test::Fail(kTestName, "UpdateTexture pixel mismatch: center=0x%08lX expected=0x%08lX",
                              (unsigned long)tex_center,
                              (unsigned long)kMagenta);
  }
  tex_sys_readback->UnlockRect();

  if (dump) {
    hr = tex_sys_readback->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                              L"d3d9ex_stretchrect_texture.bmp"),
                                        kTexW,
                                        kTexH,
                                        lr.pBits,
                                        (int)lr.Pitch,
                                        &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: texture BMP dump failed: %s", kTestName, err.c_str());
      }
      tex_sys_readback->UnlockRect();
    }
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::PresentEx", hr);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExStretchRect(argc, argv);
  Sleep(30);
  return rc;
}

