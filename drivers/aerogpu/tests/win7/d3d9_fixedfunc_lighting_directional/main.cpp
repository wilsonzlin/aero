#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
  DWORD color;
};

static void DumpBytesToFile(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT byte_count) {
  if (!file_name || !data || byte_count == 0) {
    return;
  }
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring path = aerogpu_test::JoinPath(dir, file_name);
  HANDLE h =
      CreateFileW(path.c_str(), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    aerogpu_test::PrintfStdout("INFO: %s: dump CreateFileW(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    return;
  }
  DWORD written = 0;
  if (!WriteFile(h, data, byte_count, &written, NULL) || written != byte_count) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name,
                               (unsigned)byte_count,
                               path.c_str());
    if (reporter) {
      reporter->AddArtifactPathW(path);
    }
  }
  CloseHandle(h);
}

static void DumpTightBgra32(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            int row_pitch,
                            int width,
                            int height) {
  if (!data || width <= 0 || height <= 0 || row_pitch < width * 4) {
    return;
  }
  std::vector<uint8_t> tight((size_t)width * (size_t)height * 4u, 0);
  for (int y = 0; y < height; ++y) {
    const uint8_t* src_row = (const uint8_t*)data + (size_t)y * (size_t)row_pitch;
    memcpy(&tight[(size_t)y * (size_t)width * 4u], src_row, (size_t)width * 4u);
  }
  DumpBytesToFile(test_name, reporter, file_name, &tight[0], (UINT)tight.size());
}

static D3DMATRIX MakeIdentityMatrix() {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));
  m._11 = 1.0f;
  m._22 = 1.0f;
  m._33 = 1.0f;
  m._44 = 1.0f;
  return m;
}

static D3DMATRIX MakeScaleTranslateMatrix(float sx, float sy, float sz, float tx, float ty, float tz) {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));
  m._11 = sx;
  m._22 = sy;
  m._33 = sz;
  m._44 = 1.0f;
  m._41 = tx;
  m._42 = ty;
  m._43 = tz;
  return m;
}

static int Brightness(D3DCOLOR c) {
  const int r = (int)((c >> 16) & 0xFFu);
  const int g = (int)((c >> 8) & 0xFFu);
  const int b = (int)((c >> 0) & 0xFFu);
  return r + g + b;
}

