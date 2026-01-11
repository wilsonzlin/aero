#include "..\\common\\aerogpu_test_common.h"

// This test directly exercises the WDDM kernel vblank wait path by calling
// D3DKMTWaitForVerticalBlankEvent in a tight loop and measuring the pacing.
//
// It is intentionally implemented without the WDK and dynamically loads the
// required D3DKMT entry points from gdi32.dll (similar to win7_dbgctl).

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

typedef LONG(WINAPI *PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC *pData);
typedef LONG(WINAPI *PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER *pData);
typedef LONG(WINAPI *PFND3DKMTWaitForVerticalBlankEvent)(D3DKMT_WAITFORVERTICALBLANKEVENT *pData);
typedef ULONG(WINAPI *PFNRtlNtStatusToDosError)(LONG Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
} D3DKMT_FUNCS;

static bool NtSuccess(LONG st) { return st >= 0; }

static std::string NtStatusToString(const D3DKMT_FUNCS *f, LONG st) {
  char buf[64];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)st);
  std::string out = buf;

  if (!f || !f->RtlNtStatusToDosError) {
    return out;
  }
  const DWORD win32 = (DWORD)f->RtlNtStatusToDosError(st);
  if (win32 == 0) {
    return out;
  }

  out += " (Win32 ";
  out += aerogpu_test::Win32ErrorToString(win32);
  out += ")";
  return out;
}

static bool LoadD3DKMT(D3DKMT_FUNCS *out) {
  if (!out) {
    return false;
  }
  ZeroMemory(out, sizeof(*out));
  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->WaitForVerticalBlankEvent =
      (PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(out->gdi32, "D3DKMTWaitForVerticalBlankEvent");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError = (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->WaitForVerticalBlankEvent) {
    return false;
  }
  return true;
}

static bool GetPrimaryDisplayName(wchar_t out[CCHDEVICENAME]) {
  if (!out) {
    return false;
  }

  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  wcsncpy(out, L"\\\\.\\DISPLAY1", CCHDEVICENAME - 1);
  out[CCHDEVICENAME - 1] = 0;
  return true;
}

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static int RunVblankWaitPacing(int argc, char **argv) {
  const char *kTestName = "vblank_wait_pacing";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--samples=N] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=120");
    aerogpu_test::PrintfStdout(
        "Measures kernel vblank pacing by timing successive D3DKMTWaitForVerticalBlankEvent() calls.");
    return 0;
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  uint32_t samples = 120;
  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }

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

  if (samples < 5) {
    samples = 5;
  }

  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f)) {
    return aerogpu_test::Fail(kTestName, "failed to load required D3DKMT* exports from gdi32.dll");
  }

  wchar_t display_name[CCHDEVICENAME];
  GetPrimaryDisplayName(display_name);
  HDC hdc = CreateDCW(L"DISPLAY", display_name, NULL, NULL);
  if (!hdc) {
    return aerogpu_test::Fail(kTestName, "CreateDCW failed for display %ls: %s", display_name,
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  LONG st = f.OpenAdapterFromHdc(&open);
  DeleteDC(hdc);
  if (!NtSuccess(st)) {
    return aerogpu_test::Fail(kTestName, "D3DKMTOpenAdapterFromHdc failed with %s", NtStatusToString(&f, st).c_str());
  }

  std::vector<double> deltas_ms;
  deltas_ms.reserve(samples);

  int rc = 0;
  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);
  for (uint32_t i = 0; i < samples; ++i) {
    D3DKMT_WAITFORVERTICALBLANKEVENT wait;
    ZeroMemory(&wait, sizeof(wait));
    wait.hAdapter = open.hAdapter;
    wait.hDevice = 0;
    wait.VidPnSourceId = open.VidPnSourceId;
    st = f.WaitForVerticalBlankEvent(&wait);
    if (!NtSuccess(st)) {
      rc = aerogpu_test::Fail(kTestName, "D3DKMTWaitForVerticalBlankEvent failed with %s", NtStatusToString(&f, st).c_str());
      break;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt = QpcToMs(now.QuadPart - last.QuadPart, qpc_freq);
    deltas_ms.push_back(dt);
    last = now;
  }

  if (rc == 0) {
    double sum = 0.0;
    double min_ms = 1e9;
    double max_ms = 0.0;
    for (size_t i = 0; i < deltas_ms.size(); ++i) {
      const double v = deltas_ms[i];
      sum += v;
      if (v < min_ms) min_ms = v;
      if (v > max_ms) max_ms = v;
    }
    const double avg_ms = sum / (double)deltas_ms.size();

    aerogpu_test::PrintfStdout(
        "INFO: %s: WaitForVerticalBlankEvent pacing over %u samples: avg=%.3fms min=%.3fms max=%.3fms",
        kTestName, (unsigned)samples, avg_ms, min_ms, max_ms);

    if (avg_ms <= 2.0) {
      rc = aerogpu_test::Fail(kTestName, "unexpectedly fast vblank pacing (avg=%.3fms)", avg_ms);
    } else if (avg_ms >= 50.0) {
      rc = aerogpu_test::Fail(kTestName, "unexpectedly slow vblank pacing (avg=%.3fms)", avg_ms);
    } else if (max_ms >= 250.0) {
      rc = aerogpu_test::Fail(kTestName, "unexpectedly large vblank gap (max=%.3fms)", max_ms);
    } else {
      aerogpu_test::PrintfStdout("PASS: %s", kTestName);
      rc = 0;
    }
  }

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  st = f.CloseAdapter(&close);
  if (!NtSuccess(st) && rc == 0) {
    rc = aerogpu_test::Fail(kTestName, "D3DKMTCloseAdapter failed with %s", NtStatusToString(&f, st).c_str());
  }
  return rc;
}

int main(int argc, char **argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunVblankWaitPacing(argc, argv);
}
