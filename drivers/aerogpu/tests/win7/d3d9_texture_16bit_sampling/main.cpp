#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <cstring>

using aerogpu_test::ComPtr;

struct VertexPosTex {
  float x;
  float y;
  float z;
  float w;
  float u;
  float v;
  float tu2;
  float tv2;
};

// Vertex shader (vs_2_0):
//   mov oPos, v0
//   mov oT0, v1
//   end
static const DWORD kVsCopyPosTex[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, 0x400F0000u, 0x10E40000u, // mov oPos, v0
    0x03000001u, 0x600F0000u, 0x10E40001u, // mov oT0, v1
    0x0000FFFFu, // end
};

// Pixel shader (ps_2_0):
//   texld r0, t0, s0
//   mov oC0, r0
//   end
static const DWORD kPsCopyTex[] = {
    0xFFFF0200u, // ps_2_0
    0x04000042u, 0x000F0000u, 0x30E40000u, 0x20E40800u, // texld r0, t0, s0
    0x03000001u, 0x000F0800u, 0x00E40000u, // mov oC0, r0
    0x0000FFFFu, // end
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
                               test_name ? test_name : "<null>",
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    return;
  }
  DWORD written = 0;
  if (!WriteFile(h, data, byte_count, &written, NULL) || written != byte_count) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name ? test_name : "<null>",
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name ? test_name : "<null>",
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

static inline uint8_t Expand5To8(uint32_t v) { return (uint8_t)((v << 3) | (v >> 2)); }
static inline uint8_t Expand6To8(uint32_t v) { return (uint8_t)((v << 2) | (v >> 4)); }

static inline uint32_t MakeXrgb(uint8_t r, uint8_t g, uint8_t b) {
  return 0xFF000000u | ((uint32_t)r << 16) | ((uint32_t)g << 8) | (uint32_t)b;
}

static uint32_t ConvertR5G6B5ToXrgb(uint16_t v) {
  const uint8_t r = Expand5To8((v >> 11) & 0x1Fu);
  const uint8_t g = Expand6To8((v >> 5) & 0x3Fu);
  const uint8_t b = Expand5To8((v >> 0) & 0x1Fu);
  return MakeXrgb(r, g, b);
}

static uint32_t ConvertA1R5G5B5ToXrgb(uint16_t v) {
  const uint8_t r = Expand5To8((v >> 10) & 0x1Fu);
  const uint8_t g = Expand5To8((v >> 5) & 0x1Fu);
  const uint8_t b = Expand5To8((v >> 0) & 0x1Fu);
  return MakeXrgb(r, g, b);
}

static inline uint8_t GetR(uint32_t argb) { return (uint8_t)((argb >> 16) & 0xFFu); }
static inline uint8_t GetG(uint32_t argb) { return (uint8_t)((argb >> 8) & 0xFFu); }
static inline uint8_t GetB(uint32_t argb) { return (uint8_t)((argb >> 0) & 0xFFu); }

static bool PixelRgbNear(uint32_t got_argb, uint32_t expected_argb, int tolerance) {
  const int dr = abs((int)GetR(got_argb) - (int)GetR(expected_argb));
  const int dg = abs((int)GetG(got_argb) - (int)GetG(expected_argb));
  const int db = abs((int)GetB(got_argb) - (int)GetB(expected_argb));
  return (dr <= tolerance) && (dg <= tolerance) && (db <= tolerance);
}

struct Texture16TestCase {
  const char* label;
  D3DFORMAT format;
  uint16_t texels[4];  // row-major: (0,0) (1,0) (0,1) (1,1)
};