static int RunD3D9FixedFuncLightingDirectional(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_lighting_directional";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool strict_checks = require_umd || (!allow_microsoft && !allow_non_aerogpu);

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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncLightingDirectional",
                                              L"AeroGPU D3D9 FixedFunc Lighting Directional",
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
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx (HWVP required)", hr);
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

  if (strict_checks) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

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

  // Fixed-function (no user shaders).
  hr = dev->SetVertexShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetVertexShader(NULL)", hr);
  }
  hr = dev->SetPixelShader(NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetPixelShader(NULL)", hr);
  }

  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_COLORVERTEX, TRUE);
  dev->SetRenderState(D3DRS_LIGHTING, TRUE);
  dev->SetRenderState(D3DRS_AMBIENT, 0);

  // Force stage0 to use vertex diffuse (no texturing).
  dev->SetTexture(0, NULL);
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
  dev->SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
  dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE);

  // Place the object into clip space via WORLD; view/proj remain identity.
  const D3DMATRIX world = MakeScaleTranslateMatrix(0.25f, 0.25f, 1.0f, -1.0f, -1.0f, 0.0f);
  const D3DMATRIX view = MakeIdentityMatrix();
  const D3DMATRIX proj = MakeIdentityMatrix();
  hr = dev->SetTransform(D3DTS_WORLD, &world);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(WORLD)", hr);
  }
  hr = dev->SetTransform(D3DTS_VIEW, &view);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(VIEW)", hr);
  }
  hr = dev->SetTransform(D3DTS_PROJECTION, &proj);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetTransform(PROJECTION)", hr);
  }

  D3DMATERIAL9 mat;
  ZeroMemory(&mat, sizeof(mat));
  mat.Diffuse.r = 1.0f;
  mat.Diffuse.g = 1.0f;
  mat.Diffuse.b = 1.0f;
  mat.Diffuse.a = 1.0f;
  mat.Ambient.a = 1.0f;
  mat.Emissive.a = 1.0f;
  hr = dev->SetMaterial(&mat);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetMaterial", hr);
  }

  D3DLIGHT9 light;
  ZeroMemory(&light, sizeof(light));
  light.Type = D3DLIGHT_DIRECTIONAL;
  light.Diffuse.r = 1.0f;
  light.Diffuse.g = 1.0f;
  light.Diffuse.b = 1.0f;
  light.Diffuse.a = 1.0f;
  light.Ambient.a = 1.0f;
  light.Direction.x = 0.0f;
  light.Direction.y = 0.0f;
  light.Direction.z = -1.0f;
  hr = dev->SetLight(0, &light);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetLight(0)", hr);
  }
  hr = dev->LightEnable(0, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::LightEnable(0, TRUE)", hr);
  }

  Vertex verts[3];
  verts[0].x = 2.0f;
  verts[0].y = 2.0f;
  verts[0].z = 0.5f;
  verts[0].nx = 0.0f;
  verts[0].ny = 0.0f;
  verts[0].nz = 1.0f;
  verts[0].color = 0xFFFFFFFFu;

  verts[1].x = 6.0f;
  verts[1].y = 2.0f;
  verts[1].z = 0.5f;
  verts[1].nx = 0.0f;
  verts[1].ny = 0.0f;
  verts[1].nz = 1.0f;
  verts[1].color = 0xFFFFFFFFu;

  verts[2].x = 4.0f;
  verts[2].y = 6.0f;
  verts[2].z = 0.5f;
  verts[2].nx = 0.0f;
  verts[2].ny = 0.0f;
  verts[2].nz = 1.0f;
  verts[2].color = 0xFFFFFFFFu;

  // Read-back targets.
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
  if (desc.Format != D3DFMT_X8R8G8B8 && desc.Format != D3DFMT_A8R8G8B8) {
    return reporter.Fail("unexpected backbuffer format: %lu", (unsigned long)desc.Format);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(
      desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  auto RenderAndReadCenter = [&](const D3DVECTOR& dir,
                                 std::vector<uint8_t>* out_tight_bgra32,
                                 D3DCOLOR* out_center) -> int {
    if (out_center) {
      *out_center = 0;
    }
    if (out_tight_bgra32) {
      out_tight_bgra32->clear();
    }

    light.Direction = dir;
    hr = dev->SetLight(0, &light);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::SetLight(0) direction", hr);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, 0xFF000000u, 1.0f, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
    }

    hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
    }

    hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
    }

    // Read back before PresentEx: discard swap effect makes post-present contents undefined.
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
    if (out_center) {
      *out_center = center;
    }

    if (out_tight_bgra32) {
      out_tight_bgra32->resize((size_t)desc.Width * (size_t)desc.Height * 4u, 0);
      for (UINT y = 0; y < desc.Height; ++y) {
        const uint8_t* src_row = (const uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
        memcpy(&(*out_tight_bgra32)[(size_t)y * (size_t)desc.Width * 4u],
               src_row,
               (size_t)desc.Width * 4u);
      }
    }

    sysmem->UnlockRect();
    return 0;
  };

  D3DCOLOR center_lit = 0;
  D3DCOLOR center_dark = 0;
  std::vector<uint8_t> lit_img;
  std::vector<uint8_t> dark_img;

  const D3DVECTOR dir_lit = {0.0f, 0.0f, -1.0f};
  const D3DVECTOR dir_dark = {0.0f, 0.0f, 1.0f};

  int rc = RenderAndReadCenter(dir_lit, dump ? &lit_img : NULL, &center_lit);
  if (rc != 0) {
    return rc;
  }
  rc = RenderAndReadCenter(dir_dark, dump ? &dark_img : NULL, &center_dark);
  if (rc != 0) {
    return rc;
  }

  const int b_lit = Brightness(center_lit);
  const int b_dark = Brightness(center_dark);
  const int kDelta = 200;
  if (!(b_lit > b_dark + kDelta) || b_lit < 400 || b_dark > 64) {
    if (dump) {
      // Dump both frames.
      std::string err;
      const std::wstring lit_bmp =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_lighting_directional_lit.bmp");
      const std::wstring dark_bmp =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_lighting_directional_dark.bmp");

      if (!lit_img.empty()) {
        if (aerogpu_test::WriteBmp32BGRA(lit_bmp,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         &lit_img[0],
                                         (int)desc.Width * 4,
                                         &err)) {
          reporter.AddArtifactPathW(lit_bmp);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: lit BMP dump failed: %s", kTestName, err.c_str());
        }
        DumpBytesToFile(kTestName,
                        &reporter,
                        L"d3d9_fixedfunc_lighting_directional_lit.bin",
                        &lit_img[0],
                        (UINT)lit_img.size());
      }
      if (!dark_img.empty()) {
        err.clear();
        if (aerogpu_test::WriteBmp32BGRA(dark_bmp,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         &dark_img[0],
                                         (int)desc.Width * 4,
                                         &err)) {
          reporter.AddArtifactPathW(dark_bmp);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: dark BMP dump failed: %s", kTestName, err.c_str());
        }
        DumpBytesToFile(kTestName,
                        &reporter,
                        L"d3d9_fixedfunc_lighting_directional_dark.bin",
                        &dark_img[0],
                        (UINT)dark_img.size());
      }

      if (lit_img.empty() || dark_img.empty()) {
        // Fallback: dump current sysmem surface if we didn't capture tight buffers.
        D3DLOCKED_RECT lr;
        ZeroMemory(&lr, sizeof(lr));
        if (SUCCEEDED(sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY))) {
          DumpTightBgra32(kTestName,
                          &reporter,
                          L"d3d9_fixedfunc_lighting_directional.bin",
                          lr.pBits,
                          (int)lr.Pitch,
                          (int)desc.Width,
                          (int)desc.Height);
          sysmem->UnlockRect();
        }
      }
    }

    return reporter.Fail("lighting mismatch: center_lit=0x%08lX (b=%d) center_dark=0x%08lX (b=%d) expected b_lit > b_dark + %d",
                         (unsigned long)center_lit,
                         b_lit,
                         (unsigned long)center_dark,
                         b_dark,
                         kDelta);
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9FixedFuncLightingDirectional(argc, argv);
  Sleep(30);
  return rc;
}

