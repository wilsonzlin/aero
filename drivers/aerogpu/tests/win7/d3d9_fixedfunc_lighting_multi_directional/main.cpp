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
  HANDLE h = CreateFileW(path.c_str(), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
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

static int ChannelR(D3DCOLOR c) {
  return (int)((c >> 16) & 0xFFu);
}
static int ChannelG(D3DCOLOR c) {
  return (int)((c >> 8) & 0xFFu);
}
static int ChannelB(D3DCOLOR c) {
  return (int)((c >> 0) & 0xFFu);
}

static int RunD3D9FixedFuncLightingMultiDirectional(int argc, char** argv) {
  const char* kTestName = "d3d9_fixedfunc_lighting_multi_directional";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9FixedFuncLightingMultiDirectional",
                                              L"AeroGPU D3D9 FixedFunc Lighting Multi Directional",
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

  // White material.
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

  // Light0: red directional.
  D3DLIGHT9 light0;
  ZeroMemory(&light0, sizeof(light0));
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Diffuse.r = 1.0f;
  light0.Diffuse.g = 0.0f;
  light0.Diffuse.b = 0.0f;
  light0.Diffuse.a = 1.0f;
  light0.Ambient.a = 1.0f;
  light0.Direction.x = 0.0f;
  light0.Direction.y = 0.0f;
  light0.Direction.z = -1.0f;
  hr = dev->SetLight(0, &light0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetLight(0)", hr);
  }
  hr = dev->LightEnable(0, TRUE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::LightEnable(0, TRUE)", hr);
  }

  // Light1: green directional.
  D3DLIGHT9 light1;
  ZeroMemory(&light1, sizeof(light1));
  light1.Type = D3DLIGHT_DIRECTIONAL;
  light1.Diffuse.r = 0.0f;
  light1.Diffuse.g = 1.0f;
  light1.Diffuse.b = 0.0f;
  light1.Diffuse.a = 1.0f;
  light1.Ambient.a = 1.0f;
  light1.Direction.x = 0.0f;
  light1.Direction.y = 0.0f;
  light1.Direction.z = -1.0f;
  hr = dev->SetLight(1, &light1);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetLight(1)", hr);
  }
  hr = dev->LightEnable(1, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::LightEnable(1, FALSE)", hr);
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
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  auto RenderAndReadCenter = [&](bool enable_light1,
                                 std::vector<uint8_t>* out_tight_bgra32,
                                 D3DCOLOR* out_center) -> int {
    if (out_center) {
      *out_center = 0;
    }
    if (out_tight_bgra32) {
      out_tight_bgra32->clear();
    }

    hr = dev->LightEnable(1, enable_light1 ? TRUE : FALSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::LightEnable(1)", hr);
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
        memcpy(&(*out_tight_bgra32)[(size_t)y * (size_t)desc.Width * 4u], src_row, (size_t)desc.Width * 4u);
      }
    }

    sysmem->UnlockRect();
    return 0;
  };

  D3DCOLOR center_red = 0;
  D3DCOLOR center_red_green = 0;
  std::vector<uint8_t> red_img;
  std::vector<uint8_t> red_green_img;

  int rc = RenderAndReadCenter(false, dump ? &red_img : NULL, &center_red);
  if (rc != 0) {
    return rc;
  }
  rc = RenderAndReadCenter(true, dump ? &red_green_img : NULL, &center_red_green);
  if (rc != 0) {
    return rc;
  }

  const int r0 = ChannelR(center_red);
  const int g0 = ChannelG(center_red);
  const int b0 = ChannelB(center_red);
  const int r1 = ChannelR(center_red_green);
  const int g1 = ChannelG(center_red_green);
  const int b1 = ChannelB(center_red_green);

  const int kDelta = 150;
  if (!(r0 > 200 && g0 < 64 && b0 < 64 && r1 > 200 && g1 > g0 + kDelta && b1 < 64)) {
    if (dump) {
      std::string err;
      const std::wstring red_bmp =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_lighting_multi_directional_red.bmp");
      const std::wstring red_green_bmp =
          aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_fixedfunc_lighting_multi_directional_red_green.bmp");

      if (!red_img.empty()) {
        if (aerogpu_test::WriteBmp32BGRA(red_bmp,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         &red_img[0],
                                         (int)desc.Width * 4,
                                         &err)) {
          reporter.AddArtifactPathW(red_bmp);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: red BMP dump failed: %s", kTestName, err.c_str());
        }
        DumpBytesToFile(kTestName,
                        &reporter,
                        L"d3d9_fixedfunc_lighting_multi_directional_red.bin",
                        &red_img[0],
                        (UINT)red_img.size());
      }

      err.clear();
      if (!red_green_img.empty()) {
        if (aerogpu_test::WriteBmp32BGRA(red_green_bmp,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         &red_green_img[0],
                                         (int)desc.Width * 4,
                                         &err)) {
          reporter.AddArtifactPathW(red_green_bmp);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: red+green BMP dump failed: %s", kTestName, err.c_str());
        }
        DumpBytesToFile(kTestName,
                        &reporter,
                        L"d3d9_fixedfunc_lighting_multi_directional_red_green.bin",
                        &red_green_img[0],
                        (UINT)red_green_img.size());
      }
    }

    return reporter.Fail("multi-light mismatch: red_only=0x%08lX (r=%d g=%d b=%d) red_green=0x%08lX (r=%d g=%d b=%d) expected red-only ~red and red+green adds green",
                         (unsigned long)center_red,
                         r0,
                         g0,
                         b0,
                         (unsigned long)center_red_green,
                         r1,
                         g1,
                         b1);
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9FixedFuncLightingMultiDirectional(argc, argv);
  Sleep(30);
  return rc;
}

