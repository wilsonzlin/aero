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
  if (!WriteFile(h, &tight[0], (DWORD)tight.size(), &written, NULL) || written != (DWORD)tight.size()) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name,
                               (unsigned)tight.size(),
                               path.c_str());
    if (reporter) {
      reporter->AddArtifactPathW(path);
    }
  }
  CloseHandle(h);
}

static int RunD3D9PatchSanity(int argc, char** argv) {
  const char* kTestName = "d3d9_patch_sanity";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9PatchSanity",
                                              L"AeroGPU D3D9 Patch Sanity",
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

  D3DCAPS9 caps;
  ZeroMemory(&caps, sizeof(caps));
  hr = d3d->GetDeviceCaps(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::GetDeviceCaps", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: DevCaps=0x%08lX MaxNpatchTessellationLevel=%.2f",
                             kTestName,
                             (unsigned long)caps.DevCaps,
                             (double)caps.MaxNpatchTessellationLevel);

  // D3D9 "RT patches" cover both rectangular and triangular high-order surfaces
  // (DrawRectPatch / DrawTriPatch).
  const bool caps_tri = (caps.DevCaps & D3DDEVCAPS_RTPATCHES) != 0 && caps.MaxNpatchTessellationLevel > 0.0f;
  const bool caps_rect = (caps.DevCaps & D3DDEVCAPS_RTPATCHES) != 0 && caps.MaxNpatchTessellationLevel > 0.0f;
  if (!caps_tri && !caps_rect) {
    aerogpu_test::PrintfStdout("INFO: %s: patch caps not advertised; skipping", kTestName);
    reporter.SetSkipped("patch_caps_missing");
    return reporter.Pass();
  }

  // Fixed-function state: keep this test deterministic by disabling common
  // state that could affect color output (textures/blending/depth).
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

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  bool ran_any = false;

  if (caps_tri) {
    const DWORD kBlue = D3DCOLOR_XRGB(0, 0, 255);

    // Build a cubic Bezier tri patch (10 control points). The UMD's patch path
    // expects the control points in Bernstein order:
    //   [0]=u^3, [1]=3u^2v, [2]=3uv^2, [3]=v^3,
    //   [4]=3u^2w, [5]=6uvw, [6]=3v^2w,
    //   [7]=3uw^2, [8]=3vw^2, [9]=w^3.
    //
    // Choose the control points so the patch is a linear (planar) triangle defined
    // by 3 corners, making the expected rendering simple and robust.
    const Vertex corner_u = {(float)kWidth * 0.25f, (float)kHeight * 0.25f, 0.5f, 1.0f, kBlue};
    const Vertex corner_v = {(float)kWidth * 0.75f, (float)kHeight * 0.25f, 0.5f, 1.0f, kBlue};
    const Vertex corner_w = {(float)kWidth * 0.50f, (float)kHeight * 0.75f, 0.5f, 1.0f, kBlue};

    struct TriW {
      int u;
      int v;
      int w;
    };
    const TriW kWeights[10] = {
        {3, 0, 0}, // u^3
        {2, 1, 0}, // u^2 v
        {1, 2, 0}, // u v^2
        {0, 3, 0}, // v^3
        {2, 0, 1}, // u^2 w
        {1, 1, 1}, // u v w
        {0, 2, 1}, // v^2 w
        {1, 0, 2}, // u w^2
        {0, 1, 2}, // v w^2
        {0, 0, 3}, // w^3
    };

    Vertex cp[10];
    for (int i = 0; i < 10; ++i) {
      const float fu = (float)kWeights[i].u / 3.0f;
      const float fv = (float)kWeights[i].v / 3.0f;
      const float fw = (float)kWeights[i].w / 3.0f;
      cp[i].x = corner_u.x * fu + corner_v.x * fv + corner_w.x * fw;
      cp[i].y = corner_u.y * fu + corner_v.y * fv + corner_w.y * fw;
      cp[i].z = 0.5f;
      cp[i].rhw = 1.0f;
      cp[i].color = kBlue;
    }

    ComPtr<IDirect3DVertexBuffer9> vb;
    hr = dev->CreateVertexBuffer(sizeof(cp),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_DEFAULT,
                                 vb.put(),
                                 NULL);
    if (FAILED(hr)) {
      // Fallback (some runtimes may reject DEFAULT pool allocations in constrained modes).
      hr = dev->CreateVertexBuffer(sizeof(cp),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                   D3DPOOL_SYSTEMMEM,
                                   vb.put(),
                                   NULL);
    }
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateVertexBuffer", hr);
    }

    void* vb_ptr = NULL;
    hr = vb->Lock(0, sizeof(cp), &vb_ptr, 0);
    if (FAILED(hr) || !vb_ptr) {
      return reporter.FailHresult("IDirect3DVertexBuffer9::Lock", FAILED(hr) ? hr : E_FAIL);
    }
    memcpy(vb_ptr, cp, sizeof(cp));
    vb->Unlock();

    hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(Vertex));
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::SetStreamSource", hr);
    }
    hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
    }

    // Create + draw a simple linear tri patch.
    D3DPATCHHANDLE patch = 0;
    float segs[3] = {2.0f, 2.0f, 2.0f};
    D3DTRIPATCH_INFO pinfo;
    ZeroMemory(&pinfo, sizeof(pinfo));
    pinfo.StartVertexOffset = 0;
    pinfo.NumVertices = 10;
    pinfo.Basis = D3DBASIS_BEZIER;
    pinfo.Degree = D3DDEGREE_CUBIC;

    hr = dev->CreateTriPatch(&patch, segs, &pinfo);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::CreateTriPatch", hr);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
    }

    hr = dev->DrawTriPatch(patch, segs, &pinfo);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::DrawTriPatch", hr);
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
    }

    // Read back before Present: with DISCARD swap effects, contents after Present are undefined.
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

    const uint32_t expected = 0xFF0000FFu;        // BGRA blue.
    const uint32_t expected_corner = 0xFFFF0000u; // BGRA red.
    if ((center & 0x00FFFFFFu) != (expected & 0x00FFFFFFu) ||
        (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
      if (dump) {
        std::string err;
        const std::wstring bmp_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_patch_sanity.bmp");
        if (aerogpu_test::WriteBmp32BGRA(bmp_path,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         lr.pBits,
                                         (int)lr.Pitch,
                                         &err)) {
          reporter.AddArtifactPathW(bmp_path);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
        }
        DumpTightBgra32(kTestName,
                        &reporter,
                        L"d3d9_patch_sanity.bin",
                        lr.pBits,
                        (int)lr.Pitch,
                        (int)desc.Width,
                        (int)desc.Height);
      }
      sysmem->UnlockRect();
      return reporter.Fail("pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                           (unsigned long)center,
                           (unsigned long)expected,
                           (unsigned long)corner,
                           (unsigned long)expected_corner);
    }

    sysmem->UnlockRect();

    hr = dev->DeletePatch(patch);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::DeletePatch", hr);
    }

    ran_any = true;
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: RTPATCHES caps not advertised; skipping tri patch", kTestName);
  }

  if (caps_rect) {
    // Create + draw a simple cubic rect patch.
    //
    // NOTE: Some D3D9 header vintages use a different D3DRECTPATCH_INFO layout;
    // FillRectPatchInfo() handles the known variants.
    const DWORD kGreen = D3DCOLOR_XRGB(0, 255, 0);

    // Build a planar 4x4 control point grid so the cubic Bezier surface evaluates
    // to a rectangle in screen space.
    const float x0 = (float)kWidth * 0.25f;
    const float x1 = (float)kWidth * 0.75f;
    const float y0 = (float)kHeight * 0.25f;
    const float y1 = (float)kHeight * 0.75f;

    Vertex rect_cp[16];
    for (int j = 0; j < 4; ++j) {
      const float v = (float)j / 3.0f;
      const float y = y0 + (y1 - y0) * v;
      for (int i = 0; i < 4; ++i) {
        const float u = (float)i / 3.0f;
        const float x = x0 + (x1 - x0) * u;
        Vertex vert;
        vert.x = x;
        vert.y = y;
        vert.z = 0.5f;
        vert.rhw = 1.0f;
        vert.color = kGreen;
        rect_cp[j * 4 + i] = vert;
      }
    }

    ComPtr<IDirect3DVertexBuffer9> vb_rect;
    hr = dev->CreateVertexBuffer(sizeof(rect_cp),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_DEFAULT,
                                 vb_rect.put(),
                                 NULL);
    if (FAILED(hr)) {
      hr = dev->CreateVertexBuffer(sizeof(rect_cp),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                   D3DPOOL_SYSTEMMEM,
                                   vb_rect.put(),
                                   NULL);
    }
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateVertexBuffer(rect patch)", hr);
    }

    void* vb_rect_ptr = NULL;
    hr = vb_rect->Lock(0, sizeof(rect_cp), &vb_rect_ptr, 0);
    if (FAILED(hr) || !vb_rect_ptr) {
      return reporter.FailHresult("IDirect3DVertexBuffer9::Lock(rect patch)", FAILED(hr) ? hr : E_FAIL);
    }
    memcpy(vb_rect_ptr, rect_cp, sizeof(rect_cp));
    vb_rect->Unlock();

    hr = dev->SetStreamSource(0, vb_rect.get(), 0, sizeof(Vertex));
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::SetStreamSource(rect patch)", hr);
    }
    hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF(rect patch)", hr);
    }

    D3DPATCHHANDLE rect_patch = 0;
    float rect_segs[4] = {2.0f, 2.0f, 2.0f, 2.0f};
    D3DRECTPATCH_INFO rinfo;
    if (!FillRectPatchInfo(&rinfo)) {
      aerogpu_test::PrintfStdout("INFO: %s: unknown D3DRECTPATCH_INFO layout (size=%u); skipping rect patch",
                                 kTestName,
                                 (unsigned)sizeof(D3DRECTPATCH_INFO));
      if (!ran_any) {
        reporter.SetSkipped("rect_patch_info_layout_unknown");
      }
      return reporter.Pass();
    }

    hr = dev->CreateRectPatch(&rect_patch, rect_segs, &rinfo);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::CreateRectPatch", hr);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::Clear(rect patch)", hr);
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene(rect patch)", hr);
    }

    hr = dev->DrawRectPatch(rect_patch, rect_segs, &rinfo);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::DrawRectPatch", hr);
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::EndScene(rect patch)", hr);
    }

    // Read back before Present: with DISCARD swap effects, contents after Present are undefined.
    ComPtr<IDirect3DSurface9> backbuffer;
    hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::GetBackBuffer(rect patch)", hr);
    }

    D3DSURFACE_DESC desc;
    ZeroMemory(&desc, sizeof(desc));
    hr = backbuffer->GetDesc(&desc);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DSurface9::GetDesc(rect patch)", hr);
    }

    ComPtr<IDirect3DSurface9> sysmem;
    hr = dev->CreateOffscreenPlainSurface(desc.Width,
                                          desc.Height,
                                          desc.Format,
                                          D3DPOOL_SYSTEMMEM,
                                          sysmem.put(),
                                          NULL);
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateOffscreenPlainSurface(rect patch)", hr);
    }

    hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
    if (FAILED(hr)) {
      return reporter.FailHresult("GetRenderTargetData(rect patch)", hr);
    }

    D3DLOCKED_RECT lr;
    ZeroMemory(&lr, sizeof(lr));
    hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DSurface9::LockRect(rect patch)", hr);
    }

    const int cx = (int)desc.Width / 2;
    const int cy = (int)desc.Height / 2;
    const uint32_t center2 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
    const uint32_t corner2 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);
    const uint32_t expected2 = 0xFF00FF00u;        // BGRA green.
    const uint32_t expected_corner2 = 0xFFFF0000u; // BGRA red.
    if ((center2 & 0x00FFFFFFu) != (expected2 & 0x00FFFFFFu) ||
        (corner2 & 0x00FFFFFFu) != (expected_corner2 & 0x00FFFFFFu)) {
      if (dump) {
        std::string err;
        const std::wstring bmp_path =
            aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_patch_sanity_rect.bmp");
        if (aerogpu_test::WriteBmp32BGRA(bmp_path,
                                         (int)desc.Width,
                                         (int)desc.Height,
                                         lr.pBits,
                                         (int)lr.Pitch,
                                         &err)) {
          reporter.AddArtifactPathW(bmp_path);
        } else {
          aerogpu_test::PrintfStdout("INFO: %s: rect BMP dump failed: %s", kTestName, err.c_str());
        }
      }
      sysmem->UnlockRect();
      return reporter.Fail(
          "rect patch pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
          (unsigned long)center2,
          (unsigned long)expected2,
          (unsigned long)corner2,
          (unsigned long)expected_corner2);
    }

    sysmem->UnlockRect();

    hr = dev->DeletePatch(rect_patch);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::DeletePatch(rect)", hr);
    }

    ran_any = true;
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: RTPATCHES caps not advertised; skipping rect patch", kTestName);
  }

  if (!ran_any) {
    reporter.SetSkipped("patch_caps_present_but_no_subtest_ran");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9PatchSanity(argc, argv);
}
