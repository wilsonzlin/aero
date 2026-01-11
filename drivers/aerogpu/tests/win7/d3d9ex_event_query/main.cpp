#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static int FailFast(const char* test_name, const char* fmt, ...) {
  printf("FAIL: %s: ", test_name);
  va_list ap;
  va_start(ap, fmt);
  vprintf(fmt, ap);
  va_end(ap);
  printf("\n");
  fflush(stdout);
  ExitProcess(1);
  return 1;
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

class GetDataRunner {
 public:
  GetDataRunner() : request_event_(NULL), done_event_(NULL), thread_(NULL), stop_(0), query_(NULL) {
    ZeroMemory(&res_, sizeof(res_));
  }

  ~GetDataRunner() { Stop(); }

  bool Start() {
    request_event_ = CreateEventW(NULL, FALSE, FALSE, NULL);
    done_event_ = CreateEventW(NULL, FALSE, FALSE, NULL);
    if (!request_event_ || !done_event_) {
      return false;
    }
    thread_ = CreateThread(NULL, 0, ThreadMain, this, 0, NULL);
    if (thread_) {
      // Reduce the chance of false-positive "blocked" timings due to thread scheduling jitter.
      // The test is short-lived and mostly sleeping, so this should not materially impact the system.
      SetThreadPriority(thread_, THREAD_PRIORITY_HIGHEST);
    }
    return thread_ != NULL;
  }

  void Stop() {
    if (!thread_) {
      if (request_event_) {
        CloseHandle(request_event_);
        request_event_ = NULL;
      }
      if (done_event_) {
        CloseHandle(done_event_);
        done_event_ = NULL;
      }
      return;
    }

    InterlockedExchange(&stop_, 1);
    SetEvent(request_event_);
    WaitForSingleObject(thread_, 5000);

    CloseHandle(thread_);
    thread_ = NULL;

    if (request_event_) {
      CloseHandle(request_event_);
      request_event_ = NULL;
    }
    if (done_event_) {
      CloseHandle(done_event_);
      done_event_ = NULL;
    }
  }

  bool GetData(IDirect3DQuery9* query,
               void* data,
               DWORD size,
               DWORD flags,
               DWORD timeout_ms,
               HRESULT* out_hr,
               LONGLONG* out_start_qpc,
               LONGLONG* out_end_qpc) {
    if (!thread_ || !request_event_ || !done_event_) {
      return false;
    }

    query_ = query;
    data_ = data;
    size_ = size;
    flags_ = flags;
    ZeroMemory(&res_, sizeof(res_));

    SetEvent(request_event_);
    DWORD w = WaitForSingleObject(done_event_, timeout_ms);
    if (w != WAIT_OBJECT_0) {
      return false;
    }

    if (out_hr) {
      *out_hr = res_.hr;
    }
    if (out_start_qpc) {
      *out_start_qpc = res_.start_qpc;
    }
    if (out_end_qpc) {
      *out_end_qpc = res_.end_qpc;
    }
    return true;
  }

 private:
  struct Result {
    HRESULT hr;
    LONGLONG start_qpc;
    LONGLONG end_qpc;
  };

  static DWORD WINAPI ThreadMain(LPVOID param) {
    GetDataRunner* self = (GetDataRunner*)param;
    self->Run();
    return 0;
  }

  void Run() {
    for (;;) {
      DWORD w = WaitForSingleObject(request_event_, INFINITE);
      if (w != WAIT_OBJECT_0) {
        return;
      }
      if (InterlockedCompareExchange(&stop_, 0, 0)) {
        return;
      }

      Result local;
      ZeroMemory(&local, sizeof(local));
      local.hr = E_FAIL;

      IDirect3DQuery9* query = query_;
      if (query) {
        query->AddRef();
      }

      LARGE_INTEGER a;
      LARGE_INTEGER b;
      QueryPerformanceCounter(&a);
      if (query) {
        local.hr = query->GetData(data_, size_, flags_);
      } else {
        local.hr = E_POINTER;
      }
      QueryPerformanceCounter(&b);
      local.start_qpc = a.QuadPart;
      local.end_qpc = b.QuadPart;

      if (query) {
        query->Release();
      }

      res_ = local;
      SetEvent(done_event_);
    }
  }

  HANDLE request_event_;
  HANDLE done_event_;
  HANDLE thread_;
  volatile LONG stop_;

