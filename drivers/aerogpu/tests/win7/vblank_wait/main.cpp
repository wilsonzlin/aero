#include "..\\common\\aerogpu_test_common.h"

// This test validates the WDDM vblank wait path directly via D3DKMTWaitForVerticalBlankEvent.
//
// We intentionally avoid a WDK dependency by:
//   - Dynamically loading required D3DKMT* entry points from gdi32.dll.
//   - Defining only the minimal structs needed for the APIs we call.

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

typedef struct D3DKMT_GETSCANLINE {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  UINT VidPnSourceId;
  BOOL InVerticalBlank;
  UINT ScanLine;
} D3DKMT_GETSCANLINE;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTWaitForVerticalBlankEvent)(D3DKMT_WAITFORVERTICALBLANKEVENT* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTGetScanLine)(D3DKMT_GETSCANLINE* pData);
typedef ULONG(WINAPI* PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFND3DKMTGetScanLine GetScanLine;  // optional
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
} D3DKMT_FUNCS;

static std::wstring AcpToWide(const std::string& s) {
  if (s.empty()) {
    return std::wstring();
  }
  int need = MultiByteToWideChar(CP_ACP, 0, s.c_str(), (int)s.size(), NULL, 0);
  if (need <= 0) {
    return std::wstring();
  }
  std::wstring out;
  out.resize((size_t)need);
  MultiByteToWideChar(CP_ACP, 0, s.c_str(), (int)s.size(), &out[0], need);
  return out;
}

static std::wstring GetPrimaryDisplayName() {
  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0) {
      return std::wstring(dd.DeviceName);
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      return std::wstring(dd.DeviceName);
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  return L"\\\\.\\DISPLAY1";
}

