#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
};

static int RunD3D9ExTriangle(int argc, char** argv) {
  const char* kTestName = "d3d9ex_triangle";
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
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

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--dump] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft]",
                               kTestName);
    return 0;
  }

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExTriangle",
                                              L"AeroGPU D3D9Ex Triangle",
                                              kWidth,
                                              kHeight);
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
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(kTestName, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kGreen = D3DCOLOR_XRGB(0, 255, 0);

  Vertex verts[3];
  // Triangle that covers the center pixel while leaving the top-left corner untouched, so we
  // can validate both the clear color and the draw.
  verts[0].x = (float)kWidth * 0.25f;
  verts[0].y = (float)kHeight * 0.25f;
  verts[0].z = 0.5f;
  verts[0].rhw = 1.0f;
  verts[0].color = kGreen;
  verts[1].x = (float)kWidth * 0.75f;
  verts[1].y = (float)kHeight * 0.25f;
  verts[1].z = 0.5f;
  verts[1].rhw = 1.0f;
  verts[1].color = kGreen;
  verts[2].x = (float)kWidth * 0.5f;
  verts[2].y = (float)kHeight * 0.75f;
  verts[2].z = 0.5f;
  verts[2].rhw = 1.0f;
  verts[2].color = kGreen;

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::EndScene", hr);
  }

  // Read back the backbuffer. Do this before PresentEx: with D3DSWAPEFFECT_DISCARD the contents
  // after Present are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetBackBuffer", hr);
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::GetDesc", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width,
                                        desc.Height,
                                        desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::LockRect", hr);
  }

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  const uint32_t expected = 0xFF00FF00u;  // BGRA = (0, 255, 0, 255).
  const uint32_t expected_corner = 0xFFFF0000u;  // BGRA = (0, 0, 255, 255).
  if ((center & 0x00FFFFFFu) != (expected & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    if (dump) {
      std::string err;
      aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                         L"d3d9ex_triangle.bmp"),
                                   (int)desc.Width,
                                   (int)desc.Height,
                                   lr.pBits,
                                   (int)lr.Pitch,
                                   &err);
    }
    sysmem->UnlockRect();
    return aerogpu_test::Fail(kTestName,
                              "pixel mismatch: center=0x%08lX corner(5,5)=0x%08lX",
                              (unsigned long)center,
                              (unsigned long)corner);
  }

  sysmem->UnlockRect();

  if (dump) {
    // Re-lock for dump (LockRect/UnlockRect can invalidate lr.pBits).
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr)) {
      std::string err;
      if (!aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                                                              L"d3d9ex_triangle.bmp"),
                                        (int)desc.Width,
                                        (int)desc.Height,
                                        lr.pBits,
                                        (int)lr.Pitch,
                                        &err)) {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
      }
      sysmem->UnlockRect();
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
  int rc = RunD3D9ExTriangle(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  // (Harmless for automation; this is a short sleep.)
  Sleep(30);
  return rc;
}
