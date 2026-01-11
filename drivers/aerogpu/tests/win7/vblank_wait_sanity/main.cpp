#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

typedef LONG NTSTATUS;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
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

typedef struct D3DKMT_WAITFORVERTICALBLANKEVENT {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  UINT VidPnSourceId;
} D3DKMT_WAITFORVERTICALBLANKEVENT;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTWaitForVerticalBlankEvent)(D3DKMT_WAITFORVERTICALBLANKEVENT* pData);
typedef ULONG(WINAPI* PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
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
  out->WaitForVerticalBlankEvent =
      (PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(out->gdi32, "D3DKMTWaitForVerticalBlankEvent");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError =
        (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->WaitForVerticalBlankEvent) {
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

static std::string NtStatusToString(const D3DKMT_FUNCS* f, NTSTATUS st) {
  char buf[512];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)st);
  buf[sizeof(buf) - 1] = 0;

  if (!f || !f->RtlNtStatusToDosError) {
    return std::string(buf);
  }

  DWORD win32 = f->RtlNtStatusToDosError(st);
  if (win32 == 0) {
    return std::string(buf);
  }

  const std::string msg = aerogpu_test::Win32ErrorToString(win32);
  char buf2[512];
  _snprintf(buf2, sizeof(buf2), "%s (Win32=%lu: %s)",
            buf,
            (unsigned long)win32,
            msg.c_str());
  buf2[sizeof(buf2) - 1] = 0;
  return std::string(buf2);
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

static int RunVblankWaitSanity(int argc, char** argv) {
  const char* kTestName = "vblank_wait_sanity";

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--timeout-ms=N] [--wait-timeout-ms=N] [--allow-remote] "
        "[--require-vid=0x####] [--require-did=0x####]",
        kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=120 --timeout-ms=2000");
    aerogpu_test::PrintfStdout(
        "Measures WDDM vblank delivery directly via D3DKMTWaitForVerticalBlankEvent.");
    aerogpu_test::PrintfStdout("Note: --wait-timeout-ms is accepted as an alias for --timeout-ms.");
    return 0;
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");

  uint32_t samples = 120;
  uint32_t timeout_ms = 2000;

  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }

  std::string timeout_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--timeout-ms", &timeout_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(timeout_str, &timeout_ms, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --timeout-ms: %s", err.c_str());
    }
  }
  std::string wait_timeout_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--wait-timeout-ms", &wait_timeout_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(wait_timeout_str, &timeout_ms, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --wait-timeout-ms: %s", err.c_str());
    }
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

  if (samples < 5) {
    samples = 5;
  }
  if (timeout_ms < 1) {
    timeout_ms = 1;
  }

  // Like dwm_flush_pacing, skip when running under RDP unless explicitly allowed.
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

  // Report adapter identity using D3D9Ex, mirroring the other tests.
  {
    ComPtr<IDirect3D9Ex> d3d;
    HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
    if (SUCCEEDED(hr)) {
      D3DADAPTER_IDENTIFIER9 ident;
      ZeroMemory(&ident, sizeof(ident));
      hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
      if (SUCCEEDED(hr)) {
        aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                                   kTestName,
                                   ident.Description,
                                   (unsigned)ident.VendorId,
                                   (unsigned)ident.DeviceId);
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
      } else if (has_require_vid || has_require_did) {
        return aerogpu_test::FailHresult(
            kTestName,
            "GetAdapterIdentifier (required for --require-vid/--require-did)",
            hr);
      }
    } else if (has_require_vid || has_require_did) {
      return aerogpu_test::FailHresult(
          kTestName,
          "Direct3DCreate9Ex (required for --require-vid/--require-did)",
          hr);
    }
  }

  D3DKMT_FUNCS f;
  std::string load_err;
  if (!LoadD3DKMT(&f, &load_err)) {
    return aerogpu_test::Fail(kTestName, "%s", load_err.c_str());
  }

  HDC hdc = GetDC(NULL);
  if (!hdc) {
    return aerogpu_test::Fail(kTestName, "GetDC(NULL) failed");
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  ReleaseDC(NULL, hdc);
  if (!NT_SUCCESS(st)) {
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTOpenAdapterFromHdc failed with %s",
                              NtStatusToString(&f, st).c_str());
  }

  aerogpu_test::PrintfStdout("INFO: %s: D3DKMT: hAdapter=0x%08X VidPnSourceId=%u LUID=0x%08lX%08lX",
                             kTestName,
                             (unsigned)open.hAdapter,
                             (unsigned)open.VidPnSourceId,
                             (unsigned long)open.AdapterLuid.HighPart,
                             (unsigned long)open.AdapterLuid.LowPart);

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  WaitThreadCtx waiter;
  std::string waiter_err;
  if (!StartWaitThread(&waiter, &f, open.hAdapter, open.VidPnSourceId, &waiter_err)) {
    return aerogpu_test::Fail(kTestName, "failed to start wait thread: %s", waiter_err.c_str());
  }

  std::vector<double> deltas_ms;
  deltas_ms.reserve(samples);

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);

  for (uint32_t i = 0; i < samples; ++i) {
    SetEvent(waiter.request_event);
    DWORD w = WaitForSingleObject(waiter.done_event, timeout_ms);
    if (w == WAIT_TIMEOUT) {
      // Avoid trying to clean up the wait thread: it may be blocked in the kernel thunk. Exiting
      // the process is sufficient for test automation, and avoids deadlock-prone teardown paths.
      return aerogpu_test::Fail(kTestName,
                                "vblank wait timed out after %lu ms (sample %lu/%lu)",
                                (unsigned long)timeout_ms,
                                (unsigned long)(i + 1),
                                (unsigned long)samples);
    }
    if (w != WAIT_OBJECT_0) {
      StopWaitThread(&waiter);
      return aerogpu_test::Fail(kTestName,
                                "WaitForSingleObject failed (rc=%lu)",
                                (unsigned long)w);
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter.last_status, 0, 0);
    if (!NT_SUCCESS(st)) {
      StopWaitThread(&waiter);
      return aerogpu_test::Fail(kTestName,
                                "D3DKMTWaitForVerticalBlankEvent failed with %s",
                                NtStatusToString(&f, st).c_str());
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt_ms = QpcToMs(now.QuadPart - last.QuadPart, qpc_freq);
    deltas_ms.push_back(dt_ms);
    last = now;
  }

  StopWaitThread(&waiter);

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  st = f.CloseAdapter(&close);
  if (!NT_SUCCESS(st)) {
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTCloseAdapter failed with %s",
                              NtStatusToString(&f, st).c_str());
  }

  double sum = 0.0;
  double min_ms = 1e9;
  double max_ms = 0.0;
  for (size_t i = 0; i < deltas_ms.size(); ++i) {
    const double v = deltas_ms[i];
    sum += v;
    if (v < min_ms) {
      min_ms = v;
    }
    if (v > max_ms) {
      max_ms = v;
    }
  }
  const double avg_ms = sum / (double)deltas_ms.size();

  aerogpu_test::PrintfStdout(
      "INFO: %s: vblank waits over %u samples: avg=%.3fms min=%.3fms max=%.3fms (timeout=%lu ms)",
      kTestName,
      (unsigned)samples,
      avg_ms,
      min_ms,
      max_ms,
      (unsigned long)timeout_ms);

  // Heuristic pass/fail:
  //
  // - If the wait returns almost immediately, we are not actually waiting for vblank.
  // - If we see multi-hundred-ms gaps, vblank interrupts are likely missing/stalled.
  //
  // Keep these thresholds generous: this test is intended to detect "completely broken" vblank
  // wiring, not to enforce perfect refresh accuracy.
  if (avg_ms < 2.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly fast vblank pacing (avg=%.3fms)", avg_ms);
  }
  if (max_ms > 250.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly large vblank gap (max=%.3fms)", max_ms);
  }
  if (avg_ms < 5.0 || avg_ms > 40.0) {
    aerogpu_test::PrintfStdout("INFO: %s: WARNING: unusual vblank average (avg=%.3fms)", kTestName, avg_ms);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunVblankWaitSanity(argc, argv);
}
