#include "..\\common\\aerogpu_test_common.h"

// This test directly exercises the WDDM kernel vblank wait path by calling
// D3DKMTWaitForVerticalBlankEvent in a tight loop and measuring the pacing.
//
// It intentionally avoids requiring the Windows Driver Kit (WDK): the test
// dynamically loads the required D3DKMT entry points from gdi32.dll and defines
// the minimal thunk structs locally.

typedef LONG NTSTATUS;

typedef UINT D3DKMT_HANDLE;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

typedef ULONG(WINAPI* PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  D3DKMT_HANDLE hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
} D3DKMT_OPENADAPTERFROMHDC;

typedef struct D3DKMT_CLOSEADAPTER {
  D3DKMT_HANDLE hAdapter;
} D3DKMT_CLOSEADAPTER;

typedef struct D3DKMT_WAITFORVERTICALBLANKEVENT {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  UINT VidPnSourceId;
} D3DKMT_WAITFORVERTICALBLANKEVENT;

struct D3DKMT_FUNCS {
  HMODULE gdi32;

  typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
  typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
  typedef NTSTATUS(WINAPI* PFND3DKMTWaitForVerticalBlankEvent)(
      D3DKMT_WAITFORVERTICALBLANKEVENT* pData);

  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
};

static bool LoadD3DKMT(D3DKMT_FUNCS* out) {
  if (!out) {
    return false;
  }

  ZeroMemory(out, sizeof(*out));
  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    return false;
  }

  out->OpenAdapterFromHdc =
      (D3DKMT_FUNCS::PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter =
      (D3DKMT_FUNCS::PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->WaitForVerticalBlankEvent =
      (D3DKMT_FUNCS::PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(out->gdi32, "D3DKMTWaitForVerticalBlankEvent");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError = (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->WaitForVerticalBlankEvent) {
    FreeLibrary(out->gdi32);
    ZeroMemory(out, sizeof(*out));
    return false;
  }

  return true;
}

static std::string NtStatusToString(const D3DKMT_FUNCS* f, NTSTATUS st) {
  char buf[64];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)st);
  std::string out(buf);

  if (f && f->RtlNtStatusToDosError) {
    DWORD win32 = (DWORD)f->RtlNtStatusToDosError(st);
    if (win32 != 0) {
      char hdr[64];
      _snprintf(hdr, sizeof(hdr), " (Win32=%lu: ", (unsigned long)win32);
      out += hdr;
      out += aerogpu_test::Win32ErrorToString(win32);
      out += ")";
    }
  }

  return out;
}

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

typedef struct WaitThreadCtx {
  const D3DKMT_FUNCS* f;
  D3DKMT_HANDLE hAdapter;
  UINT vid_pn_source_id;
  HANDLE request_event;
  HANDLE done_event;
  HANDLE thread;
  volatile LONG stop;
  volatile LONG last_status;
} WaitThreadCtx;

static DWORD WINAPI WaitThreadProc(LPVOID param) {
  WaitThreadCtx* ctx = (WaitThreadCtx*)param;
  for (;;) {
    DWORD w = WaitForSingleObject(ctx->request_event, INFINITE);
    if (w != WAIT_OBJECT_0) {
      InterlockedExchange(&ctx->last_status, (LONG)0xC0000001L /* STATUS_UNSUCCESSFUL */);
      SetEvent(ctx->done_event);
      continue;
    }

    if (InterlockedCompareExchange(&ctx->stop, 0, 0) != 0) {
      break;
    }

    D3DKMT_WAITFORVERTICALBLANKEVENT e;
    ZeroMemory(&e, sizeof(e));
    e.hAdapter = ctx->hAdapter;
    e.hDevice = 0;
    e.VidPnSourceId = ctx->vid_pn_source_id;
    NTSTATUS st = ctx->f->WaitForVerticalBlankEvent(&e);
    InterlockedExchange(&ctx->last_status, st);
    SetEvent(ctx->done_event);
  }
  return 0;
}

