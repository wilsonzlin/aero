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

static int RunD3D9ExEventQuery(int argc, char** argv) {
  const char* kTestName = "d3d9ex_event_query";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--show] [--show-window] [--hidden] [--iterations=N] [--require-vid=0x####] "
        "[--require-did=0x####] "
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
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                           D3DDEVTYPE_HAL,
                           hwnd,
                           create_flags,
                           &pp,
                           NULL,
                           dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES |
                   D3DCREATE_MULTITHREADED;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             &pp,
                             NULL,
                             dev.put());
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
    hr = dev->Flush();
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "Flush", hr);
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

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExEventQuery(argc, argv);
}