static int RunTexture16SamplingCase(aerogpu_test::TestReporter& reporter,
                                   const char* test_name,
                                   IDirect3D9Ex* d3d,
                                   IDirect3DDevice9Ex* dev,
                                   const Texture16TestCase& tc,
                                   bool dump) {
  if (!test_name || !d3d || !dev) {
    return reporter.Fail("RunTexture16SamplingCase: invalid args");
  }

  HRESULT hr = d3d->CheckDeviceFormat(D3DADAPTER_DEFAULT,
                                      D3DDEVTYPE_HAL,
                                      D3DFMT_X8R8G8B8,
                                      0,
                                      D3DRTYPE_TEXTURE,
                                      tc.format);
  if (FAILED(hr)) {
    return reporter.FailHresult(aerogpu_test::FormatString("CheckDeviceFormat(%s)", tc.label).c_str(), hr);
  }

  // Stage through a systemmem texture so LockRect works reliably even when default-pool allocations
  // are guest-backed.
  ComPtr<IDirect3DTexture9> sys_tex;
  hr = dev->CreateTexture(2, 2, 1, 0, tc.format, D3DPOOL_SYSTEMMEM, sys_tex.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult(aerogpu_test::FormatString("CreateTexture(sysmem %s)", tc.label).c_str(), hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sys_tex->LockRect(0, &lr, NULL, 0);
  if (FAILED(hr) || !lr.pBits) {
    return reporter.FailHresult(aerogpu_test::FormatString("LockRect(sysmem %s)", tc.label).c_str(),
                               FAILED(hr) ? hr : E_FAIL);
  }
  for (int y = 0; y < 2; ++y) {
    uint8_t* row = (uint8_t*)lr.pBits + (size_t)y * (size_t)lr.Pitch;
    uint16_t* dst = (uint16_t*)row;
    dst[0] = tc.texels[y * 2 + 0];
    dst[1] = tc.texels[y * 2 + 1];
  }
  sys_tex->UnlockRect(0);

  ComPtr<IDirect3DTexture9> gpu_tex;
  hr = dev->CreateTexture(2, 2, 1, 0, tc.format, D3DPOOL_DEFAULT, gpu_tex.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult(aerogpu_test::FormatString("CreateTexture(default %s)", tc.label).c_str(), hr);
  }
  hr = dev->UpdateTexture(sys_tex.get(), gpu_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult(aerogpu_test::FormatString("UpdateTexture(%s)", tc.label).c_str(), hr);
  }

  // Create shaders.
  ComPtr<IDirect3DVertexShader9> vs;
  hr = dev->CreateVertexShader(kVsCopyPosTex, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }
  ComPtr<IDirect3DPixelShader9> ps;
  hr = dev->CreatePixelShader(kPsCopyTex, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }

  // Create vertex declaration (pos + tex).
  const D3DVERTEXELEMENT9 decl[] = {
      {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {0, 16, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> vdecl;
  hr = dev->CreateVertexDeclaration(decl, vdecl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }

  // Full-screen quad.
  const VertexPosTex verts[4] = {
      {-1.0f, -1.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
      {-1.0f, 1.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 1.0f, 1.0f, 0.0f, 1.0f},
      {1.0f, 1.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(sizeof(verts),
                               D3DUSAGE_WRITEONLY | D3DUSAGE_DYNAMIC,
                               0,
                               D3DPOOL_DEFAULT,
                               vb.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }
  void* vb_ptr = NULL;
  hr = vb->Lock(0, sizeof(verts), &vb_ptr, D3DLOCK_DISCARD);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("VertexBuffer Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, verts, sizeof(verts));
  vb->Unlock();

  // Pipeline state.
  hr = dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(ZENABLE=FALSE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(CULLMODE=NONE)", hr);
  }
  hr = dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderState(ALPHABLENDENABLE=FALSE)", hr);
  }
  dev->SetRenderState(D3DRS_SRGBWRITEENABLE, FALSE);

  hr = dev->SetSamplerState(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MINFILTER=POINT)", hr);
  }
  hr = dev->SetSamplerState(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetSamplerState(MAGFILTER=POINT)", hr);
  }
  dev->SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
  dev->SetSamplerState(0, D3DSAMP_SRGBTEXTURE, FALSE);

  hr = dev->SetVertexShader(vs.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader", hr);
  }
  hr = dev->SetPixelShader(ps.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader", hr);
  }
  hr = dev->SetVertexDeclaration(vdecl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexDeclaration", hr);
  }
  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(VertexPosTex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }
  hr = dev->SetTexture(0, gpu_tex.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTexture(0)", hr);
  }

  // Draw.
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, 0xFF000000u, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->DrawPrimitive(D3DPT_TRIANGLESTRIP, 0, 2);
  dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("DrawPrimitive", hr);
  }
  dev->Flush();

  // Read back before PresentEx; with D3DSWAPEFFECT_DISCARD the contents after Present are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetBackBuffer", hr);
  }
  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetDesc(backbuffer)", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }
  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT bb;
  ZeroMemory(&bb, sizeof(bb));
  hr = sysmem->LockRect(&bb, NULL, D3DLOCK_READONLY);
  if (FAILED(hr) || !bb.pBits) {
    return reporter.FailHresult("LockRect(sysmem backbuffer)", FAILED(hr) ? hr : E_FAIL);
  }

  if (dump) {
    std::string err;
    const std::wstring bmp_path =
        aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(),
                               aerogpu_test::Utf8ToWideFallbackAcp(aerogpu_test::FormatString("%s_%s.bmp", test_name, tc.label)).c_str());
    if (aerogpu_test::WriteBmp32BGRA(bmp_path, (int)desc.Width, (int)desc.Height, bb.pBits, (int)bb.Pitch, &err)) {
      reporter.AddArtifactPathW(bmp_path);
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name, err.c_str());
    }

    const std::wstring bin_leaf =
        aerogpu_test::Utf8ToWideFallbackAcp(aerogpu_test::FormatString("%s_%s.bin", test_name, tc.label));
    DumpTightBgra32(test_name, &reporter, bin_leaf.c_str(), bb.pBits, (int)bb.Pitch, (int)desc.Width, (int)desc.Height);
  }

  // Sample four points (center of each quadrant).
  const int w = (int)desc.Width;
  const int h = (int)desc.Height;
  const int x0 = (w > 0) ? (w / 4) : 0;
  const int y0 = (h > 0) ? (h / 4) : 0;
  const int x1 = (w > 0) ? ((w * 3) / 4) : 0;
  const int y1 = (h > 0) ? ((h * 3) / 4) : 0;

  const uint32_t tl = aerogpu_test::ReadPixelBGRA(bb.pBits, (int)bb.Pitch, x0, y0);
  const uint32_t tr = aerogpu_test::ReadPixelBGRA(bb.pBits, (int)bb.Pitch, x1, y0);
  const uint32_t bl = aerogpu_test::ReadPixelBGRA(bb.pBits, (int)bb.Pitch, x0, y1);
  const uint32_t br = aerogpu_test::ReadPixelBGRA(bb.pBits, (int)bb.Pitch, x1, y1);

  sysmem->UnlockRect();

  const int kTol = 8;
  uint32_t exp_tl = 0;
  uint32_t exp_tr = 0;
  uint32_t exp_bl = 0;
  uint32_t exp_br = 0;
  if (tc.format == D3DFMT_R5G6B5) {
    exp_tl = ConvertR5G6B5ToXrgb(tc.texels[0]);
    exp_tr = ConvertR5G6B5ToXrgb(tc.texels[1]);
    exp_bl = ConvertR5G6B5ToXrgb(tc.texels[2]);
    exp_br = ConvertR5G6B5ToXrgb(tc.texels[3]);
  } else if (tc.format == D3DFMT_A1R5G5B5) {
    exp_tl = ConvertA1R5G5B5ToXrgb(tc.texels[0]);
    exp_tr = ConvertA1R5G5B5ToXrgb(tc.texels[1]);
    exp_bl = ConvertA1R5G5B5ToXrgb(tc.texels[2]);
    exp_br = ConvertA1R5G5B5ToXrgb(tc.texels[3]);
  } else {
    return reporter.Fail("internal error: unexpected format for %s", tc.label);
  }

  const bool ok_tl = PixelRgbNear(tl, exp_tl, kTol);
  const bool ok_tr = PixelRgbNear(tr, exp_tr, kTol);
  const bool ok_bl = PixelRgbNear(bl, exp_bl, kTol);
  const bool ok_br = PixelRgbNear(br, exp_br, kTol);
  if (!ok_tl || !ok_tr || !ok_bl || !ok_br) {
    return reporter.Fail(
        "%s texture sampling mismatch (tol=%d). "
        "TL(%d,%d) got=0x%08lX rgb=(%u,%u,%u) exp≈0x%08lX rgb=(%u,%u,%u); "
        "TR(%d,%d) got=0x%08lX rgb=(%u,%u,%u) exp≈0x%08lX rgb=(%u,%u,%u); "
        "BL(%d,%d) got=0x%08lX rgb=(%u,%u,%u) exp≈0x%08lX rgb=(%u,%u,%u); "
        "BR(%d,%d) got=0x%08lX rgb=(%u,%u,%u) exp≈0x%08lX rgb=(%u,%u,%u)",
        tc.label,
        kTol,
        x0,
        y0,
        (unsigned long)tl,
        (unsigned)GetR(tl),
        (unsigned)GetG(tl),
        (unsigned)GetB(tl),
        (unsigned long)exp_tl,
        (unsigned)GetR(exp_tl),
        (unsigned)GetG(exp_tl),
        (unsigned)GetB(exp_tl),
        x1,
        y0,
        (unsigned long)tr,
        (unsigned)GetR(tr),
        (unsigned)GetG(tr),
        (unsigned)GetB(tr),
        (unsigned long)exp_tr,
        (unsigned)GetR(exp_tr),
        (unsigned)GetG(exp_tr),
        (unsigned)GetB(exp_tr),
        x0,
        y1,
        (unsigned long)bl,
        (unsigned)GetR(bl),
        (unsigned)GetG(bl),
        (unsigned)GetB(bl),
        (unsigned long)exp_bl,
        (unsigned)GetR(exp_bl),
        (unsigned)GetG(exp_bl),
        (unsigned)GetB(exp_bl),
        x1,
        y1,
        (unsigned long)br,
        (unsigned)GetR(br),
        (unsigned)GetG(br),
        (unsigned)GetB(br),
        (unsigned long)exp_br,
        (unsigned)GetR(exp_br),
        (unsigned)GetG(exp_br),
        (unsigned)GetB(exp_br));
  }

  return 0;
}

static int RunD3D9Texture16BitSampling(int argc, char** argv) {
  const char* kTestName = "d3d9_texture_16bit_sampling";
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

  const int kWidth = 64;
  const int kHeight = 64;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9Texture16BitSampling",
                                              L"AeroGPU D3D9 16-bit Texture Sampling",
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
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateDeviceEx", hr);
  }

  // Test cases: 2x2 texture with distinct corners.
  const Texture16TestCase tc_r5g6b5 = {"R5G6B5",
                                       D3DFMT_R5G6B5,
                                       {
                                           0xF800u, // red
                                           0x07E0u, // green
                                           0x001Fu, // blue
                                           0xFFFFu, // white
                                       }};

  int rc = RunTexture16SamplingCase(reporter, kTestName, d3d.get(), dev.get(), tc_r5g6b5, dump);
  if (rc != 0) {
    return rc;
  }

  // Optional: A1R5G5B5 (skip if not supported).
  const Texture16TestCase tc_a1r5g5b5 = {"A1R5G5B5",
                                        D3DFMT_A1R5G5B5,
                                        {
                                            0xFC00u, // red (a=1,r=31)
                                            0x83E0u, // green (a=1,g=31)
                                            0x801Fu, // blue (a=1,b=31)
                                            0xFFFFu, // white
                                        }};

  hr = d3d->CheckDeviceFormat(D3DADAPTER_DEFAULT,
                              D3DDEVTYPE_HAL,
                              D3DFMT_X8R8G8B8,
                              0,
                              D3DRTYPE_TEXTURE,
                              tc_a1r5g5b5.format);
  if (SUCCEEDED(hr)) {
    rc = RunTexture16SamplingCase(reporter, kTestName, d3d.get(), dev.get(), tc_a1r5g5b5, dump);
    if (rc != 0) {
      return rc;
    }
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: skipping optional %s test (CheckDeviceFormat hr=%s)",
                               kTestName,
                               tc_a1r5g5b5.label,
                               aerogpu_test::HresultToString(hr).c_str());
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9Texture16BitSampling(argc, argv);
  Sleep(30);
  return rc;
}
