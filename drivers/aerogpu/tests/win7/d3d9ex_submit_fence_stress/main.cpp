#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

#include "..\\..\\..\\protocol\\aerogpu_ring.h"

#include <deque>

using aerogpu_test::ComPtr;
using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static const char* RingFormatToString(uint32_t fmt) {
  switch (fmt) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      return "legacy";
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      return "agpu";
    default:
      return "unknown";
  }
}

static void DumpRingDumpV2(const char* test_name, const aerogpu_escape_dump_ring_v2_inout& dump) {
  unsigned long window_start = 0;
  if (dump.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU && dump.desc_count != 0) {
    window_start = (unsigned long)(dump.tail - dump.desc_count);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: ring dump v2: ring_id=%lu format=%s size_bytes=%lu head=0x%08lX tail=0x%08lX desc_count=%lu window_start=0x%08lX",
      test_name,
      (unsigned long)dump.ring_id,
      RingFormatToString((uint32_t)dump.ring_format),
      (unsigned long)dump.ring_size_bytes,
      (unsigned long)dump.head,
      (unsigned long)dump.tail,
      (unsigned long)dump.desc_count,
      window_start);

  uint32_t count = dump.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }
  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_ring_desc_v2& d = dump.desc[i];
    const unsigned long ring_index =
        (dump.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) ? (window_start + (unsigned long)i) : (unsigned long)i;
    aerogpu_test::PrintfStdout(
        "INFO: %s:   desc[%lu] ring_index=%lu: fence=%I64u flags=0x%08lX cmd_gpa=0x%I64X cmd_size=%lu alloc_table_gpa=0x%I64X alloc_table_size=%lu",
        test_name,
        (unsigned long)i,
        ring_index,
        (unsigned long long)d.fence,
        (unsigned long)d.flags,
        (unsigned long long)d.cmd_gpa,
        (unsigned long)d.cmd_size_bytes,
        (unsigned long long)d.alloc_table_gpa,
        (unsigned long)d.alloc_table_size_bytes);
  }
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
        "Usage: %s.exe [--iterations=N] [--show] [--json[=PATH]] [--allow-remote] [--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd] [--require-agpu]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Stresses D3D9Ex submits and validates per-submission fences via AeroGPU debug output. "
        "On AGPU devices, also validates PRESENT flag + alloc table presence via ring dump v2.");
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
  const bool require_agpu = aerogpu_test::HasArg(argc, argv, "--require-agpu");
  const bool show_window =
      aerogpu_test::HasArg(argc, argv, "--show-window") || aerogpu_test::HasArg(argc, argv, "--show");

  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
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

  if (require_umd || require_agpu || (!allow_microsoft && !allow_non_aerogpu)) {
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
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    if (validate_fences) {
      return reporter.Fail("%s", kmt_err.c_str());
    }
    aerogpu_test::PrintfStdout("INFO: %s: %s (skipping KMD fence validation)", kTestName, kmt_err.c_str());
  }

  D3DKMT_HANDLE kmt_adapter = 0;
  if (kmt.gdi32) {
    std::string open_err;
    if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &kmt_adapter, &open_err)) {
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
    if (aerogpu_test::kmt::AerogpuQueryFence(&kmt, kmt_adapter, &base_submitted, &base_completed, &st)) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: KMD fences before: submitted=%I64u completed=%I64u",
          kTestName,
          base_submitted,
          base_completed);
    } else if (validate_fences) {
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        return reporter.Fail("AeroGPU KMD fence escape not supported (NTSTATUS=0x%08lX)", (unsigned long)st);
      }
      return reporter.Fail("D3DKMTEscape(query-fence) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }
  }

  DbwinCapture dbwin;
  if (validate_fences) {
    std::string dbwin_err;
    if (!dbwin.Start(&dbwin_err)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
      return reporter.Fail("DBWIN capture init failed: %s", dbwin_err.c_str());
    }
  }

  hr = dev->SetMaximumFrameLatency(1);
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
    return reporter.FailHresult("IDirect3DDevice9Ex::SetMaximumFrameLatency(1)", hr);
  }

  ComPtr<IDirect3DQuery9> query;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, query.put());
  if (FAILED(hr) || !query) {
    aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
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
  bool validated_ring_desc = false;
  const bool enforce_agpu_ring_checks = require_umd || require_agpu;

  for (uint32_t i = 0; i < iterations; ++i) {
    MSG msg;
    while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
      TranslateMessage(&msg);
      DispatchMessageW(&msg);
    }

    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB((int)(i & 255), 0, 0), 1.0f, 0);
    if (FAILED(hr)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
      return reporter.FailHresult("Clear", hr);
    }

    hr = query->Issue(D3DISSUE_END);
    if (FAILED(hr)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
      return reporter.FailHresult("IDirect3DQuery9::Issue(END)", hr);
    }

    unsigned long long issue_fence = 0;
    std::string issue_line;
    if (validate_fences) {
      if (!dbwin.WaitForSubmitFence(pid, 2000, /*expected_present=*/0, &issue_fence, &issue_line)) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("timed out waiting for submit fence log (iteration %u)", (unsigned)(i + 1));
      }
      if (issue_fence == 0) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("got fence=0 from submit log: %s", issue_line.c_str());
      }
      if (last_fence != 0 && issue_fence <= last_fence) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
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
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.FailHresult("IDirect3DQuery9::GetData(FLUSH)", hr);
      }
      if ((GetTickCount() - start) > 5000) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("query did not complete within 5s (iteration %u)", (unsigned)(i + 1));
      }
      Sleep(0);
    }

    if (validate_fences && kmt_adapter) {
      unsigned long long submitted = 0;
      unsigned long long completed = 0;
      NTSTATUS st = 0;
      if (!aerogpu_test::kmt::AerogpuQueryFence(&kmt, kmt_adapter, &submitted, &completed, &st)) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("D3DKMTEscape(query-fence) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      }
      if (completed < issue_fence) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
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
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.FailHresult("IDirect3DDevice9Ex::PresentEx(DONOTWAIT)", hr);
      }
      if ((GetTickCount() - present_start) > 5000) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("PresentEx(DONOTWAIT) did not make progress within 5s");
      }
      Sleep(0);
    }

    if (validate_fences) {
      aerogpu_escape_dump_ring_v2_inout dump_at_present;
      ZeroMemory(&dump_at_present, sizeof(dump_at_present));
      NTSTATUS dump_at_present_status = 0;
      bool have_dump_at_present = false;

      // Capture a ring dump snapshot *before* waiting on DBWIN so we minimize the chance of racing
      // the device consuming the descriptor.
      if (!validated_ring_desc && kmt_adapter) {
        have_dump_at_present =
            aerogpu_test::kmt::AerogpuDumpRingV2(&kmt, kmt_adapter, /*ring_id=*/0, &dump_at_present, &dump_at_present_status);
      }

      unsigned long long present_fence = 0;
      std::string present_line;
      if (!dbwin.WaitForSubmitFence(pid, 2000, /*expected_present=*/1, &present_fence, &present_line)) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("timed out waiting for present submit fence log");
      }
      if (present_fence == 0) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("got fence=0 from present submit log: %s", present_line.c_str());
      }
      if (present_fence <= last_fence) {
        aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
        return reporter.Fail("non-monotonic present fence: prev=%I64u cur=%I64u (line: %s)",
                              last_fence,
                              present_fence,
                              present_line.c_str());
      }
      last_fence = present_fence;

      // Validate that PRESENT submissions are marked as such in the ring descriptor and that
      // submissions referencing guest-backed allocations include an alloc table (alloc_table_gpa).
      if (!validated_ring_desc && kmt_adapter) {
        aerogpu_escape_dump_ring_v2_inout dump = dump_at_present;
        NTSTATUS dump_status = dump_at_present_status;
        bool have_dump = have_dump_at_present;

        aerogpu_dbgctl_ring_desc_v2 present_desc;
        uint32_t present_desc_index = 0;
        bool found_present_desc = false;
        bool skip_ring_asserts = false;

        // Retry for a short bounded window (best-effort). This avoids flakes if the device consumes
        // the ring entry quickly.
        const DWORD retry_start = GetTickCount();
        for (;;) {
          if (!have_dump) {
            have_dump =
                aerogpu_test::kmt::AerogpuDumpRingV2(&kmt, kmt_adapter, /*ring_id=*/0, &dump, &dump_status);
          }
          if (!have_dump) {
            break;
          }

          // On legacy devices, the ring dump doesn't provide alloc tables; treat as optional unless
          // the caller explicitly requires AGPU.
          if (dump.ring_format != AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
            if (enforce_agpu_ring_checks) {
              DumpRingDumpV2(kTestName, dump);
              aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
              return reporter.Fail("expected AGPU ring format for ring dump v2, got %s (ring_format=%lu)",
                                   RingFormatToString((uint32_t)dump.ring_format),
                                   (unsigned long)dump.ring_format);
            }
            aerogpu_test::PrintfStdout(
                "INFO: %s: ring format is %s; skipping ring descriptor assertions (pass --require-agpu to fail)",
                kTestName,
                RingFormatToString((uint32_t)dump.ring_format));
            skip_ring_asserts = true;
            break;
          }

          found_present_desc = aerogpu_test::kmt::FindRingDescByFence(dump,
                                                                     present_fence,
                                                                     &present_desc,
                                                                     &present_desc_index);
          if (!found_present_desc) {
            aerogpu_dbgctl_ring_desc_v2 last_desc;
            uint32_t last_idx = 0;
            if (aerogpu_test::kmt::GetLastWrittenRingDesc(dump, &last_desc, &last_idx) &&
                (unsigned long long)last_desc.fence == present_fence) {
              present_desc = last_desc;
              present_desc_index = last_idx;
              found_present_desc = true;
            }
          }

          if (found_present_desc) {
            break;
          }

          if ((GetTickCount() - retry_start) > 250) {
            break;
          }

          // Retry with a fresh dump.
          have_dump = false;
          Sleep(0);
        }

        if (skip_ring_asserts) {
          validated_ring_desc = true;
        } else if (!have_dump) {
          if (!enforce_agpu_ring_checks && dump_status == aerogpu_test::kmt::kStatusNotSupported) {
            aerogpu_test::PrintfStdout(
                "INFO: %s: ring dump v2 escape not supported (NTSTATUS=0x%08lX); skipping ring descriptor assertions",
                kTestName,
                (unsigned long)dump_status);
            validated_ring_desc = true;
          } else {
            aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
            return reporter.Fail("D3DKMTEscape(dump-ring-v2) failed (NTSTATUS=0x%08lX)",
                                 (unsigned long)dump_status);
          }
        } else if (!found_present_desc) {
          DumpRingDumpV2(kTestName, dump);
          aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
          return reporter.Fail("failed to find ring descriptor for present fence=%I64u", present_fence);
        } else {
          aerogpu_test::PrintfStdout(
              "INFO: %s: matched ring desc[%lu] for present fence=%I64u flags=0x%08lX alloc_table_gpa=0x%I64X alloc_table_size=%lu",
              kTestName,
              (unsigned long)present_desc_index,
              present_fence,
              (unsigned long)present_desc.flags,
              (unsigned long long)present_desc.alloc_table_gpa,
              (unsigned long)present_desc.alloc_table_size_bytes);

          if ((present_desc.flags & AEROGPU_SUBMIT_FLAG_PRESENT) == 0) {
            DumpRingDumpV2(kTestName, dump);
            aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
            return reporter.Fail(
                "present fence=%I64u missing AEROGPU_SUBMIT_FLAG_PRESENT in ring descriptor (flags=0x%08lX)",
                present_fence,
                (unsigned long)present_desc.flags);
          }

          if (present_desc.alloc_table_gpa == 0 ||
              present_desc.alloc_table_size_bytes < sizeof(struct aerogpu_alloc_table_header)) {
            DumpRingDumpV2(kTestName, dump);
            aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
            return reporter.Fail(
                "present fence=%I64u has missing/invalid alloc table: alloc_table_gpa=0x%I64X alloc_table_size=%lu (expected >= %lu)",
                present_fence,
                (unsigned long long)present_desc.alloc_table_gpa,
                (unsigned long)present_desc.alloc_table_size_bytes,
                (unsigned long)sizeof(struct aerogpu_alloc_table_header));
          }

          validated_ring_desc = true;
        }
      }
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

  aerogpu_test::kmt::CloseAdapter(&kmt, kmt_adapter);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSubmitFenceStress(argc, argv);
}