  IDirect3DQuery9* query_;
  void* data_;
  DWORD size_;
  DWORD flags_;
  Result res_;
};

namespace {

struct StressWorkerParams {
  int index;
  int iterations;
  bool show_window;
  HANDLE start_event;
  volatile LONG* any_failed;
  volatile LONG* saw_was_still_drawing;
  bool allow_microsoft;
  bool allow_non_aerogpu;
  bool require_umd;
  bool has_require_vid;
  bool has_require_did;
  uint32_t require_vid;
  uint32_t require_did;
};

static DWORD WINAPI StressWorkerThreadProc(void* userdata) {
  StressWorkerParams* p = (StressWorkerParams*)userdata;
  if (!p) {
    return 1;
  }

  const wchar_t* kClassName = (p->index == 0) ? L"AeroGPU_D3D9ExEventQuery_0" : L"AeroGPU_D3D9ExEventQuery_1";
  const wchar_t* kTitle = (p->index == 0) ? L"AeroGPU D3D9Ex EventQuery 0" : L"AeroGPU D3D9Ex EventQuery 1";

  HWND hwnd = aerogpu_test::CreateBasicWindow(kClassName, kTitle, 128, 128, p->show_window);
  if (!hwnd) {
    InterlockedExchange(p->any_failed, 1);
    return 1;
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    InterlockedExchange(p->any_failed, 1);
    return 1;
  }

  // Basic adapter sanity check to avoid false PASS when AeroGPU isn't active.
  {
    D3DADAPTER_IDENTIFIER9 ident;
    ZeroMemory(&ident, sizeof(ident));
    hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
    if (SUCCEEDED(hr)) {
      aerogpu_test::PrintfStdout("INFO: d3d9ex_event_query: stress[%d]: adapter: %s (VID=0x%04X DID=0x%04X)",
                                 p->index,
                                 ident.Description,
                                 (unsigned)ident.VendorId,
                                 (unsigned)ident.DeviceId);
      if (!p->allow_microsoft && ident.VendorId == 0x1414) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      if (p->has_require_vid && ident.VendorId != p->require_vid) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      if (p->has_require_did && ident.DeviceId != p->require_did) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      if (!p->allow_non_aerogpu && !p->has_require_vid && !p->has_require_did &&
          !(ident.VendorId == 0x1414 && p->allow_microsoft) &&
          !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
    } else if (p->has_require_vid || p->has_require_did) {
      InterlockedExchange(p->any_failed, 1);
      return 1;
    }
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = 128;
  pp.BackBufferHeight = 128;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  // Vsync makes it easy to hit the frame-latency limit and exercise DONOTWAIT.
  pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr)) {
    // Some environments (e.g. remote sessions) can have unusual vblank/pacing behavior; fall back to
    // immediate present rather than failing the entire stress phase.
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  }
  if (FAILED(hr)) {
    InterlockedExchange(p->any_failed, 1);
    return 1;
  }

  if (p->require_umd || (!p->allow_microsoft && !p->allow_non_aerogpu)) {
    if (aerogpu_test::RequireAeroGpuD3D9UmdLoaded("d3d9ex_event_query") != 0) {
      InterlockedExchange(p->any_failed, 1);
      return 1;
    }
  }

  hr = dev->SetMaximumFrameLatency(1);
  if (FAILED(hr)) {
    InterlockedExchange(p->any_failed, 1);
    return 1;
  }

  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr)) {
    InterlockedExchange(p->any_failed, 1);
    return 1;
  }

  WaitForSingleObject(p->start_event, INFINITE);

  for (int i = 0; i < p->iterations; ++i) {
    if (InterlockedCompareExchange(p->any_failed, 0, 0) != 0) {
      return 1;
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB((p->index * 64 + i) & 255, 0, 0), 1.0f, 0);
    if (FAILED(hr)) {
      InterlockedExchange(p->any_failed, 1);
      return 1;
    }

    hr = dev->BeginScene();
    if (SUCCEEDED(hr)) {
      dev->EndScene();
    }

    hr = q->Issue(D3DISSUE_END);
    if (FAILED(hr)) {
      InterlockedExchange(p->any_failed, 1);
      return 1;
    }

    // Encourage the other thread to submit between Issue and GetData to stress
    // per-submission fence tracking.
    Sleep(0);

    DWORD done = 0;
    DWORD start = GetTickCount();
    for (;;) {
      hr = q->GetData(&done, sizeof(done), D3DGETDATA_FLUSH);
      if (hr == S_OK) {
        break;
      }
      if (hr != S_FALSE && hr != D3DERR_WASSTILLDRAWING) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      if (GetTickCount() - start > 5000) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      Sleep(0);
    }

    // Present with DONOTWAIT; if we hit the frame-latency limit, we should get
    // D3DERR_WASSTILLDRAWING and then eventually make progress once prior work
    // completes. This must be tracked per-device (other devices/processes
    // should not interfere).
    start = GetTickCount();
    for (;;) {
      hr = dev->PresentEx(NULL, NULL, NULL, NULL, D3DPRESENT_DONOTWAIT);
      if (hr == S_OK) {
        break;
      }
      if (hr == D3DERR_WASSTILLDRAWING) {
        InterlockedExchange(p->saw_was_still_drawing, 1);
      } else {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      if (GetTickCount() - start > 5000) {
        InterlockedExchange(p->any_failed, 1);
        return 1;
      }
      Sleep(0);
    }
  }

  return 0;
}

} // namespace