static bool LoadD3DKMT(D3DKMT_FUNCS* out, std::string* err) {
  if (!out) {
    if (err) *err = "LoadD3DKMT: out == NULL";
    return false;
  }
  ZeroMemory(out, sizeof(*out));

  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    if (err) *err = "LoadLibraryW(gdi32.dll) failed";
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->WaitForVerticalBlankEvent = (PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(
      out->gdi32, "D3DKMTWaitForVerticalBlankEvent");
  out->GetScanLine = (PFND3DKMTGetScanLine)GetProcAddress(out->gdi32, "D3DKMTGetScanLine");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError = (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->WaitForVerticalBlankEvent) {
    if (err) {
      *err =
          "Required D3DKMT* exports not found in gdi32.dll (need D3DKMTOpenAdapterFromHdc, "
          "D3DKMTCloseAdapter, D3DKMTWaitForVerticalBlankEvent).";
    }
    return false;
  }

  return true;
}

static std::string NtStatusToString(const D3DKMT_FUNCS* f, NTSTATUS st) {
  char buf[64];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)st);
  std::string out(buf);

  DWORD win32 = 0;
  if (f && f->RtlNtStatusToDosError) {
    win32 = (DWORD)f->RtlNtStatusToDosError(st);
  }
  if (win32 != 0) {
    char buf2[64];
    _snprintf(buf2, sizeof(buf2), " (Win32=%lu: ", (unsigned long)win32);
    out += buf2;
    out += aerogpu_test::Win32ErrorToString(win32);
    out += ")";
  }
  return out;
}

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static int RunVblankWait(int argc, char** argv) {
  const char* kTestName = "vblank_wait";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--display \\\\.\\DISPLAYn] [--samples=N] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout("Default: --display=primary --samples=120");
    aerogpu_test::PrintfStdout("Measures vblank pacing by timing successive D3DKMTWaitForVerticalBlankEvent calls.");
    return 0;
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
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

  uint32_t samples = 120;
  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }

  std::wstring display = GetPrimaryDisplayName();
  std::string display_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--display", &display_str)) {
    if (display_str.empty()) {
      return aerogpu_test::Fail(kTestName, "invalid --display: missing value");
    }
    display = AcpToWide(display_str);
    if (display.empty()) {
      return aerogpu_test::Fail(kTestName, "invalid --display: could not convert to wide string");
    }
  }

  if (samples < 5) {
    samples = 5;
  }

  std::string fail_msg;
  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f, &fail_msg)) {
    return aerogpu_test::Fail(kTestName, "%s", fail_msg.c_str());
  }

  DEVMODEW dm;
  ZeroMemory(&dm, sizeof(dm));
  dm.dmSize = sizeof(dm);
  if (EnumDisplaySettingsW(display.c_str(), ENUM_CURRENT_SETTINGS, &dm)) {
    if (dm.dmDisplayFrequency > 1) {
      aerogpu_test::PrintfStdout("INFO: %s: display=%ls mode=%lux%lu@%luHz",
                                 kTestName,
                                 display.c_str(),
                                 (unsigned long)dm.dmPelsWidth,
                                 (unsigned long)dm.dmPelsHeight,
                                 (unsigned long)dm.dmDisplayFrequency);
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: display=%ls mode=%lux%lu@(default Hz)",
                                 kTestName,
                                 display.c_str(),
                                 (unsigned long)dm.dmPelsWidth,
                                 (unsigned long)dm.dmPelsHeight);
    }
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: display=%ls (EnumDisplaySettingsW failed: %s)",
                               kTestName,
                               display.c_str(),
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  HDC hdc = CreateDCW(L"DISPLAY", display.c_str(), NULL, NULL);
  if (!hdc) {
    return aerogpu_test::Fail(kTestName,
                              "CreateDCW failed for %ls: %s",
                              display.c_str(),
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  DeleteDC(hdc);
  hdc = NULL;
  if (!NT_SUCCESS(st)) {
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTOpenAdapterFromHdc failed with %s",
                              NtStatusToString(&f, st).c_str());
  }

  aerogpu_test::PrintfStdout("INFO: %s: VidPnSourceId=%lu AdapterLuid=0x%08lX:0x%08lX",
                             kTestName,
                             (unsigned long)open.VidPnSourceId,
                             (unsigned long)open.AdapterLuid.HighPart,
                             (unsigned long)open.AdapterLuid.LowPart);

  if (f.GetScanLine) {
    D3DKMT_GETSCANLINE scan;
    ZeroMemory(&scan, sizeof(scan));
    scan.hAdapter = open.hAdapter;
    scan.hDevice = 0;
    scan.VidPnSourceId = open.VidPnSourceId;
    NTSTATUS scan_st = f.GetScanLine(&scan);
    if (NT_SUCCESS(scan_st)) {
      aerogpu_test::PrintfStdout("INFO: %s: scanline=%lu inVblank=%d",
                                 kTestName,
                                 (unsigned long)scan.ScanLine,
                                 (int)scan.InVerticalBlank);
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: D3DKMTGetScanLine failed with %s",
                                 kTestName,
                                 NtStatusToString(&f, scan_st).c_str());
    }
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    D3DKMT_CLOSEADAPTER close;
    ZeroMemory(&close, sizeof(close));
    close.hAdapter = open.hAdapter;
    f.CloseAdapter(&close);
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  D3DKMT_WAITFORVERTICALBLANKEVENT wait;
  ZeroMemory(&wait, sizeof(wait));
  wait.hAdapter = open.hAdapter;
  wait.hDevice = 0;
  wait.VidPnSourceId = open.VidPnSourceId;

  // Warm up once to avoid counting first-time initialization.
  st = f.WaitForVerticalBlankEvent(&wait);
  if (!NT_SUCCESS(st)) {
    D3DKMT_CLOSEADAPTER close;
    ZeroMemory(&close, sizeof(close));
    close.hAdapter = open.hAdapter;
    f.CloseAdapter(&close);
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTWaitForVerticalBlankEvent(warmup) failed with %s",
                              NtStatusToString(&f, st).c_str());
  }

  std::vector<double> deltas_ms;
  deltas_ms.reserve(samples);

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);
  for (uint32_t i = 0; i < samples; ++i) {
    st = f.WaitForVerticalBlankEvent(&wait);
    if (!NT_SUCCESS(st)) {
      fail_msg = "D3DKMTWaitForVerticalBlankEvent failed with " + NtStatusToString(&f, st);
      break;
    }
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    deltas_ms.push_back(QpcToMs(now.QuadPart - last.QuadPart, qpc_freq));
    last = now;
  }

  if (fail_msg.empty()) {
    double sum = 0.0;
    double min_ms = 1e9;
    double max_ms = 0.0;
    for (size_t i = 0; i < deltas_ms.size(); ++i) {
      const double v = deltas_ms[i];
      sum += v;
      if (v < min_ms) min_ms = v;
      if (v > max_ms) max_ms = v;
    }
    const double avg_ms = (deltas_ms.empty()) ? 0.0 : (sum / (double)deltas_ms.size());

    aerogpu_test::PrintfStdout(
        "INFO: %s: vblank pacing over %u samples: avg=%.3fms min=%.3fms max=%.3fms",
        kTestName,
        (unsigned)samples,
        avg_ms,
        min_ms,
        max_ms);

    // Heuristic pass/fail:
    //
    // - If the wait returns almost immediately, we're not actually blocking on vblank.
    // - If we see multi-hundred-ms gaps, something is stalling the vblank interrupt path.
    //
    // Keep these thresholds generous: the goal is to catch "completely broken" behavior.
    if (avg_ms < 2.0) {
      char buf[128];
      _snprintf(buf, sizeof(buf), "unexpectedly fast vblank pacing (avg=%.3fms)", avg_ms);
      fail_msg = buf;
    } else if (max_ms > 250.0) {
      char buf[128];
      _snprintf(buf, sizeof(buf), "unexpectedly large vblank gap (max=%.3fms)", max_ms);
      fail_msg = buf;
    } else if (dm.dmDisplayFrequency > 1) {
      const double expected_ms = 1000.0 / (double)dm.dmDisplayFrequency;
      const double diff = (avg_ms > expected_ms) ? (avg_ms - expected_ms) : (expected_ms - avg_ms);
      // Warn (but do not fail) if we're far from the configured refresh rate.
      if (diff > 5.0 && diff > expected_ms * 0.25) {
        aerogpu_test::PrintfStdout("INFO: %s: WARNING: avg %.3fms deviates from expected %.3fms (%luHz)",
                                   kTestName,
                                   avg_ms,
                                   expected_ms,
                                   (unsigned long)dm.dmDisplayFrequency);
      }
    }
  }

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  NTSTATUS close_st = f.CloseAdapter(&close);
  if (!NT_SUCCESS(close_st)) {
    if (fail_msg.empty()) {
      fail_msg = "D3DKMTCloseAdapter failed with " + NtStatusToString(&f, close_st);
    } else {
      fail_msg += " (and D3DKMTCloseAdapter failed with " + NtStatusToString(&f, close_st) + ")";
    }
  }

  if (!fail_msg.empty()) {
    return aerogpu_test::Fail(kTestName, "%s", fail_msg.c_str());
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunVblankWait(argc, argv);
}

