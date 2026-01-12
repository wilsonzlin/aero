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

static void PumpMessages() {
  MSG msg;
  while (PeekMessage(&msg, NULL, 0, 0, PM_REMOVE)) {
    TranslateMessage(&msg);
    DispatchMessage(&msg);
  }
}

static int RunD3D9ExMultiframeTriangle(int argc, char** argv) {
  const char* kTestName = "d3d9ex_multiframe_triangle";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--frames=N] [--require-vid=0x####] "
        "[--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

  uint32_t frames = 20;
  aerogpu_test::GetArgUint32(argc, argv, "--frames", &frames);
  if (frames < 2) {
    frames = 2;
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExMultiframeTriangle",
                                              L"AeroGPU D3D9Ex Multiframe Triangle",
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
          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
          "Install AeroGPU driver or pass --allow-microsoft.",
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
      return reporter.Fail("adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                           "or use --require-vid/--require-did)",
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

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
  }

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  // Use non-symmetric vertex colors so we catch D3DCOLOR channel-ordering regressions
  // (e.g. BGRA-in-memory vs RGBA-in-shader).
  const DWORD kBlue = D3DCOLOR_XRGB(0, 0, 255);
  const DWORD kYellow = D3DCOLOR_XRGB(255, 255, 0);

  // Create a dynamic vertex buffer once and update it each frame.
  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(sizeof(Vertex) * 3,
                               D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_DEFAULT,
                               vb.put(),
                               NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer", hr);
  }

  hr = dev->SetStreamSource(0, vb.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }

  // Reuse the backbuffer + system-memory surface for readback.
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

  uint32_t first_center = 0;
  uint32_t second_center = 0;
  const std::wstring dump_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_multiframe_triangle.bmp");

  for (uint32_t frame = 0; frame < frames; ++frame) {
    PumpMessages();

    // Alternate between blue and yellow; both have R != B so they are sensitive to BGRA<->RGBA
    // channel swaps.
    const DWORD tri_color = (frame & 1) ? kYellow : kBlue;

    // Update VB contents.
    void* data = NULL;
    hr = vb->Lock(0, sizeof(Vertex) * 3, &data, D3DLOCK_DISCARD);
    if (FAILED(hr) || !data) {
      return reporter.FailHresult("IDirect3DVertexBuffer9::Lock", hr);
    }
    Vertex* verts = reinterpret_cast<Vertex*>(data);
    verts[0].x = (float)kWidth * 0.25f;
    verts[0].y = (float)kHeight * 0.25f;
    verts[0].z = 0.5f;
    verts[0].rhw = 1.0f;
    verts[0].color = tri_color;
    verts[1].x = (float)kWidth * 0.75f;
    verts[1].y = (float)kHeight * 0.25f;
    verts[1].z = 0.5f;
    verts[1].rhw = 1.0f;
    verts[1].color = tri_color;
    verts[2].x = (float)kWidth * 0.5f;
    verts[2].y = (float)kHeight * 0.75f;
    verts[2].z = 0.5f;
    verts[2].rhw = 1.0f;
    verts[2].color = tri_color;

    vb->Unlock();

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::Clear", hr);
    }

    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
    }

    hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 1);
    if (FAILED(hr)) {
      dev->EndScene();
      return reporter.FailHresult("IDirect3DDevice9Ex::DrawPrimitive", hr);
    }

    hr = dev->EndScene();
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::EndScene", hr);
    }

    // Read back before PresentEx; with D3DSWAPEFFECT_DISCARD the contents after Present are undefined.
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
    sysmem->UnlockRect();

    if (frame == 0) {
      first_center = center;
    } else if (frame == 1) {
      second_center = center;
    }

    hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx", hr);
    }
  }

  // Validate that the center pixel changes across frames (blue on frame 0, yellow on frame 1).
  const uint32_t expected0 = 0xFF0000FFu;  // BGRA = (255, 0, 0, 255) = blue
  const uint32_t expected1 = 0xFFFFFF00u;  // BGRA = (0, 255, 255, 255) = yellow
  if ((first_center & 0x00FFFFFFu) != (expected0 & 0x00FFFFFFu) ||
      (second_center & 0x00FFFFFFu) != (expected1 & 0x00FFFFFFu)) {
    if (dump) {
      // Dump the last-read sysmem surface by re-rendering one more green frame.
      HRESULT dump_hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
      if (SUCCEEDED(dump_hr)) {
        dump_hr = dev->BeginScene();
      }
      if (SUCCEEDED(dump_hr)) {
        dump_hr = dev->DrawPrimitive(D3DPT_TRIANGLELIST, 0, 1);
      }
      if (SUCCEEDED(dump_hr)) {
        dump_hr = dev->EndScene();
      }
      if (SUCCEEDED(dump_hr)) {
        dump_hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
      }
      if (SUCCEEDED(dump_hr)) {
        D3DLOCKED_RECT lr;
        ZeroMemory(&lr, sizeof(lr));
        if (SUCCEEDED(sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY))) {
          std::string err;
          if (aerogpu_test::WriteBmp32BGRA(dump_bmp_path,
                                          (int)desc.Width,
                                          (int)desc.Height,
                                          lr.pBits,
                                          (int)lr.Pitch,
                                          &err)) {
            reporter.AddArtifactPathW(dump_bmp_path);
          } else if (!err.empty()) {
            aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
          }
          DumpTightBgra32(kTestName,
                          &reporter,
                          L"d3d9ex_multiframe_triangle.bin",
                          lr.pBits,
                          (int)lr.Pitch,
                          (int)desc.Width,
                          (int)desc.Height);
          sysmem->UnlockRect();
        }
      }
    }

    return reporter.Fail(
        "pixel mismatch: frame0_center=0x%08lX expected 0x%08lX; frame1_center=0x%08lX expected 0x%08lX",
                          (unsigned long)first_center,
                          (unsigned long)expected0,
                          (unsigned long)second_center,
                          (unsigned long)expected1);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExMultiframeTriangle(argc, argv);
  Sleep(30);
  return rc;
}
