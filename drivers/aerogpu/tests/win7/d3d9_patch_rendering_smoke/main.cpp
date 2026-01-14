#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
};

static Vertex MixVertex(const Vertex& a, const Vertex& b, float wa, float wb) {
  Vertex out;
  out.x = a.x * wa + b.x * wb;
  out.y = a.y * wa + b.y * wb;
  out.z = a.z * wa + b.z * wb;
  out.rhw = a.rhw * wa + b.rhw * wb;
  out.color = a.color;
  return out;
}

static Vertex MixVertex3(const Vertex& a, const Vertex& b, const Vertex& c, float wa, float wb, float wc) {
  Vertex out;
  out.x = a.x * wa + b.x * wb + c.x * wc;
  out.y = a.y * wa + b.y * wb + c.y * wc;
  out.z = a.z * wa + b.z * wb + c.z * wc;
  out.rhw = a.rhw * wa + b.rhw * wb + c.rhw * wc;
  out.color = a.color;
  return out;
}

static bool FillRectPatchInfo(D3DRECTPATCH_INFO* out) {
  if (!out) {
    return false;
  }
  ZeroMemory(out, sizeof(*out));

  // D3DRECTPATCH_INFO layout varies across header vintages; use sizeof() to pick
  // a compatible layout and memcpy into the runtime struct so this test can
  // build on older toolchains/SDKs.
  //
  // Known layouts:
  // - 16 bytes: {StartVertexOffset, NumVertices, Basis, Degree}
  // - 28 bytes: {StartVertexOffsetWidth, StartVertexOffsetHeight, Width, Height, Stride, Basis, Degree}
  if (sizeof(D3DRECTPATCH_INFO) == 16) {
    struct Info16 {
      UINT StartVertexOffset;
      UINT NumVertices;
      D3DBASISTYPE Basis;
      D3DDEGREETYPE Degree;
    };
    Info16 info;
    info.StartVertexOffset = 0;
    info.NumVertices = 16;
    info.Basis = D3DBASIS_BEZIER;
    info.Degree = D3DDEGREE_CUBIC;
    memcpy(out, &info, sizeof(info));
    return true;
  }
  if (sizeof(D3DRECTPATCH_INFO) == 28) {
    struct Info28 {
      UINT StartVertexOffsetWidth;
      UINT StartVertexOffsetHeight;
      UINT Width;
      UINT Height;
      UINT Stride;
      D3DBASISTYPE Basis;
      D3DDEGREETYPE Degree;
    };
    Info28 info;
    info.StartVertexOffsetWidth = 0;
    info.StartVertexOffsetHeight = 0;
    info.Width = 4;
    info.Height = 4;
    info.Stride = 4;
    info.Basis = D3DBASIS_BEZIER;
    info.Degree = D3DDEGREE_CUBIC;
    memcpy(out, &info, sizeof(info));
    return true;
  }

  return false;
}

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

static inline int AbsDiffU8(uint32_t a, uint32_t b) {
  return (a > b) ? (int)(a - b) : (int)(b - a);
}

static bool ColorCloseRgb(uint32_t got, uint32_t expected, int tol) {
  const uint32_t gb = got & 0xFFu;
  const uint32_t gg = (got >> 8) & 0xFFu;
  const uint32_t gr = (got >> 16) & 0xFFu;

  const uint32_t eb = expected & 0xFFu;
  const uint32_t eg = (expected >> 8) & 0xFFu;
  const uint32_t er = (expected >> 16) & 0xFFu;

  return AbsDiffU8(gr, er) <= tol && AbsDiffU8(gg, eg) <= tol && AbsDiffU8(gb, eb) <= tol;
}

struct ColorMatchInfo {
  bool found;
  int first_x;
  int first_y;
  uint32_t first_pixel;
  uint32_t match_count;

  ColorMatchInfo() : found(false), first_x(0), first_y(0), first_pixel(0), match_count(0) {}
};

static ColorMatchInfo FindColorMatches(const void* bits,
                                       int pitch,
                                       int width,
                                       int height,
                                       uint32_t expected,
                                       int tol) {
  ColorMatchInfo out;
  if (!bits || width <= 0 || height <= 0 || pitch <= 0) {
    return out;
  }
  for (int y = 0; y < height; ++y) {
    for (int x = 0; x < width; ++x) {
      const uint32_t p = aerogpu_test::ReadPixelBGRA(bits, pitch, x, y);
      if (ColorCloseRgb(p, expected, tol)) {
        out.match_count++;
        if (!out.found) {
          out.found = true;
          out.first_x = x;
          out.first_y = y;
          out.first_pixel = p;
        }
      }
    }
  }
  return out;
}

