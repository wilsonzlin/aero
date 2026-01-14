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

static const UINT kSrcVertexCount = 5;
static const UINT kDestVertexCount = 9;
static const UINT kSrcStartIndex = 1;
static const UINT kDestIndex = 3;
static const UINT kProcessVertexCount = 3;

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         DWORD create_flags,
                                         IDirect3DDevice9Ex** out_dev,
                                         bool* out_used_software_vertex_processing,
                                         HRESULT* out_hw_create_hr) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  if (out_used_software_vertex_processing) {
    *out_used_software_vertex_processing = false;
  }

  const HRESULT hw_hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                            D3DDEVTYPE_HAL,
                                            hwnd,
                                            create_flags,
                                            pp,
                                            NULL,
                                            out_dev);
  if (out_hw_create_hr) {
    *out_hw_create_hr = hw_hr;
  }
  if (SUCCEEDED(hw_hr)) {
    return hw_hr;
  }

  DWORD fallback_flags = create_flags;
  fallback_flags &= ~D3DCREATE_HARDWARE_VERTEXPROCESSING;
  fallback_flags |= D3DCREATE_SOFTWARE_VERTEXPROCESSING;
  const HRESULT sw_hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                            D3DDEVTYPE_HAL,
                                            hwnd,
                                            fallback_flags,
                                            pp,
                                            NULL,
                                            out_dev);
  if (SUCCEEDED(sw_hr) && out_used_software_vertex_processing) {
    *out_used_software_vertex_processing = true;
  }
  return sw_hr;
}

