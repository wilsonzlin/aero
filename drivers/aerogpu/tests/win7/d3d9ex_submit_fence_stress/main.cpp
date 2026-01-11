#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

#include "..\\..\\..\\protocol\\aerogpu_dbgctl_escape.h"

#include <deque>

using aerogpu_test::ComPtr;

typedef LONG NTSTATUS;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#endif

typedef UINT D3DKMT_HANDLE;

typedef struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  D3DKMT_HANDLE hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
} D3DKMT_OPENADAPTERFROMHDC;

typedef struct D3DKMT_CLOSEADAPTER {
  D3DKMT_HANDLE hAdapter;
} D3DKMT_CLOSEADAPTER;

typedef enum D3DKMT_ESCAPETYPE {
  D3DKMT_ESCAPE_DRIVERPRIVATE = 0,
} D3DKMT_ESCAPETYPE;

typedef struct D3DKMT_ESCAPEFLAGS {
  union {
    struct {
      UINT HardwareAccess : 1;
      UINT Reserved : 31;
    };
    UINT Value;
  };
} D3DKMT_ESCAPEFLAGS;

typedef struct D3DKMT_ESCAPE {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  D3DKMT_HANDLE hContext;
  D3DKMT_ESCAPETYPE Type;
  D3DKMT_ESCAPEFLAGS Flags;
  VOID* pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_ESCAPE;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTEscape)(D3DKMT_ESCAPE* pData);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
} D3DKMT_FUNCS;

static bool LoadD3DKMT(D3DKMT_FUNCS* out, std::string* err) {
  ZeroMemory(out, sizeof(*out));

  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    if (err) {
      *err = "LoadLibraryW(gdi32.dll) failed";
    }
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->Escape = (PFND3DKMTEscape)GetProcAddress(out->gdi32, "D3DKMTEscape");

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->Escape) {
    if (err) {
      *err =
          "Required D3DKMT* exports not found in gdi32.dll. This test requires Windows Vista+ (WDDM).";
    }
    if (out->gdi32) {
      FreeLibrary(out->gdi32);
      out->gdi32 = NULL;
    }
    return false;
  }

  return true;
}

