#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  // D3DCREATE_MULTITHREADED makes it easier to probe API calls from helper threads in the
  // future without running afoul of D3D9's thread-affinity rules.
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING |
                       D3DCREATE_NOWINDOWCHANGES |
                       D3DCREATE_MULTITHREADED;
  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING |
                   D3DCREATE_NOWINDOWCHANGES |
                   D3DCREATE_MULTITHREADED;
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

static int RunD3D9ExDwmDdiSanity(int argc, char** argv) {
  const char* kTestName = "d3d9ex_dwm_ddi_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExDwmDdiSanity",
                                              L"AeroGPU D3D9Ex DWM DDI Sanity",
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
  // Prefer vblank paced to match DWM behavior.
  pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;

  ComPtr<IDirect3DDevice9Ex> dev;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
  if (FAILED(hr)) {
    // Remote sessions and unusual display stacks may not support interval-one presents.
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
    if (SUCCEEDED(hr)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: CreateDeviceEx with D3DPRESENT_INTERVAL_ONE failed; using IMMEDIATE present interval",
          kTestName);
    }
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
    return aerogpu_test::FailHresult(
        kTestName,
        "GetAdapterIdentifier (required for --require-vid/--require-did)",
        hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  const double kMaxSingleCallMs = 250.0;

  // --- CheckDeviceState: must be fast and non-fatal (S_OK / S_PRESENT_OCCLUDED) ---
  const int kCheckDeviceStateIters = 200;
  for (int i = 0; i < kCheckDeviceStateIters; ++i) {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->CheckDeviceState(hwnd);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);

    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "CheckDeviceState appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CheckDeviceState", hr);
    }
  }

  // --- PresentEx throttling (max frame latency) ---
  // DWM typically presents without D3DPRESENT_DONOTWAIT; the UMD must throttle by
  // waiting/polling internally, but never hang.
  hr = dev->SetMaximumFrameLatency(1);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::SetMaximumFrameLatency(1)", hr);
  }

  UINT max_frame_latency = 0;
  {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->GetMaximumFrameLatency(&max_frame_latency);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);
    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "GetMaximumFrameLatency appears to block (%.3f ms)", call_ms);
    }
  }
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetMaximumFrameLatency", hr);
  }
  if (max_frame_latency < 1 || max_frame_latency > 16) {
    return aerogpu_test::Fail(kTestName, "GetMaximumFrameLatency returned %u (expected [1,16])", (unsigned)max_frame_latency);
  }

  const int kPresentThrottleIters = 60;
  for (int i = 0; i < kPresentThrottleIters; ++i) {
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(i & 1 ? 0 : 255, 0, 0), 1.0f, 0);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Clear(present throttle)", hr);
    }

    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->PresentEx(NULL, NULL, NULL, NULL, 0);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);

    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "PresentEx appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::PresentEx(throttle)", hr);
    }
  }

  // --- Present statistics: must succeed and remain non-blocking (DWM probes these) ---
  const int kPresentStatsIters = 200;
  UINT last_present_count = 0;
  for (int i = 0; i < kPresentStatsIters; ++i) {
    D3DPRESENTSTATS st;
    ZeroMemory(&st, sizeof(st));

    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->GetPresentStats(&st);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);

    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "GetPresentStats appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetPresentStats", hr);
    }

    QueryPerformanceCounter(&before);
    hr = dev->GetLastPresentCount(&last_present_count);
    QueryPerformanceCounter(&after);
    const double last_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (last_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "GetLastPresentCount appears to block (%.3f ms)", last_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetLastPresentCount", hr);
    }

    if (st.PresentCount < last_present_count) {
      return aerogpu_test::Fail(kTestName,
                                "present stats invalid: PresentCount=%u LastPresentCount=%u",
                                (unsigned)st.PresentCount,
                                (unsigned)last_present_count);
    }
  }

  // --- Display mode query: must succeed and not block ---
  D3DDISPLAYMODEEX mode;
  ZeroMemory(&mode, sizeof(mode));
  mode.Size = sizeof(mode);
  D3DDISPLAYROTATION rotation = D3DDISPLAYROTATION_IDENTITY;
  {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->GetDisplayModeEx(0, &mode, &rotation);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);
    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "GetDisplayModeEx appears to block (%.3f ms)", call_ms);
    }
  }
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetDisplayModeEx", hr);
  }

  // --- WaitForVBlank: must always be bounded (and not hang in remote/non-vblank setups) ---
  const int kWaitForVBlankIters = 10;
  for (int i = 0; i < kWaitForVBlankIters; ++i) {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->WaitForVBlank(0);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);

    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "WaitForVBlank appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::WaitForVBlank", hr);
    }
  }

  // --- GPU thread priority: must accept/clamp values and never block ---
  const int kGpuPriorityIters = 100;
  for (int i = 0; i < kGpuPriorityIters; ++i) {
    const int req = (i & 1) ? 100 : -100;
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->SetGPUThreadPriority(req);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);
    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "SetGPUThreadPriority appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::SetGPUThreadPriority", hr);
    }

    int got = 0;
    QueryPerformanceCounter(&before);
    hr = dev->GetGPUThreadPriority(&got);
    QueryPerformanceCounter(&after);
    const double get_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (get_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "GetGPUThreadPriority appears to block (%.3f ms)", get_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetGPUThreadPriority", hr);
    }
    if (got < -7 || got > 7) {
      return aerogpu_test::Fail(kTestName,
                                "GetGPUThreadPriority returned %d (expected clamped to [-7, 7])",
                                got);
    }
  }

  // --- Residency APIs: must report resources as resident and never block ---
  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(64, 64, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, tex.put(), NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CreateTexture", hr);
  }

  ComPtr<IDirect3DVertexBuffer9> vb;
  hr = dev->CreateVertexBuffer(256, 0, 0, D3DPOOL_DEFAULT, vb.put(), NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CreateVertexBuffer", hr);
  }

  IDirect3DResource9* resources[2] = {tex.get(), vb.get()};
  const UINT resource_count = 2;

  const int kResidencyIters = 200;
  for (int i = 0; i < kResidencyIters; ++i) {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = dev->CheckResourceResidency(resources, resource_count);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);
    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "CheckResourceResidency appears to block (%.3f ms)", call_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CheckResourceResidency", hr);
    }

    D3DRESOURCERESIDENCY status[2];
    status[0] = D3DRESOURCERESIDENCY_EVICTED_TO_DISK;
    status[1] = D3DRESOURCERESIDENCY_EVICTED_TO_DISK;

    QueryPerformanceCounter(&before);
    hr = dev->QueryResourceResidency(resources, resource_count, status);
    QueryPerformanceCounter(&after);
    const double query_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (query_ms > kMaxSingleCallMs) {
      return aerogpu_test::Fail(kTestName, "QueryResourceResidency appears to block (%.3f ms)", query_ms);
    }
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::QueryResourceResidency", hr);
    }

    for (int r = 0; r < 2; ++r) {
      if (status[r] != D3DRESOURCERESIDENCY_FULLY_RESIDENT) {
        return aerogpu_test::Fail(kTestName,
                                  "QueryResourceResidency[%d] returned %d (expected FULLY_RESIDENT=%d)",
                                  r,
                                  (int)status[r],
                                  (int)D3DRESOURCERESIDENCY_FULLY_RESIDENT);
      }
    }
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExDwmDdiSanity(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}