static int RunD3D9ProcessVerticesSmoke(int argc, char** argv) {
  const char* kTestName = "d3d9_process_vertices_smoke";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--show] [--show-window] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--allow-remote]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Creates a D3D9Ex device, uses IDirect3DDevice9::ProcessVertices to copy/transform vertices into a "
        "destination vertex buffer (with non-zero SrcStartIndex/DestIndex), then draws from the processed buffer "
        "and validates pixels via GetRenderTargetData.");
    aerogpu_test::PrintfStdout("Default: window is shown (pass --hidden to hide it; --show overrides --hidden).");
    aerogpu_test::PrintfStdout("With --dump: writes d3d9_process_vertices_smoke.bmp and d3d9_process_vertices_smoke.bin.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  if (aerogpu_test::HasArg(argc, argv, "--show") || aerogpu_test::HasArg(argc, argv, "--show-window")) {
    hidden = false;
  }
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");

  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ProcessVerticesSmoke",
                                              L"AeroGPU D3D9 ProcessVertices Smoke",
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
  bool used_software_vp = false;
  HRESULT hw_create_hr = S_OK;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put(), &used_software_vp, &hw_create_hr);
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

  // This test is specifically meant to validate the ProcessVertices DDI path. If we ended up with
  // software vertex processing, the runtime may execute parts of the vertex processing on the CPU,
  // which can mask driver-side ProcessVertices regressions (silent no-ops / memory corruption).
  if (used_software_vp || dev->GetSoftwareVertexProcessing()) {
    if (used_software_vp) {
      return reporter.Fail(
          "CreateDeviceEx(HWVP) failed with %s; fell back to software vertex processing. "
          "This can mask driver-side ProcessVertices regressions; cannot validate DDI.",
          aerogpu_test::HresultToString(hw_create_hr).c_str());
    }
    return reporter.Fail(
        "device is using software vertex processing; expected hardware vertex processing for ProcessVertices validation");
  }

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kBlue = D3DCOLOR_XRGB(0, 0, 255);
  const DWORD kGreen = D3DCOLOR_XRGB(0, 255, 0);
  const DWORD kYellow = D3DCOLOR_XRGB(255, 255, 0);

  // Source VB includes dummy vertices at indices 0..1 so that:
  //   - ignoring SrcStartIndex, or
  //   - ignoring SetStreamSource's non-zero offset
  // produces a triangle entirely outside the viewport (center pixel remains the clear color).
  Vertex src_verts[kSrcVertexCount];
  src_verts[0].x = 0.0f;
  src_verts[0].y = -1000.0f;
  src_verts[0].z = 0.5f;
  src_verts[0].rhw = 1.0f;
  src_verts[0].color = kGreen;

  src_verts[1].x = 1000.0f;
  src_verts[1].y = -1000.0f;
  src_verts[1].z = 0.5f;
  src_verts[1].rhw = 1.0f;
  src_verts[1].color = kGreen;

  // Triangle that covers the center pixel while leaving the top-left corner untouched.
  src_verts[2].x = (float)kWidth * 0.25f;
  src_verts[2].y = (float)kHeight * 0.25f;
  src_verts[2].z = 0.5f;
  src_verts[2].rhw = 1.0f;
  src_verts[2].color = kBlue;

  src_verts[3].x = (float)kWidth * 0.75f;
  src_verts[3].y = (float)kHeight * 0.25f;
  src_verts[3].z = 0.5f;
  src_verts[3].rhw = 1.0f;
  src_verts[3].color = kBlue;

  src_verts[4].x = (float)kWidth * 0.5f;
  src_verts[4].y = (float)kHeight * 0.75f;
  src_verts[4].z = 0.5f;
  src_verts[4].rhw = 1.0f;
  src_verts[4].color = kBlue;

  ComPtr<IDirect3DVertexBuffer9> vb_src;
  hr = dev->CreateVertexBuffer(sizeof(src_verts),
                               D3DUSAGE_WRITEONLY,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_DEFAULT,
                               vb_src.put(),
                               NULL);
  if (FAILED(hr) || !vb_src) {
    return reporter.FailHresult("CreateVertexBuffer(src)", hr);
  }

  void* vb_ptr = NULL;
  hr = vb_src->Lock(0, sizeof(src_verts), &vb_ptr, 0);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Lock(src)", hr);
  }
  memcpy(vb_ptr, src_verts, sizeof(src_verts));
  hr = vb_src->Unlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Unlock(src)", hr);
  }

  ComPtr<IDirect3DVertexBuffer9> vb_dst;
  hr = dev->CreateVertexBuffer(sizeof(Vertex) * kDestVertexCount,
                               D3DUSAGE_WRITEONLY,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_DEFAULT,
                               vb_dst.put(),
                               NULL);
  if (FAILED(hr) || !vb_dst) {
    return reporter.FailHresult("CreateVertexBuffer(dst)", hr);
  }

  // Initialize the destination VB to sentinel off-screen verts. If ProcessVertices silently does
  // nothing, DrawPrimitive will render nothing and the center pixel will remain red.
  Vertex dst_init[kDestVertexCount];
  // Indices [0..2] form a small on-screen sentinel triangle (green) so we can detect bugs where
  // ProcessVertices ignores DestIndex and overwrites the start of the buffer.
  dst_init[0].x = 20.0f;
  dst_init[0].y = 20.0f;
  dst_init[0].z = 0.5f;
  dst_init[0].rhw = 1.0f;
  dst_init[0].color = kGreen;

  dst_init[1].x = 60.0f;
  dst_init[1].y = 20.0f;
  dst_init[1].z = 0.5f;
  dst_init[1].rhw = 1.0f;
  dst_init[1].color = kGreen;

  dst_init[2].x = 20.0f;
  dst_init[2].y = 60.0f;
  dst_init[2].z = 0.5f;
  dst_init[2].rhw = 1.0f;
  dst_init[2].color = kGreen;

  // Indices [3..5] are off-screen sentinels; a no-op ProcessVertices should leave these untouched
  // so the "processed" draw renders nothing (center stays red).
  for (UINT i = 3; i < 6; ++i) {
    dst_init[i].x = 0.0f;
    dst_init[i].y = -1000.0f;
    dst_init[i].z = 0.5f;
    dst_init[i].rhw = 1.0f;
    dst_init[i].color = kGreen;
  }

  // Indices [6..8] form another on-screen sentinel triangle (yellow). This catches buffer overrun
  // bugs where ProcessVertices writes beyond VertexCount and clobbers subsequent vertices.
  dst_init[6].x = (float)kWidth - 20.0f;
  dst_init[6].y = 20.0f;
  dst_init[6].z = 0.5f;
  dst_init[6].rhw = 1.0f;
  dst_init[6].color = kYellow;

  dst_init[7].x = (float)kWidth - 60.0f;
  dst_init[7].y = 20.0f;
  dst_init[7].z = 0.5f;
  dst_init[7].rhw = 1.0f;
  dst_init[7].color = kYellow;

  dst_init[8].x = (float)kWidth - 20.0f;
  dst_init[8].y = 60.0f;
  dst_init[8].z = 0.5f;
  dst_init[8].rhw = 1.0f;
  dst_init[8].color = kYellow;
  vb_ptr = NULL;
  hr = vb_dst->Lock(0, sizeof(dst_init), &vb_ptr, 0);
  if (FAILED(hr) || !vb_ptr) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Lock(dst)", hr);
  }
  memcpy(vb_ptr, dst_init, sizeof(dst_init));
  hr = vb_dst->Unlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DVertexBuffer9::Unlock(dst)", hr);
  }

  // Output declaration matching our fixed-function Vertex layout.
  D3DVERTEXELEMENT9 out_elems[] = {
      {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITIONT, 0},
      {0, 16, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
      D3DDECL_END()};

  ComPtr<IDirect3DVertexDeclaration9> out_decl;
  hr = dev->CreateVertexDeclaration(out_elems, out_decl.put());
  if (FAILED(hr) || !out_decl) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  // Use a non-zero stream offset to exercise stream offset handling in the ProcessVertices path.
  hr = dev->SetStreamSource(0, vb_src.get(), sizeof(Vertex), sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetStreamSource(src)", hr);
  }

  // Critical requirement: exercise non-zero SrcStartIndex and non-zero DestIndex.
  hr = dev->ProcessVertices(/*SrcStartIndex=*/kSrcStartIndex,
                            /*DestIndex=*/kDestIndex,
                            /*VertexCount=*/kProcessVertexCount,
                            vb_dst.get(),
                            out_decl.get(),
                            /*Flags=*/0);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::ProcessVertices", hr);
  }

  hr = dev->SetStreamSource(0, vb_dst.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::SetStreamSource(dst)", hr);
  }

  // Draw the sentinel triangle first (should remain green if DestIndex is honored).
  hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitive(sentinel)", hr);
  }

  // Draw a second sentinel triangle (should remain yellow if ProcessVertices doesn't overwrite out
  // of bounds).
  hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 6, 1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitive(sentinel2)", hr);
  }

  // Draw the processed vertices from DestIndex (non-zero).
  hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, kDestIndex, 1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitive", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
  }

  // Read back the backbuffer. Do this before PresentEx: with D3DSWAPEFFECT_DISCARD the contents
  // after Present are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr) || !backbuffer) {
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
  if (FAILED(hr) || !sysmem) {
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
  const uint32_t sentinel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 30, 30);
  const uint32_t sentinel2 = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, kWidth - 30, 30);

  if (dump) {
    std::string err;
    const std::wstring bmp_path =
        aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9_process_vertices_smoke.bmp");
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path, (int)desc.Width, (int)desc.Height, lr.pBits, (int)lr.Pitch, &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(bmp_path);
    }
    DumpTightBgra32(kTestName,
                    &reporter,
                    L"d3d9_process_vertices_smoke.bin",
                    lr.pBits,
                    (int)lr.Pitch,
                    (int)desc.Width,
                    (int)desc.Height);
  }

  sysmem->UnlockRect();

  const uint32_t expected_center = 0xFF0000FFu;  // BGRA = blue.
  const uint32_t expected_corner = 0xFFFF0000u;  // BGRA = red clear.
  const uint32_t expected_sentinel = 0xFF00FF00u;  // BGRA = green sentinel.
  const uint32_t expected_sentinel2 = 0xFFFFFF00u;  // BGRA = yellow sentinel.
  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu) ||
      (sentinel & 0x00FFFFFFu) != (expected_sentinel & 0x00FFFFFFu) ||
      (sentinel2 & 0x00FFFFFFu) != (expected_sentinel2 & 0x00FFFFFFu)) {
    return reporter.Fail(
        "pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX; sentinel(30,30)=0x%08lX expected 0x%08lX; sentinel2(%d,30)=0x%08lX expected 0x%08lX",
        (unsigned long)center,
        (unsigned long)expected_center,
        (unsigned long)corner,
        (unsigned long)expected_corner,
        (unsigned long)sentinel,
        (unsigned long)expected_sentinel,
        (int)kWidth - 30,
        (unsigned long)sentinel2,
        (unsigned long)expected_sentinel2);
  }

  hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ProcessVerticesSmoke(argc, argv);
  Sleep(30);
  return rc;
}