static int RunD3D9ExEventQuery(int argc, char** argv) {
  const char* kTestName = "d3d9ex_event_query";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--show] [--show-window] [--hidden] [--iterations=N] [--stress-iterations=N] [--process-stress] "
        "[--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    aerogpu_test::PrintfStdout("Default: window is hidden (pass --show to display it).");
    return 0;
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  // Hide the window by default since this is a synchronization microtest. Use --show/--show-window
  // when debugging interactively.
  bool hidden = true;
  if (aerogpu_test::HasArg(argc, argv, "--hidden")) {
    hidden = true;
  }
  const bool show_window =
      aerogpu_test::HasArg(argc, argv, "--show-window") || aerogpu_test::HasArg(argc, argv, "--show");
  if (show_window) {
    hidden = false;
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

  uint32_t iterations = 6;
  std::string iterations_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--iterations", &iterations_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(iterations_str, &iterations, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --iterations: %s", err.c_str());
    }
  }
  if (iterations < 3) {
    iterations = 3;
  }
  if (iterations > 64) {
    iterations = 64;
  }

  uint32_t stress_iterations = 200;
  std::string stress_iterations_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--stress-iterations", &stress_iterations_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(stress_iterations_str, &stress_iterations, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --stress-iterations: %s", err.c_str());
    }
  }
  if (stress_iterations < 10) {
    stress_iterations = 10;
  }
  if (stress_iterations > 2000) {
    stress_iterations = 2000;
  }

  const bool child_stress = aerogpu_test::HasArg(argc, argv, "--child-stress");
  const bool process_stress = aerogpu_test::HasArg(argc, argv, "--process-stress");

  if (child_stress) {
    uint32_t child_index = 0;
    (void)aerogpu_test::GetArgUint32(argc, argv, "--child-index", &child_index);
    if (child_index > 1) {
      return aerogpu_test::Fail(kTestName, "invalid --child-index=%u (expected 0 or 1)", (unsigned)child_index);
    }

    std::string start_event_str;
    if (!aerogpu_test::GetArgValue(argc, argv, "--start-event", &start_event_str) || start_event_str.empty()) {
      return aerogpu_test::Fail(kTestName, "missing --start-event for --child-stress");
    }

    const std::wstring start_event_w(start_event_str.begin(), start_event_str.end());
    HANDLE start_event = OpenEventW(SYNCHRONIZE, FALSE, start_event_w.c_str());
    if (!start_event) {
      return aerogpu_test::Fail(kTestName,
                                "OpenEvent(%ls) failed: %s",
                                start_event_w.c_str(),
                                aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }

    volatile LONG any_failed = 0;
    volatile LONG saw_was_still_drawing = 0;
    StressWorkerParams params;
    ZeroMemory(&params, sizeof(params));
    params.index = (int)child_index;
    params.iterations = (int)stress_iterations;
    params.show_window = show_window && !hidden;
    params.start_event = start_event;
    params.any_failed = &any_failed;
    params.saw_was_still_drawing = &saw_was_still_drawing;
    params.allow_microsoft = allow_microsoft;
    params.allow_non_aerogpu = allow_non_aerogpu;
    params.require_umd = require_umd;
    params.has_require_vid = has_require_vid;
    params.has_require_did = has_require_did;
    params.require_vid = require_vid;
    params.require_did = require_did;

    const DWORD worker_rc = StressWorkerThreadProc(&params);
    CloseHandle(start_event);

    if (worker_rc != 0 || any_failed != 0) {
      return aerogpu_test::Fail(kTestName, "child stress failed (index=%u)", (unsigned)child_index);
    }

    aerogpu_test::PrintfStdout("INFO: %s: child %u: PresentEx(DONOTWAIT) observed WASSTILLDRAWING=%s",
                               kTestName,
                               (unsigned)child_index,
                               saw_was_still_drawing != 0 ? "yes" : "no");
    aerogpu_test::PrintfStdout("PASS: %s", kTestName);
    return 0;
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  const int kWidth = 256;
  const int kHeight = 256;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExEventQuery",
                                              L"AeroGPU D3D9Ex Event Query",
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
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES |
                       D3DCREATE_MULTITHREADED;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
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
        kTestName, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  ComPtr<IDirect3DQuery9> query;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, query.put());
  if (FAILED(hr) || !query) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CreateQuery(EVENT)", hr);
  }

  GetDataRunner getdata;
  if (!getdata.Start()) {
    return aerogpu_test::Fail(kTestName, "GetDataRunner start failed");
  }

  // `D3DGETDATA_DONOTFLUSH` is used by DWM to poll EVENT queries; it must return quickly and
  // must not block waiting for the GPU to finish work.
  const double kMaxGetDataCallMs = 5.0;

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(8, 8, 8), 1.0f, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Clear(warmup)", hr);
  }
  hr = query->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::Issue(END warmup)", hr);
  }

  // First poll must not block and should indicate "not ready" when using DONOTFLUSH.
  {
    HRESULT hr_immediate = E_FAIL;
    LONGLONG start_qpc = 0;
    LONGLONG end_qpc = 0;
    if (!getdata.GetData(query.get(),
                         NULL,
                         0,
                         D3DGETDATA_DONOTFLUSH,
                         200,
                         &hr_immediate,
                         &start_qpc,
                         &end_qpc)) {
      FailFast(kTestName, "GetData(DONOTFLUSH warmup) hung");
    }
    const double call_ms = QpcToMs(end_qpc - start_qpc, qpc_freq);
    if (call_ms > kMaxGetDataCallMs) {
      return aerogpu_test::Fail(kTestName,
                                "GetData(D3DGETDATA_DONOTFLUSH warmup) blocked for %.3fms",
                                call_ms);
    }
    if (hr_immediate != S_FALSE && hr_immediate != D3DERR_WASSTILLDRAWING) {
      if (hr_immediate == S_OK) {
        return aerogpu_test::Fail(kTestName,
                                  "GetData(D3DGETDATA_DONOTFLUSH warmup) returned S_OK immediately; "
                                  "expected not-ready");
      }
      return aerogpu_test::FailHresult(kTestName, "GetData(DONOTFLUSH warmup)", hr_immediate);
    }
  }

  hr = dev->Flush();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Flush(warmup)", hr);
  }
  {
    const DWORD start = GetTickCount();
    for (;;) {
      HRESULT hr_poll = E_FAIL;
      LONGLONG poll_start_qpc = 0;
      LONGLONG poll_end_qpc = 0;
      if (!getdata.GetData(query.get(),
                           NULL,
                           0,
                           D3DGETDATA_DONOTFLUSH,
                           200,
                           &hr_poll,
                           &poll_start_qpc,
                           &poll_end_qpc)) {
        FailFast(kTestName, "GetData(warmup) hung");
      }
      const double poll_call_ms = QpcToMs(poll_end_qpc - poll_start_qpc, qpc_freq);
      if (poll_call_ms > kMaxGetDataCallMs) {
        return aerogpu_test::Fail(kTestName, "GetData(DONOTFLUSH) warmup poll blocked for %.3fms", poll_call_ms);
      }
      if (hr_poll == S_OK) {
        break;
      }
      if (hr_poll != S_FALSE && hr_poll != D3DERR_WASSTILLDRAWING) {
        return aerogpu_test::FailHresult(kTestName, "GetData(warmup)", hr_poll);
      }
      if ((GetTickCount() - start) > 2000) {
        return aerogpu_test::Fail(kTestName, "warmup query did not complete within 2s");
      }
      Sleep(1);
    }
  }

  for (uint32_t it = 0; it < iterations; ++it) {
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(10 + it, 20 + it, 30 + it), 1.0f, 0);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "Clear", hr);
    }

    hr = query->Issue(D3DISSUE_END);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::Issue(END)", hr);
    }

    HRESULT hr_immediate = E_FAIL;
    LONGLONG start_qpc = 0;
    LONGLONG end_qpc = 0;
    if (!getdata.GetData(query.get(),
                         NULL,
                         0,
                         D3DGETDATA_DONOTFLUSH,
                         200,
                         &hr_immediate,
                         &start_qpc,
                         &end_qpc)) {
      FailFast(kTestName, "GetData(DONOTFLUSH) hung (iteration %u)", (unsigned)it);
    }
    const double immediate_ms = QpcToMs(end_qpc - start_qpc, qpc_freq);

    if (hr_immediate != S_FALSE && hr_immediate != D3DERR_WASSTILLDRAWING) {
      if (hr_immediate == S_OK) {
        return aerogpu_test::Fail(
            kTestName,
            "GetData(D3DGETDATA_DONOTFLUSH) returned S_OK immediately (iteration %u); expected not-ready "
            "(S_FALSE/WASSTILLDRAWING) to confirm the query tracks real GPU progress",
            (unsigned)it);
      }
      return aerogpu_test::FailHresult(kTestName, "GetData(DONOTFLUSH)", hr_immediate);
    }
    if (immediate_ms > kMaxGetDataCallMs) {
      return aerogpu_test::Fail(kTestName,
                                "GetData(DONOTFLUSH) took too long: %.3fms (iteration %u, hr=%s)",
                                immediate_ms,
                                (unsigned)it,
                                aerogpu_test::HresultToString(hr_immediate).c_str());
    }

    hr = dev->Flush();
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "Flush", hr);
    }

    const DWORD poll_start = GetTickCount();
    uint32_t polls = 0;
    for (;;) {
      ++polls;
      HRESULT hr_poll = E_FAIL;
      LONGLONG poll_start_qpc = 0;
      LONGLONG poll_end_qpc = 0;
      if (!getdata.GetData(query.get(),
                           NULL,
                           0,
                           D3DGETDATA_DONOTFLUSH,
                           200,
                           &hr_poll,
                           &poll_start_qpc,
                           &poll_end_qpc)) {
        FailFast(kTestName, "GetData poll hung (iteration %u)", (unsigned)it);
      }
      const double poll_call_ms = QpcToMs(poll_end_qpc - poll_start_qpc, qpc_freq);
      if (poll_call_ms > kMaxGetDataCallMs) {
        return aerogpu_test::Fail(kTestName,
                                  "GetData(DONOTFLUSH) poll blocked for %.3fms (iteration %u)",
                                  poll_call_ms,
                                  (unsigned)it);
      }
      if (hr_poll == S_OK) {
        break;
      }
      if (hr_poll != S_FALSE && hr_poll != D3DERR_WASSTILLDRAWING) {
        return aerogpu_test::FailHresult(kTestName, "GetData poll", hr_poll);
      }
      if ((GetTickCount() - poll_start) > 2000) {
        return aerogpu_test::Fail(kTestName,
                                  "event query did not complete within 2s (iteration %u, polls=%u)",
                                  (unsigned)it,
                                  (unsigned)polls);
      }
      Sleep(1);
    }

    aerogpu_test::PrintfStdout("INFO: %s: iteration %u: immediate=%.3fms polls=%u",
                               kTestName,
                               (unsigned)it,
                               immediate_ms,
                               (unsigned)polls);
  }

  if (!saw_immediate_not_ready) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: GetData(D3DGETDATA_DONOTFLUSH) returned S_OK immediately for every iteration; "
        "this is allowed, but it makes the test less sensitive to stalled query/fence behavior",
        kTestName);
  }

  if (process_stress) {
    // --- Multi-process stress test ---
    aerogpu_test::PrintfStdout("INFO: %s: starting multi-process stress (%u iterations per process)",
                               kTestName,
                               (unsigned)stress_iterations);

    std::wstring exe_path;
    std::string exe_err;
    HMODULE self = GetModuleHandleW(NULL);
    if (!self || !aerogpu_test::TryGetModuleFileNameW(self, &exe_path, &exe_err) || exe_path.empty()) {
      if (exe_err.empty()) {
        exe_err = "GetModuleFileNameW failed";
      }
      return aerogpu_test::Fail(kTestName, "failed to resolve executable path: %s", exe_err.c_str());
    }

    char event_name_a[128];
    sprintf(event_name_a,
            "AeroGPU_D3D9ExEventQuery_Start_%lu_%lu",
            (unsigned long)GetCurrentProcessId(),
            (unsigned long)GetTickCount());
    const std::wstring event_name_w(event_name_a, event_name_a + strlen(event_name_a));

    HANDLE start_event = CreateEventW(NULL, TRUE, FALSE, event_name_w.c_str());
    if (!start_event) {
      return aerogpu_test::Fail(kTestName,
                                "CreateEvent(start_event) failed: %s",
                                aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }

    HANDLE job = CreateJobObjectW(NULL, NULL);
    if (job) {
      JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
      ZeroMemory(&info, sizeof(info));
      info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
      if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info))) {
        aerogpu_test::PrintfStdout("INFO: %s: SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: %s",
                                   kTestName,
                                   aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
        CloseHandle(job);
        job = NULL;
      }
    }

    HANDLE procs[2] = {NULL, NULL};
    HANDLE threads[2] = {NULL, NULL};
    for (int i = 0; i < 2; ++i) {
      std::wstring cmdline = L"\"";
      cmdline += exe_path;
      cmdline += L"\" --child-stress";
      cmdline += L" --child-index=";
      wchar_t idx_buf[16];
      wsprintfW(idx_buf, L"%d", i);
      cmdline += idx_buf;
      cmdline += L" --start-event=";
      cmdline += event_name_w;
      cmdline += L" --stress-iterations=";
      wchar_t iter_buf[32];
      wsprintfW(iter_buf, L"%u", (unsigned)stress_iterations);
      cmdline += iter_buf;
      if (show_window && !hidden) {
        cmdline += L" --show";
      } else {
        cmdline += L" --hidden";
      }
      if (has_require_vid) {
        cmdline += L" --require-vid=";
        cmdline += std::wstring(require_vid_str.begin(), require_vid_str.end());
      }
      if (has_require_did) {
        cmdline += L" --require-did=";
        cmdline += std::wstring(require_did_str.begin(), require_did_str.end());
      }
      if (allow_microsoft) {
        cmdline += L" --allow-microsoft";
      }
      if (allow_non_aerogpu) {
        cmdline += L" --allow-non-aerogpu";
      }
      if (require_umd) {
        cmdline += L" --require-umd";
      }

      std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
      cmdline_buf.push_back(0);

      STARTUPINFOW si;
      ZeroMemory(&si, sizeof(si));
      si.cb = sizeof(si);

      PROCESS_INFORMATION pi;
      ZeroMemory(&pi, sizeof(pi));

      BOOL ok = CreateProcessW(exe_path.c_str(),
                               &cmdline_buf[0],
                               NULL,
                               NULL,
                               FALSE,
                               0,
                               NULL,
                               NULL,
                               &si,
                               &pi);
      if (!ok) {
        DWORD werr = GetLastError();
        if (job) {
          CloseHandle(job);
          job = NULL;
        }
        CloseHandle(start_event);
        for (int j = 0; j < 2; ++j) {
          if (threads[j]) {
            CloseHandle(threads[j]);
          }
          if (procs[j]) {
            CloseHandle(procs[j]);
          }
        }
        return aerogpu_test::Fail(kTestName,
                                  "CreateProcessW failed: %s",
                                  aerogpu_test::Win32ErrorToString(werr).c_str());
      }

      procs[i] = pi.hProcess;
      threads[i] = pi.hThread;
      if (job && !AssignProcessToJobObject(job, pi.hProcess)) {
        aerogpu_test::PrintfStdout("INFO: %s: AssignProcessToJobObject failed: %s",
                                   kTestName,
                                   aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      }
    }

    SetEvent(start_event);

    // Scale the join timeout with iteration count so manual runs with large
    // --stress-iterations values don't spuriously fail, while still bounding the
    // wait in case a child hangs.
    DWORD stress_timeout_ms = 30000;
    const DWORD scaled_timeout_ms = (DWORD)stress_iterations * 200;
    if (scaled_timeout_ms > stress_timeout_ms) {
      stress_timeout_ms = scaled_timeout_ms;
    }
    if (stress_timeout_ms > 300000) {
      stress_timeout_ms = 300000;
    }

    DWORD w = WaitForMultipleObjects(2, procs, TRUE, stress_timeout_ms);
    if (w != WAIT_OBJECT_0) {
      FailFast(kTestName, "multi-process stress timed out waiting for child processes");
    }

    bool ok = true;
    for (int i = 0; i < 2; ++i) {
      DWORD exit_code = 1;
      if (!GetExitCodeProcess(procs[i], &exit_code)) {
        ok = false;
      } else if (exit_code != 0) {
        ok = false;
      }
    }

    for (int i = 0; i < 2; ++i) {
      if (threads[i]) {
        CloseHandle(threads[i]);
      }
      if (procs[i]) {
        CloseHandle(procs[i]);
      }
    }
    CloseHandle(start_event);
    if (job) {
      CloseHandle(job);
    }

    if (!ok) {
      return aerogpu_test::Fail(kTestName, "multi-process stress child failed");
    }
  } else {
    // --- Multi-device stress test ---
    aerogpu_test::PrintfStdout("INFO: %s: starting multi-device stress (%u iterations per device)",
                               kTestName,
                               (unsigned)stress_iterations);

    HANDLE start_event = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!start_event) {
      return aerogpu_test::Fail(kTestName, "CreateEvent failed");
    }

    volatile LONG any_failed = 0;
    volatile LONG saw_was_still_drawing = 0;

    StressWorkerParams params[2];
    ZeroMemory(params, sizeof(params));
    for (int i = 0; i < 2; ++i) {
      params[i].index = i;
    params[i].iterations = (int)stress_iterations;
    params[i].show_window = show_window && !hidden;
    params[i].start_event = start_event;
    params[i].any_failed = &any_failed;
    params[i].saw_was_still_drawing = &saw_was_still_drawing;
    params[i].allow_microsoft = allow_microsoft;
    params[i].allow_non_aerogpu = allow_non_aerogpu;
    params[i].require_umd = require_umd;
    params[i].has_require_vid = has_require_vid;
    params[i].has_require_did = has_require_did;
    params[i].require_vid = require_vid;
    params[i].require_did = require_did;
  }

    HANDLE threads[2];
    threads[0] = CreateThread(NULL, 0, StressWorkerThreadProc, &params[0], 0, NULL);
    threads[1] = CreateThread(NULL, 0, StressWorkerThreadProc, &params[1], 0, NULL);
    if (!threads[0] || !threads[1]) {
      if (threads[0]) {
        CloseHandle(threads[0]);
      }
      if (threads[1]) {
        CloseHandle(threads[1]);
      }
      CloseHandle(start_event);
      return aerogpu_test::Fail(kTestName, "CreateThread failed");
    }

    SetEvent(start_event);

    // Scale the join timeout with iteration count so manual runs with large
    // --stress-iterations values don't spuriously fail, while still bounding the
    // wait in case a worker thread hangs.
    DWORD stress_timeout_ms = 30000;
    const DWORD scaled_timeout_ms = (DWORD)stress_iterations * 100;
    if (scaled_timeout_ms > stress_timeout_ms) {
      stress_timeout_ms = scaled_timeout_ms;
    }
    if (stress_timeout_ms > 300000) {
      stress_timeout_ms = 300000;
    }

    DWORD w = WaitForMultipleObjects(2, threads, TRUE, stress_timeout_ms);
    CloseHandle(threads[0]);
    CloseHandle(threads[1]);
    CloseHandle(start_event);

    if (w != WAIT_OBJECT_0) {
      FailFast(kTestName, "multi-device stress timed out waiting for worker threads");
    }

    if (any_failed != 0) {
      return aerogpu_test::Fail(kTestName, "multi-device stress worker failed");
    }

    aerogpu_test::PrintfStdout("INFO: %s: PresentEx(DONOTWAIT) observed WASSTILLDRAWING=%s",
                               kTestName,
                               saw_was_still_drawing != 0 ? "yes" : "no");
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExEventQuery(argc, argv);
}