static bool StartWaitThread(WaitThreadCtx* out,
                            const D3DKMT_FUNCS* f,
                            D3DKMT_HANDLE hAdapter,
                            UINT vid_pn_source_id,
                            std::string* err) {
  if (!out || !f) {
    if (err) {
      *err = "invalid args";
    }
    return false;
  }

  ZeroMemory(out, sizeof(*out));
  out->f = f;
  out->hAdapter = hAdapter;
  out->vid_pn_source_id = vid_pn_source_id;
  out->stop = 0;
  out->last_status = 0;
  out->request_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  out->done_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  if (!out->request_event || !out->done_event) {
    if (err) {
      *err = "CreateEventW failed";
    }
    if (out->request_event) {
      CloseHandle(out->request_event);
      out->request_event = NULL;
    }
    if (out->done_event) {
      CloseHandle(out->done_event);
      out->done_event = NULL;
    }
    return false;
  }

  out->thread = CreateThread(NULL, 0, WaitThreadProc, out, 0, NULL);
  if (!out->thread) {
    if (err) {
      *err = "CreateThread failed";
    }
    CloseHandle(out->request_event);
    out->request_event = NULL;
    CloseHandle(out->done_event);
    out->done_event = NULL;
    return false;
  }
  return true;
}

static void StopWaitThread(WaitThreadCtx* ctx) {
  if (!ctx) {
    return;
  }

  if (ctx->thread) {
    InterlockedExchange(&ctx->stop, 1);
    SetEvent(ctx->request_event);
    WaitForSingleObject(ctx->thread, 5000);
    CloseHandle(ctx->thread);
    ctx->thread = NULL;
  }

  if (ctx->request_event) {
    CloseHandle(ctx->request_event);
    ctx->request_event = NULL;
  }
  if (ctx->done_event) {
    CloseHandle(ctx->done_event);
    ctx->done_event = NULL;
  }
}