static bool OpenPrimaryKmtAdapter(const D3DKMT_FUNCS* f, D3DKMT_HANDLE* out_adapter, std::string* err) {
  if (!f || !out_adapter) {
    return false;
  }
  *out_adapter = 0;

  HDC hdc = GetDC(NULL);
  if (!hdc) {
    if (err) {
      *err = "GetDC(NULL) failed";
    }
    return false;
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f->OpenAdapterFromHdc(&open);
  ReleaseDC(NULL, hdc);

  if (!NT_SUCCESS(st) || open.hAdapter == 0) {
    if (err) {
      char buf[128];
      _snprintf(buf, sizeof(buf), "D3DKMTOpenAdapterFromHdc failed (NTSTATUS=0x%08lX)",
                (unsigned long)st);
      buf[sizeof(buf) - 1] = 0;
      *err = buf;
    }
    return false;
  }

  *out_adapter = open.hAdapter;
  return true;
}

static void CloseKmtAdapter(const D3DKMT_FUNCS* f, D3DKMT_HANDLE adapter) {
  if (!f || !adapter) {
    return;
  }
  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = adapter;
  (void)f->CloseAdapter(&close);
}

static bool QueryKmdFence(const D3DKMT_FUNCS* f,
                          D3DKMT_HANDLE adapter,
                          unsigned long long* out_submitted,
                          unsigned long long* out_completed,
                          NTSTATUS* out_status) {
  if (out_submitted) {
    *out_submitted = 0;
  }
  if (out_completed) {
    *out_completed = 0;
  }
  if (out_status) {
    *out_status = 0;
  }
  if (!f || !adapter || !f->Escape) {
    if (out_status) {
      *out_status = (NTSTATUS)0xC000000DL; // STATUS_INVALID_PARAMETER
    }
    return false;
  }

  aerogpu_escape_query_fence_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = adapter;
  e.hDevice = 0;
  e.hContext = 0;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = &q;
  e.PrivateDriverDataSize = sizeof(q);

  const NTSTATUS st = f->Escape(&e);
  if (out_status) {
    *out_status = st;
  }
  if (!NT_SUCCESS(st)) {
    return false;
  }

  if (out_submitted) {
    *out_submitted = (unsigned long long)q.last_submitted_fence;
  }
  if (out_completed) {
    *out_completed = (unsigned long long)q.last_completed_fence;
  }
  return true;
}

class DbwinCapture {
 public:
  DbwinCapture()
      : mapping_(NULL),
        view_(NULL),
        buffer_ready_(NULL),
        data_ready_(NULL),
        stop_event_(NULL),
        msg_event_(NULL),
        thread_(NULL),
        cs_inited_(false) {
    ZeroMemory(&cs_, sizeof(cs_));
  }

  ~DbwinCapture() { Stop(); }

  bool Start(std::string* err) {
    Stop();

    mapping_ = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 4096, L"DBWIN_BUFFER");
    if (!mapping_) {
      if (err) {
        *err = "CreateFileMappingW(DBWIN_BUFFER) failed";
      }
      return false;
    }

    view_ = MapViewOfFile(mapping_, FILE_MAP_READ, 0, 0, 0);
    if (!view_) {
      if (err) {
        *err = "MapViewOfFile(DBWIN_BUFFER) failed";
      }
      Stop();
      return false;
    }

    buffer_ready_ = CreateEventW(NULL, FALSE, FALSE, L"DBWIN_BUFFER_READY");
    data_ready_ = CreateEventW(NULL, FALSE, FALSE, L"DBWIN_DATA_READY");
    if (!buffer_ready_ || !data_ready_) {
      if (err) {
        *err = "CreateEventW(DBWIN_*) failed";
      }
      Stop();
      return false;
    }

    stop_event_ = CreateEventW(NULL, TRUE, FALSE, NULL);
    msg_event_ = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (!stop_event_ || !msg_event_) {
      if (err) {
        *err = "CreateEventW failed";
      }
      Stop();
      return false;
    }

    InitializeCriticalSection(&cs_);
    cs_inited_ = true;

    thread_ = CreateThread(NULL, 0, ThreadMain, this, 0, NULL);
    if (!thread_) {
      if (err) {
        *err = "CreateThread failed";
      }
      Stop();
      return false;
    }

    return true;
  }

  void Clear() {
    if (!cs_inited_ || !msg_event_) {
      return;
    }
    EnterCriticalSection(&cs_);
    queue_.clear();
    ResetEvent(msg_event_);
    LeaveCriticalSection(&cs_);
  }

  void Stop() {
    if (stop_event_) {
      SetEvent(stop_event_);
    }

    if (thread_) {
      WaitForSingleObject(thread_, 2000);
      CloseHandle(thread_);
      thread_ = NULL;
    }

    if (msg_event_) {
      CloseHandle(msg_event_);
      msg_event_ = NULL;
    }

    if (stop_event_) {
      CloseHandle(stop_event_);
      stop_event_ = NULL;
    }

    if (data_ready_) {
      CloseHandle(data_ready_);
      data_ready_ = NULL;
    }
    if (buffer_ready_) {
      // Ensure we don't leave OutputDebugString callers stuck waiting.
      SetEvent(buffer_ready_);
      CloseHandle(buffer_ready_);
      buffer_ready_ = NULL;
    }

    if (view_) {
      UnmapViewOfFile(view_);
      view_ = NULL;
    }
    if (mapping_) {
      CloseHandle(mapping_);
      mapping_ = NULL;
    }

    if (cs_inited_) {
      // Guard against double-delete if Stop() is called after a partial Start() failure.
      DeleteCriticalSection(&cs_);
      ZeroMemory(&cs_, sizeof(cs_));
      cs_inited_ = false;
    }

    queue_.clear();
  }

  bool WaitForSubmitFence(DWORD pid,
                          DWORD timeout_ms,
                          int expected_present,
                          unsigned long long* out_fence,
                          std::string* out_line) {
    if (out_fence) {
      *out_fence = 0;
    }
    if (out_line) {
      out_line->clear();
    }
    if (!msg_event_) {
      return false;
    }

    const DWORD start = GetTickCount();
    for (;;) {
      const DWORD elapsed = GetTickCount() - start;
      if (elapsed >= timeout_ms) {
        return false;
      }
      const DWORD remaining = timeout_ms - elapsed;
      DWORD w = WaitForSingleObject(msg_event_, remaining);
      if (w != WAIT_OBJECT_0) {
        return false;
      }

      Message msg;
      {
        EnterCriticalSection(&cs_);
        if (queue_.empty()) {
          ResetEvent(msg_event_);
          LeaveCriticalSection(&cs_);
          continue;
        }
        msg = queue_.front();
        queue_.pop_front();
        if (queue_.empty()) {
          ResetEvent(msg_event_);
        }
        LeaveCriticalSection(&cs_);
      }

      if (msg.pid != pid) {
        continue;
      }

      unsigned long long fence = 0;
      int present = -1;
      if (TryParseSubmitFence(msg.text.c_str(), &fence, &present)) {
        if (expected_present >= 0 && present != expected_present) {
          continue;
        }
        if (out_fence) {
          *out_fence = fence;
        }
        if (out_line) {
          *out_line = msg.text;
        }
        return true;
      }
    }
  }

 private:
  struct Message {
    DWORD pid;
    std::string text;
  };

  static bool TryParseSubmitFence(const char* line, unsigned long long* out_fence, int* out_present) {
    if (out_fence) {
      *out_fence = 0;
    }
    if (out_present) {
      *out_present = -1;
    }
    if (!line || !out_fence) {
      return false;
    }
    // Example line:
    // aerogpu-d3d9: submit cmd_bytes=123 fence=456 present=0
    const char* prefix = "aerogpu-d3d9: submit";
    if (!aerogpu_test::StrIContainsA(line, prefix)) {
      return false;
    }
    const char* fence_key = strstr(line, "fence=");
    if (!fence_key) {
      return false;
    }
    fence_key += 6;
    char* end = NULL;
    unsigned long long fence = _strtoui64(fence_key, &end, 10);
    if (!end || end == fence_key) {
      return false;
    }
    *out_fence = fence;

    const char* present_key = strstr(line, "present=");
    if (present_key && out_present) {
      present_key += 8;
      char* pend = NULL;
      const unsigned long pv = strtoul(present_key, &pend, 10);
      if (pend && pend != present_key) {
        *out_present = (pv != 0) ? 1 : 0;
      }
    }
    return true;
  }

  static DWORD WINAPI ThreadMain(LPVOID param) {
    DbwinCapture* self = (DbwinCapture*)param;
    self->Run();
    return 0;
  }

  void Run() {
    if (!buffer_ready_ || !data_ready_ || !stop_event_ || !view_) {
      return;
    }

    // Allow the first OutputDebugString writer to proceed.
    SetEvent(buffer_ready_);

    HANDLE handles[2] = {stop_event_, data_ready_};
    for (;;) {
      DWORD w = WaitForMultipleObjects(2, handles, FALSE, INFINITE);
      if (w == WAIT_OBJECT_0) {
        break;
      }
      if (w != WAIT_OBJECT_0 + 1) {
        break;
      }

      const DWORD pid = *(DWORD*)view_;
      const char* text = (const char*)view_ + sizeof(DWORD);
      const size_t max = 4096 - sizeof(DWORD);
      size_t len = 0;
      for (; len < max; ++len) {
        if (text[len] == 0) {
          break;
        }
      }

      Message msg;
      msg.pid = pid;
      msg.text.assign(text, text + len);

      {
        EnterCriticalSection(&cs_);
        queue_.push_back(msg);
        // Prevent unbounded growth if the system is chatty.
        if (queue_.size() > 2048) {
          queue_.pop_front();
        }
        SetEvent(msg_event_);
        LeaveCriticalSection(&cs_);
      }

      // Signal readiness for the next writer.
      SetEvent(buffer_ready_);
    }
  }

  HANDLE mapping_;
  void* view_;
  HANDLE buffer_ready_;
  HANDLE data_ready_;
  HANDLE stop_event_;
  HANDLE msg_event_;
  HANDLE thread_;
  bool cs_inited_;
  CRITICAL_SECTION cs_;
  std::deque<Message> queue_;
};

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         DWORD create_flags,
                                         IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  HRESULT hr =
      d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, pp, NULL, out_dev);
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

