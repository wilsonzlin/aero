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

  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
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

static int RunD3D9ExQueryLatency(int argc, char** argv) {
  const char* kTestName = "d3d9ex_query_latency";
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExQueryLatency",
                                              L"AeroGPU D3D9Ex Query+Latency",
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
  // Prefer vblank pacing: DWM depends on present throttling and max frame latency interacting with
  // vsync/composition in typical configurations.
  pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;

  ComPtr<IDirect3DDevice9Ex> dev;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
  if (FAILED(hr)) {
    // Some environments (e.g. remote sessions) can have unusual vblank/pacing behavior; fall back to
    // immediate present rather than failing the entire query/latency validation.
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

  const bool remote = GetSystemMetrics(SM_REMOTESESSION) != 0;
  if (remote) {
    // Composition/vblank behavior differs in RDP sessions, but the D3D9Ex query + frame latency APIs
    // are still expected to function.
    aerogpu_test::PrintfStdout("INFO: %s: remote session detected (SM_REMOTESESSION=1)", kTestName);
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  // --- EVENT query completion test ---
  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }

  // Submit a trivial command so there is something for the query to wait behind.
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(10, 20, 30), 1.0f, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Clear", hr);
  }

  hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::Issue(D3DISSUE_END)", hr);
  }

  const double kQueryTimeoutMs = 2000.0;
  // If GetData blocks, it defeats the purpose of D3DQUERYTYPE_EVENT polling (DWM relies on polling).
  // Keep this threshold generous to avoid false positives from scheduling hiccups.
  const double kMaxSingleGetDataCallMs = 250.0;

  LARGE_INTEGER start_qpc;
  QueryPerformanceCounter(&start_qpc);
  DWORD polls = 0;

  while (TRUE) {
    BOOL done = FALSE;

    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    hr = q->GetData(&done, sizeof(done), D3DGETDATA_FLUSH);
    LARGE_INTEGER after;
    QueryPerformanceCounter(&after);

    const double call_ms = QpcToMs(after.QuadPart - before.QuadPart, qpc_freq);
    if (call_ms > kMaxSingleGetDataCallMs) {
      return aerogpu_test::Fail(kTestName,
                                "IDirect3DQuery9::GetData appears to block (%.3f ms)",
                                call_ms);
    }

    ++polls;

    if (hr == S_OK) {
      if (!done) {
        return aerogpu_test::Fail(kTestName, "EVENT query returned S_OK but done==FALSE");
      }
      break;
    }
    if (hr != S_FALSE) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::GetData", hr);
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double elapsed_ms = QpcToMs(now.QuadPart - start_qpc.QuadPart, qpc_freq);
    if (elapsed_ms > kQueryTimeoutMs) {
      return aerogpu_test::Fail(kTestName,
                                "EVENT query did not complete within %.0f ms (polls=%lu)",
                                kQueryTimeoutMs,
                                (unsigned long)polls);
    }

    // Avoid a pure busy-spin in case the driver needs CPU time to make progress.
    Sleep(0);
  }

  LARGE_INTEGER end_qpc;
  QueryPerformanceCounter(&end_qpc);
  aerogpu_test::PrintfStdout("INFO: %s: EVENT query signaled after %lu polls (%.3f ms)",
                             kTestName,
                             (unsigned long)polls,
                             QpcToMs(end_qpc.QuadPart - start_qpc.QuadPart, qpc_freq));

  // --- Max frame latency API test ---
  hr = dev->SetMaximumFrameLatency(1);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::SetMaximumFrameLatency(1)", hr);
  }

  UINT max_frame_latency = 0;
  hr = dev->GetMaximumFrameLatency(&max_frame_latency);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::GetMaximumFrameLatency", hr);
  }

  // D3D9Ex documentation defines valid range as [1, 16].
  if (max_frame_latency < 1) {
    return aerogpu_test::Fail(kTestName,
                              "GetMaximumFrameLatency returned %u (expected >= 1)",
                              (unsigned)max_frame_latency);
  }
  if (max_frame_latency != 1) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: SetMaximumFrameLatency(1) reported %u (clamped?)",
        kTestName,
        (unsigned)max_frame_latency);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: maximum frame latency set to %u",
                               kTestName,
                               (unsigned)max_frame_latency);
  }

  // Best-effort throttle check: PresentEx(DONOTWAIT) should return D3DERR_WASSTILLDRAWING at least
  // occasionally when vblank pacing is active and max frame latency is low.
  //
  // Do not hard-fail on the absence of WASSTILLDRAWING: composition/vblank can be disabled (e.g. RDP),
  // and some present paths can be effectively immediate.
  const int kPresentIters = 200;
  int present_ok = 0;
  int present_still_drawing = 0;
  for (int i = 0; i < kPresentIters; ++i) {
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, i & 1 ? 0 : 255), 1.0f, 0);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Clear(present loop)", hr);
    }

    hr = dev->PresentEx(NULL, NULL, NULL, NULL, D3DPRESENT_DONOTWAIT);
    if (hr == D3D_OK) {
      ++present_ok;
    } else if (hr == D3DERR_WASSTILLDRAWING) {
      ++present_still_drawing;
    } else {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::PresentEx(DONOTWAIT)", hr);
    }
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: PresentEx(DONOTWAIT) stats: ok=%d stillDrawing=%d (iters=%d)",
      kTestName,
      present_ok,
      present_still_drawing,
      kPresentIters);
  if (present_still_drawing == 0) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: no D3DERR_WASSTILLDRAWING observed (best-effort check; composition/vblank may be unavailable)",
        kTestName);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  int rc = RunD3D9ExQueryLatency(argc, argv);
  // Give the window a moment to appear for manual observation when running interactively.
  Sleep(30);
  return rc;
}