static bool FindColorNearCenter(const void* bits,
                                int pitch,
                                int width,
                                int height,
                                uint32_t expected,
                                int tol,
                                int radius,
                                int* out_x,
                                int* out_y,
                                uint32_t* out_pixel) {
  if (!bits || width <= 0 || height <= 0 || pitch <= 0) {
    return false;
  }
  const int cx = width / 2;
  const int cy = height / 2;
  for (int dy = -radius; dy <= radius; ++dy) {
    for (int dx = -radius; dx <= radius; ++dx) {
      const int x = cx + dx;
      const int y = cy + dy;
      if (x < 0 || y < 0 || x >= width || y >= height) {
        continue;
      }
      const uint32_t p = aerogpu_test::ReadPixelBGRA(bits, pitch, x, y);
      if (ColorCloseRgb(p, expected, tol)) {
        if (out_x) *out_x = x;
        if (out_y) *out_y = y;
        if (out_pixel) *out_pixel = p;
        return true;
      }
    }
  }
  return false;
}

static void DumpFrameIfRequested(const char* test_name,
                                 aerogpu_test::TestReporter* reporter,
                                 const wchar_t* bmp_name,
                                 const wchar_t* bin_name,
                                 const void* bits,
                                 int pitch,
                                 int width,
                                 int height) {
  if (!bmp_name || !bin_name || !bits || width <= 0 || height <= 0 || pitch <= 0) {
    return;
  }
  std::string err;
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring bmp_path = aerogpu_test::JoinPath(dir, bmp_name);
  if (!aerogpu_test::WriteBmp32BGRA(bmp_path, width, height, bits, pitch, &err)) {
    aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed (%ls): %s", test_name, bmp_name, err.c_str());
  } else if (reporter) {
    reporter->AddArtifactPathW(bmp_path);
  }
  DumpTightBgra32(test_name, reporter, bin_name, bits, pitch, width, height);
}

static int ValidateBackbufferStage(const char* test_name,
                                  aerogpu_test::TestReporter* reporter,
                                  const char* stage_name,
                                  IDirect3DDevice9Ex* dev,
                                  IDirect3DSurface9* backbuffer,
                                  IDirect3DSurface9* sysmem,
                                  const D3DSURFACE_DESC& desc,
                                  bool dump,
                                  uint32_t clear_color,
                                  uint32_t expected_patch_color) {
  if (!test_name || !stage_name || !dev || !backbuffer || !sysmem) {
    return reporter ? reporter->Fail("%s: ValidateBackbufferStage: invalid arguments", stage_name) : 1;
  }

  HRESULT hr = dev->GetRenderTargetData(backbuffer, sysmem);
  if (FAILED(hr)) {
    return reporter ? reporter->FailHresult("GetRenderTargetData", hr) : 1;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter ? reporter->FailHresult("IDirect3DSurface9::LockRect", hr) : 1;
  }

  const int width = (int)desc.Width;
  const int height = (int)desc.Height;
  const int corner_x = 5;
  const int corner_y = 5;
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, corner_x, corner_y);

  const int tol = 8;
  const int center_radius = 2;
  int match_x = 0;
  int match_y = 0;
  uint32_t match_px = 0;
  const bool center_ok = FindColorNearCenter(lr.pBits,
                                             (int)lr.Pitch,
                                             width,
                                             height,
                                             expected_patch_color,
                                             tol,
                                             center_radius,
                                             &match_x,
                                             &match_y,
                                             &match_px);

  const bool corner_ok = ColorCloseRgb(corner, clear_color, 2);
  const ColorMatchInfo any_match = FindColorMatches(lr.pBits,
                                                    (int)lr.Pitch,
                                                    width,
                                                    height,
                                                    expected_patch_color,
                                                    tol);

  if (dump) {
    std::wstring stage_w = aerogpu_test::Utf8ToWideFallbackAcp(std::string(stage_name));
    std::wstring bmp_name = L"d3d9_patch_rendering_smoke_";
    bmp_name += stage_w;
    bmp_name += L".bmp";
    std::wstring bin_name = L"d3d9_patch_rendering_smoke_";
    bin_name += stage_w;
    bin_name += L".bin";
    DumpFrameIfRequested(test_name,
                         reporter,
                         bmp_name.c_str(),
                         bin_name.c_str(),
                         lr.pBits,
                         (int)lr.Pitch,
                         width,
                         height);
  }

  sysmem->UnlockRect();

  if (!corner_ok || !center_ok) {
    if (!corner_ok) {
      return reporter
                 ? reporter->Fail("%s: clear pixel mismatch at (%d,%d): got=0x%08lX expected=0x%08lX",
                                  stage_name,
                                  corner_x,
                                  corner_y,
                                  (unsigned long)corner,
                                  (unsigned long)clear_color)
                 : 1;
    }

    if (!any_match.found) {
      return reporter ? reporter->Fail("%s: expected patch color 0x%08lX near center but it was not rendered anywhere "
                                       "(center search radius=%d tol=%d). corner=0x%08lX clear=0x%08lX",
                                       stage_name,
                                       (unsigned long)expected_patch_color,
                                       center_radius,
                                       tol,
                                       (unsigned long)corner,
                                       (unsigned long)clear_color)
                      : 1;
    }

    return reporter ? reporter->Fail("%s: expected patch color 0x%08lX near center but not found "
                                     "(center radius=%d tol=%d). found %lu matching pixels; first match at (%d,%d)=0x%08lX",
                                     stage_name,
                                     (unsigned long)expected_patch_color,
                                     center_radius,
                                     tol,
                                     (unsigned long)any_match.match_count,
                                     any_match.first_x,
                                     any_match.first_y,
                                     (unsigned long)any_match.first_pixel)
                    : 1;
  }

  aerogpu_test::PrintfStdout("INFO: %s: %s: patch color matched near center at (%d,%d)=0x%08lX (matches=%lu)",
                             test_name,
                             stage_name,
                             match_x,
                             match_y,
                             (unsigned long)match_px,
                             (unsigned long)any_match.match_count);
  return 0;
}