static int RunSubmitFenceStress(int argc, char** argv) {
  const char* kTestName = "d3d9ex_submit_fence_stress";

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--iterations=N] [--show] [--allow-remote] [--json[=PATH]] [--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd]",
        kTestName);
    aerogpu_test::PrintfStdout("Stresses D3D9Ex submits and validates per-submission fences via AeroGPU debug output.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  // Enable per-submit fence logging in the AeroGPU D3D9 UMD (captured via DBWIN).
  // This must be set before the UMD DLL is loaded.
  SetEnvironmentVariableA("AEROGPU_D3D9_LOG_SUBMITS", "1");

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool show_window =
      aerogpu_test::HasArg(argc, argv, "--show-window") || aerogpu_test::HasArg(argc, argv, "--show");

  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  uint32_t iterations = 200;
  std::string iter_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--iterations", &iter_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(iter_str, &iterations, &err)) {
      return reporter.Fail("invalid --iterations: %s", err.c_str());
    }
  }
  if (iterations < 10) {
    iterations = 10;
  }
  if (iterations > 2000) {
    iterations = 2000;
  }

  const int kWidth = 256;
  const int kHeight = 256;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSubmitFenceStress",
                                              L"AeroGPU D3D9Ex Submit Fence Stress",
                                              kWidth,
                                              kHeight,
                                              show_window);
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
  // Prefer vsync to exercise max-frame-latency throttling.
  pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr)) {
    // Some environments (e.g. unusual vblank configs) may not support interval-one. Fall back to immediate.
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  }
  if (FAILED(hr) || !dev) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    reporter.SetAdapterInfoA(ident.Description, (uint32_t)ident.VendorId, (uint32_t)ident.DeviceId);
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               kTestName,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return reporter.Fail("refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                           "Install AeroGPU driver or pass --allow-microsoft.",
                           (unsigned)ident.VendorId,
                           (unsigned)ident.DeviceId);
    }
    if (!allow_non_aerogpu && !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return reporter.Fail("adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu)",
                           ident.Description);
    }
  }

  bool aero_umd_loaded = false;
  {
    std::wstring path;
    std::string err;
    aero_umd_loaded =
        aerogpu_test::GetLoadedModulePathByBaseName(aerogpu_test::ExpectedAeroGpuD3D9UmdModuleBaseName(),
                                                    &path,
                                                    &err);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
    aero_umd_loaded = true;
  }

  // If the AeroGPU UMD isn't loaded (e.g. running on a different adapter), we can still smoke-test the
  // D3D9Ex query/present loop, but we cannot validate per-submission fences.
  const bool validate_fences = aero_umd_loaded;

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!LoadD3DKMT(&kmt, &kmt_err)) {
    if (validate_fences) {
      return reporter.Fail("%s", kmt_err.c_str());
    }
    aerogpu_test::PrintfStdout("INFO: %s: %s (skipping KMD fence validation)", kTestName, kmt_err.c_str());
  }

  D3DKMT_HANDLE kmt_adapter = 0;
  if (kmt.gdi32) {
    std::string open_err;
    if (!OpenPrimaryKmtAdapter(&kmt, &kmt_adapter, &open_err)) {
      if (validate_fences) {
        return reporter.Fail("%s", open_err.c_str());
      }
      aerogpu_test::PrintfStdout("INFO: %s: %s (skipping KMD fence validation)", kTestName, open_err.c_str());
    }
  }

  unsigned long long base_submitted = 0;
  unsigned long long base_completed = 0;
  if (kmt_adapter) {
    NTSTATUS st = 0;
    if (QueryKmdFence(&kmt, kmt_adapter, &base_submitted, &base_completed, &st)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: KMD fences before: submitted=%I64u completed=%I64u",
          kTestName,
          base_submitted,
          base_completed);
    } else if (validate_fences) {
      if (st == STATUS_NOT_SUPPORTED) {
        return reporter.Fail("AeroGPU KMD fence escape not supported (NTSTATUS=0x%08lX)", (unsigned long)st);
      }
      return reporter.Fail("D3DKMTEscape(query-fence) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }
  }

  DbwinCapture dbwin;
  if (validate_fences) {
    std::string dbwin_err;
    if (!dbwin.Start(&dbwin_err)) {
      CloseKmtAdapter(&kmt, kmt_adapter);
      return reporter.Fail("DBWIN capture init failed: %s", dbwin_err.c_str());
    }
  }

  hr = dev->SetMaximumFrameLatency(1);
  if (FAILED(hr)) {
    CloseKmtAdapter(&kmt, kmt_adapter);
    return reporter.FailHresult("IDirect3DDevice9Ex::SetMaximumFrameLatency(1)", hr);
  }

  ComPtr<IDirect3DQuery9> query;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, query.put());
  if (FAILED(hr) || !query) {
    CloseKmtAdapter(&kmt, kmt_adapter);
    return reporter.FailHresult("IDirect3DDevice9Ex::CreateQuery(EVENT)", hr);
  }

  if (validate_fences) {
    // Drop any messages produced during device creation so the first iteration
    // reads the submit corresponding to the first Issue/Present calls.
    dbwin.Clear();
  }

  const DWORD pid = GetCurrentProcessId();
  unsigned long long last_fence = 0;
  bool saw_was_still_drawing = false;

  for (uint32_t i = 0; i < iterations; ++i) {
    MSG msg;
    while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
      TranslateMessage(&msg);
      DispatchMessageW(&msg);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB((int)(i & 255), 0, 0), 1.0f, 0);
    if (FAILED(hr)) {
      CloseKmtAdapter(&kmt, kmt_adapter);
      return reporter.FailHresult("Clear", hr);
    }

    hr = query->Issue(D3DISSUE_END);
    if (FAILED(hr)) {
      CloseKmtAdapter(&kmt, kmt_adapter);
      return reporter.FailHresult("IDirect3DQuery9::Issue(END)", hr);
    }

    unsigned long long issue_fence = 0;
    std::string issue_line;
    if (validate_fences) {
      if (!dbwin.WaitForSubmitFence(pid, 2000, /*expected_present=*/0, &issue_fence, &issue_line)) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("timed out waiting for submit fence log (iteration %u)", (unsigned)(i + 1));
      }
      if (issue_fence == 0) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("got fence=0 from submit log: %s", issue_line.c_str());
      }
      if (last_fence != 0 && issue_fence <= last_fence) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("non-monotonic submit fence: prev=%I64u cur=%I64u (line: %s)",
                             last_fence,
                             issue_fence,
                             issue_line.c_str());
      }
      last_fence = issue_fence;
    }

    const DWORD start = GetTickCount();
    DWORD done = 0;
    for (;;) {
      hr = query->GetData(&done, sizeof(done), D3DGETDATA_FLUSH);
      if (hr == S_OK) {
        break;
      }
      if (hr != S_FALSE && hr != D3DERR_WASSTILLDRAWING) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.FailHresult("IDirect3DQuery9::GetData(FLUSH)", hr);
      }
      if ((GetTickCount() - start) > 5000) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("query did not complete within 5s (iteration %u)", (unsigned)(i + 1));
      }
      Sleep(0);
    }

    if (validate_fences && kmt_adapter) {
      unsigned long long submitted = 0;
      unsigned long long completed = 0;
      NTSTATUS st = 0;
      if (!QueryKmdFence(&kmt, kmt_adapter, &submitted, &completed, &st)) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("D3DKMTEscape(query-fence) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      }
      if (completed < issue_fence) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail(
            "query completed but KMD fence is behind: fence=%I64u completed=%I64u submitted=%I64u",
            issue_fence,
            completed,
            submitted);
      }
    }

    // Present with DONOTWAIT; if we hit the frame-latency limit, we should get
    // D3DERR_WASSTILLDRAWING and then eventually make progress once prior work completes.
    DWORD present_start = GetTickCount();
    for (;;) {
      hr = dev->PresentEx(NULL, NULL, NULL, NULL, D3DPRESENT_DONOTWAIT);
      if (hr == S_OK) {
        break;
      }
      if (hr == D3DERR_WASSTILLDRAWING) {
        saw_was_still_drawing = true;
      } else {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx(DONOTWAIT)", hr);
      }
      if ((GetTickCount() - present_start) > 5000) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("PresentEx(DONOTWAIT) did not make progress within 5s");
      }
      Sleep(0);
    }

    if (validate_fences) {
      unsigned long long present_fence = 0;
      std::string present_line;
      if (!dbwin.WaitForSubmitFence(pid, 2000, /*expected_present=*/1, &present_fence, &present_line)) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("timed out waiting for present submit fence log");
      }
      if (present_fence == 0) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("got fence=0 from present submit log: %s", present_line.c_str());
      }
      if (present_fence <= last_fence) {
        CloseKmtAdapter(&kmt, kmt_adapter);
        return reporter.Fail("non-monotonic present fence: prev=%I64u cur=%I64u (line: %s)",
                             last_fence,
                             present_fence,
                             present_line.c_str());
      }
      last_fence = present_fence;
    }
  }

  if (validate_fences) {
    aerogpu_test::PrintfStdout("INFO: %s: last observed submission fence=%I64u", kTestName, last_fence);
  }

  if (saw_was_still_drawing) {
    aerogpu_test::PrintfStdout("INFO: %s: observed D3DERR_WASSTILLDRAWING during PresentEx throttling", kTestName);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: PresentEx(DONOTWAIT) never returned D3DERR_WASSTILLDRAWING", kTestName);
  }

  CloseKmtAdapter(&kmt, kmt_adapter);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSubmitFenceStress(argc, argv);
}