static int RunWaitVblankPacing(int argc, char** argv) {
  const char* kTestName = "wait_vblank_pacing";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--wait-timeout-ms=N] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=120");
    aerogpu_test::PrintfStdout("Measures KMD vblank pacing by timing D3DKMTWaitForVerticalBlankEvent().");
    return 0;
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  uint32_t samples = 120;
  uint32_t wait_timeout_ms = 2000;
  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }
  std::string wait_timeout_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--wait-timeout-ms", &wait_timeout_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(wait_timeout_str, &wait_timeout_ms, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --wait-timeout-ms: %s", err.c_str());
    }
  }

  if (samples < 5) {
    samples = 5;
  }
  if (wait_timeout_ms < 1) {
    wait_timeout_ms = 1;
  }

  // Some remote display paths do not deliver vblank semantics in a meaningful way.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      aerogpu_test::PrintfStdout("PASS: %s", kTestName);
      return 0;
    }
    return aerogpu_test::Fail(
        kTestName,
        "running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f)) {
    return aerogpu_test::Fail(
        kTestName,
        "failed to resolve D3DKMT* thunks from gdi32.dll (requires Windows Vista+ WDDM)");
  }

  D3DKMT_HANDLE h_adapter = 0;

  // Open the default display adapter via the screen HDC.
  HDC hdc = GetDC(NULL);
  if (!hdc) {
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName, "GetDC(NULL) failed");
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  ReleaseDC(NULL, hdc);
  if (!NT_SUCCESS(st)) {
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTOpenAdapterFromHdc failed with %s",
                              NtStatusToString(&f, st).c_str());
  }
  h_adapter = open.hAdapter;

  if (open.VidPnSourceId != 0) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: OpenAdapterFromHdc returned VidPnSourceId=%u (test targets VidPnSourceId=0)",
        kTestName,
        (unsigned)open.VidPnSourceId);
  }

  // Run the vblank wait on a dedicated thread so we can bound the wall time of each wait.
  // If vblank interrupts are missing/broken, D3DKMTWaitForVerticalBlankEvent can block indefinitely.
  WaitThreadCtx waiter;
  std::string waiter_err;
  if (!StartWaitThread(&waiter, &f, h_adapter, 0 /* VidPnSourceId */, &waiter_err)) {
    D3DKMT_CLOSEADAPTER close;
    ZeroMemory(&close, sizeof(close));
    close.hAdapter = h_adapter;
    f.CloseAdapter(&close);
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName, "failed to start wait thread: %s", waiter_err.c_str());
  }

  // Warm up once to avoid counting first-time initialization.
  SetEvent(waiter.request_event);
  DWORD w = WaitForSingleObject(waiter.done_event, (DWORD)wait_timeout_ms);
  if (w == WAIT_TIMEOUT) {
    // Avoid trying to clean up the wait thread: it may be blocked in the kernel thunk. Exiting the
    // process is sufficient for test automation, and avoids deadlock-prone teardown paths.
    return aerogpu_test::Fail(kTestName,
                              "vblank wait timed out after %lu ms (warmup)",
                              (unsigned long)wait_timeout_ms);
  }
  if (w != WAIT_OBJECT_0) {
    StopWaitThread(&waiter);
    D3DKMT_CLOSEADAPTER close;
    ZeroMemory(&close, sizeof(close));
    close.hAdapter = h_adapter;
    f.CloseAdapter(&close);
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName,
                              "WaitForSingleObject failed (rc=%lu)",
                              (unsigned long)w);
  }
  st = (NTSTATUS)InterlockedCompareExchange(&waiter.last_status, 0, 0);
  if (!NT_SUCCESS(st)) {
    StopWaitThread(&waiter);
    D3DKMT_CLOSEADAPTER close;
    ZeroMemory(&close, sizeof(close));
    close.hAdapter = h_adapter;
    f.CloseAdapter(&close);
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTWaitForVerticalBlankEvent(warmup) failed with %s",
                              NtStatusToString(&f, st).c_str());
  }

  double sum = 0.0;
  double min_ms = 1e9;
  double max_ms = 0.0;
  uint32_t collected = 0;

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);

  int rc = 0;
  for (uint32_t i = 0; i < samples; ++i) {
    SetEvent(waiter.request_event);
    w = WaitForSingleObject(waiter.done_event, (DWORD)wait_timeout_ms);
    if (w == WAIT_TIMEOUT) {
      // Avoid trying to clean up the wait thread: it may be blocked in the kernel thunk.
      return aerogpu_test::Fail(kTestName,
                                "vblank wait timed out after %lu ms (sample %lu/%lu)",
                                (unsigned long)wait_timeout_ms,
                                (unsigned long)(i + 1),
                                (unsigned long)samples);
    }
    if (w != WAIT_OBJECT_0) {
      rc = aerogpu_test::Fail(kTestName,
                              "WaitForSingleObject failed (rc=%lu)",
                              (unsigned long)w);
      break;
    }
    st = (NTSTATUS)InterlockedCompareExchange(&waiter.last_status, 0, 0);
    if (!NT_SUCCESS(st)) {
      rc = aerogpu_test::Fail(kTestName,
                              "D3DKMTWaitForVerticalBlankEvent failed with %s",
                              NtStatusToString(&f, st).c_str());
      break;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt_ms = QpcToMs(now.QuadPart - last.QuadPart, qpc_freq);
    sum += dt_ms;
    if (dt_ms < min_ms) min_ms = dt_ms;
    if (dt_ms > max_ms) max_ms = dt_ms;
    collected++;
    last = now;

    // If we already observed a very large gap, fail early to avoid a long/hung run.
    if (max_ms > 250.0) {
      break;
    }
  }

  StopWaitThread(&waiter);

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = h_adapter;
  NTSTATUS close_st = f.CloseAdapter(&close);
  if (!NT_SUCCESS(close_st)) {
    aerogpu_test::PrintfStdout("INFO: %s: D3DKMTCloseAdapter failed with %s",
                               kTestName,
                               NtStatusToString(&f, close_st).c_str());
  }

  FreeLibrary(f.gdi32);

  if (rc != 0) {
    return rc;
  }

  if (collected == 0) {
    return aerogpu_test::Fail(kTestName, "no samples collected");
  }

  const double avg_ms = sum / (double)collected;

  aerogpu_test::PrintfStdout(
      "INFO: %s: D3DKMTWaitForVerticalBlankEvent pacing over %u samples: avg=%.3fms min=%.3fms "
      "max=%.3fms",
      kTestName,
      (unsigned)collected,
      avg_ms,
      min_ms,
      max_ms);

  if (avg_ms < 2.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly fast vblank pacing (avg=%.3fms)", avg_ms);
  }
  if (max_ms > 250.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly large vblank gap (max=%.3fms)", max_ms);
  }

  if (avg_ms < 10.0 || avg_ms > 25.0) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: note: avg=%.3fms (expected ~16.7ms for 60 Hz). This may be normal on non-60Hz displays.",
        kTestName,
        avg_ms);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunWaitVblankPacing(argc, argv);
}