static int RunD3D9PatchRenderingSmoke(int argc, char** argv) {
  const char* kTestName = "d3d9_patch_rendering_smoke";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9PatchRenderingSmoke",
                                              L"AeroGPU D3D9 Patch Rendering Smoke",
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

  // Fixed-function state.
  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZWRITEENABLE, FALSE);
  dev->SetRenderState(D3DRS_FILLMODE, D3DFILL_SOLID);
  dev->SetTexture(0, NULL);
  dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
  dev->SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
  dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  dev->SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
  dev->SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE);
  dev->SetTextureStageState(1, D3DTSS_ALPHAOP, D3DTOP_DISABLE);

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  // Colors.
  const DWORD kClearRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kRectBlue = D3DCOLOR_XRGB(0, 0, 255);
  const DWORD kTriYellow = D3DCOLOR_XRGB(255, 255, 0);

  // Control points: 16 rect + 10 tri.
  std::vector<Vertex> control_points;
  control_points.resize(26);

  const float left = (float)kWidth * 0.25f;
  const float right = (float)kWidth * 0.75f;
  const float top = (float)kHeight * 0.25f;
  const float bottom = (float)kHeight * 0.75f;

  // Rect patch control points (4x4, row-major).
  for (int j = 0; j < 4; ++j) {
    const float v = (float)j / 3.0f;
    for (int i = 0; i < 4; ++i) {
      const float u = (float)i / 3.0f;
      Vertex& vert = control_points[(size_t)j * 4u + (size_t)i];
      vert.x = left + u * (right - left);
      vert.y = top + v * (bottom - top);
      vert.z = 0.5f;
      vert.rhw = 1.0f;
      vert.color = kRectBlue;
    }
  }

  // Tri patch control points (cubic Bezier; matches AeroGPU control point order in UMD):
  // [0]=u^3, [1]=3u^2v, [2]=3uv^2, [3]=v^3,
  // [4]=3u^2w, [5]=6uvw, [6]=3v^2w,
  // [7]=3uw^2, [8]=3vw^2, [9]=w^3.
  const Vertex tri_u = {left, bottom, 0.5f, 1.0f, kTriYellow};             // u^3
  const Vertex tri_v = {right, bottom, 0.5f, 1.0f, kTriYellow};            // v^3
  const Vertex tri_w = {(left + right) * 0.5f, top, 0.5f, 1.0f, kTriYellow}; // w^3
  const float a_23 = 2.0f / 3.0f;
  const float a_13 = 1.0f / 3.0f;

  std::vector<Vertex> tri_cp;
  tri_cp.resize(10);
  tri_cp[0] = tri_u;                       // u^3
  tri_cp[1] = MixVertex(tri_u, tri_v, a_23, a_13); // u^2 v
  tri_cp[2] = MixVertex(tri_u, tri_v, a_13, a_23); // u v^2
  tri_cp[3] = tri_v;                       // v^3
  tri_cp[4] = MixVertex(tri_u, tri_w, a_23, a_13); // u^2 w
  tri_cp[5] = MixVertex3(tri_u, tri_v, tri_w, a_13, a_13, a_13); // u v w
  tri_cp[6] = MixVertex(tri_v, tri_w, a_23, a_13); // v^2 w
  tri_cp[7] = MixVertex(tri_u, tri_w, a_13, a_23); // u w^2
  tri_cp[8] = MixVertex(tri_v, tri_w, a_13, a_23); // v w^2
  tri_cp[9] = tri_w;                       // w^3

  for (int i = 0; i < 10; ++i) {
    control_points[16 + i] = tri_cp[(size_t)i];
  }

  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer((UINT)(control_points.size() * sizeof(Vertex)),
                               D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_DEFAULT,
                               vb.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }

  void* vb_ptr = NULL;
  hr = vb->Lock(0, 0, &vb_ptr, D3DLOCK_DISCARD);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(vb_ptr, &control_points[0], control_points.size() * sizeof(Vertex));
  hr = vb->Unlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Unlock", hr);
  }

  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }

  D3DRECTPATCH_INFO rect_info;
  if (!FillRectPatchInfo(&rect_info)) {
    aerogpu_test::PrintfStdout("INFO: %s: unknown D3DRECTPATCH_INFO layout (size=%u); skipping",
                               kTestName,
                               (unsigned)sizeof(D3DRECTPATCH_INFO));
    reporter.SetSkipped("rect_patch_info_layout_unknown");
    return reporter.Pass();
  }

  D3DTRIPATCH_INFO tri_info;
  ZeroMemory(&tri_info, sizeof(tri_info));
  tri_info.StartVertexOffset = 16;
  tri_info.NumVertices = 10;
  tri_info.Basis = D3DBASIS_BEZIER;
  tri_info.Degree = D3DDEGREE_CUBIC;

  float rect_segs[4] = {8.0f, 8.0f, 8.0f, 8.0f};
  float tri_segs[3] = {8.0f, 8.0f, 8.0f};

  // Backbuffer readback surfaces.
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

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  // Stage 1: DrawRectPatch twice (cache hit path).
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClearRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->DrawRectPatch(1, rect_segs, &rect_info);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawRectPatch (first)", hr);
  }
  hr = dev->DrawRectPatch(1, rect_segs, &rect_info);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawRectPatch (second)", hr);
  }
  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }
  int stage_rc = ValidateBackbufferStage(kTestName,
                                         &reporter,
                                         "rect_twice",
                                         dev.get(),
                                         backbuffer.get(),
                                         sysmem.get(),
                                         desc,
                                         dump,
                                         kClearRed,
                                         kRectBlue);
  if (stage_rc != 0) {
    return stage_rc;
  }

  // Delete the patch and re-draw with the same handle. This should still render.
  hr = dev->DeletePatch(1);
  if (FAILED(hr)) {
    return reporter.FailHresult("DeletePatch", hr);
  }

  // Stage 2: DrawRectPatch after DeletePatch.
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClearRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->DrawRectPatch(1, rect_segs, &rect_info);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawRectPatch (after DeletePatch)", hr);
  }
  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }
  stage_rc = ValidateBackbufferStage(kTestName,
                                     &reporter,
                                     "rect_after_delete",
                                     dev.get(),
                                     backbuffer.get(),
                                     sysmem.get(),
                                     desc,
                                     dump,
                                     kClearRed,
                                     kRectBlue);
  if (stage_rc != 0) {
    return stage_rc;
  }

  // Stage 3: DrawTriPatch (smoke test).
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClearRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->DrawTriPatch(2, tri_segs, &tri_info);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawTriPatch", hr);
  }
  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }
  stage_rc = ValidateBackbufferStage(kTestName,
                                     &reporter,
                                     "tri",
                                     dev.get(),
                                     backbuffer.get(),
                                     sysmem.get(),
                                     desc,
                                     dump,
                                     kClearRed,
                                     kTriYellow);
  if (stage_rc != 0) {
    return stage_rc;
  }

  // Delete the tri patch and re-draw with the same handle. This should still render.
  hr = dev->DeletePatch(2);
  if (FAILED(hr)) {
    return reporter.FailHresult("DeletePatch(tri)", hr);
  }

  // Stage 4: DrawTriPatch after DeletePatch.
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClearRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear(tri after delete)", hr);
  }
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene(tri after delete)", hr);
  }
  hr = dev->DrawTriPatch(2, tri_segs, &tri_info);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawTriPatch(after DeletePatch)", hr);
  }
  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene(tri after delete)", hr);
  }
  stage_rc = ValidateBackbufferStage(kTestName,
                                     &reporter,
                                     "tri_after_delete",
                                     dev.get(),
                                     backbuffer.get(),
                                     sysmem.get(),
                                     desc,
                                     dump,
                                     kClearRed,
                                     kTriYellow);
  if (stage_rc != 0) {
    return stage_rc;
  }

  // Optional present for manual observation when running interactively.
  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9PatchRenderingSmoke(argc, argv);
  Sleep(30);
  return rc;
}
