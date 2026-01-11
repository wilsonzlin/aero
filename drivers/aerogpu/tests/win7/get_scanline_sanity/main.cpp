#include "..\\common\\aerogpu_test_common.h"

#include <algorithm>

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

typedef struct D3DKMT_GETSCANLINE {
  D3DKMT_HANDLE hAdapter;
  UINT VidPnSourceId;
  BOOL InVerticalBlank;
  UINT ScanLine;
} D3DKMT_GETSCANLINE;

typedef NTSTATUS(WINAPI* PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER* pData);
typedef NTSTATUS(WINAPI* PFND3DKMTGetScanLine)(D3DKMT_GETSCANLINE* pData);
typedef ULONG(WINAPI* PFNRtlNtStatusToDosError)(NTSTATUS Status);

struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTGetScanLine GetScanLine;
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
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->GetScanLine = (PFND3DKMTGetScanLine)GetProcAddress(out->gdi32, "D3DKMTGetScanLine");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError =
        (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  const bool ok = out->OpenAdapterFromHdc && out->CloseAdapter && out->GetScanLine;
  if (!ok) {
    FreeLibrary(out->gdi32);
    ZeroMemory(out, sizeof(*out));
  }
  return ok;
}

static std::string NtStatusToString(NTSTATUS st, PFNRtlNtStatusToDosError conv) {
  char buf[128];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)st);
  std::string out = buf;

  if (conv) {
    DWORD win32 = conv(st);
    if (win32 != 0) {
      out += " (Win32=";
      char num[32];
      _snprintf(num, sizeof(num), "%lu", (unsigned long)win32);
      out += num;
      out += ": ";
      out += aerogpu_test::Win32ErrorToString(win32);
      out += ")";
    }
  }

  return out;
}

static bool GetPrimaryDisplayName(wchar_t out[CCHDEVICENAME]) {
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

static int RunGetScanlineSanity(int argc, char** argv) {
  const char* kTestName = "get_scanline_sanity";

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--samples=N] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=200 (min 20)");
    aerogpu_test::PrintfStdout("Calls D3DKMTGetScanLine repeatedly and validates sane, varying results.");
    return 0;
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  uint32_t samples = 200;
  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }
  if (samples < 20) {
    samples = 20;
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

  const int screen_height = GetSystemMetrics(SM_CYSCREEN);

  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f)) {
    return aerogpu_test::Fail(kTestName, "failed to load D3DKMT exports from gdi32.dll");
  }

  wchar_t display_name[CCHDEVICENAME];
  if (!GetPrimaryDisplayName(display_name)) {
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName, "failed to determine primary display name");
  }

  HDC hdc = CreateDCW(L"DISPLAY", display_name, NULL, NULL);
  if (!hdc) {
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName,
                              "CreateDCW failed for %ls: %s",
                              display_name,
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  DeleteDC(hdc);
  if (!NT_SUCCESS(st)) {
    FreeLibrary(f.gdi32);
    return aerogpu_test::Fail(kTestName,
                              "D3DKMTOpenAdapterFromHdc failed with %s",
                              NtStatusToString(st, f.RtlNtStatusToDosError).c_str());
  }

  uint32_t in_vblank = 0;
  uint32_t out_vblank = 0;
  uint32_t min_scanline = 0xFFFFFFFFu;
  uint32_t max_scanline = 0;
  std::vector<uint32_t> out_scanlines;
  out_scanlines.reserve(samples);

  int rc = 0;
  for (uint32_t i = 0; i < samples; ++i) {
    D3DKMT_GETSCANLINE s;
    ZeroMemory(&s, sizeof(s));
    s.hAdapter = open.hAdapter;
    s.VidPnSourceId = 0;

    st = f.GetScanLine(&s);
    if (!NT_SUCCESS(st)) {
      rc = aerogpu_test::Fail(kTestName,
                              "D3DKMTGetScanLine failed with %s",
                              NtStatusToString(st, f.RtlNtStatusToDosError).c_str());
      break;
    }

    if (s.InVerticalBlank) {
      ++in_vblank;
    } else {
      ++out_vblank;
      out_scanlines.push_back((uint32_t)s.ScanLine);
      if (screen_height > 0 && s.ScanLine >= (UINT)screen_height) {
        rc = aerogpu_test::Fail(
            kTestName, "ScanLine out of bounds: %u (screen height %d)", (unsigned)s.ScanLine, screen_height);
        break;
      }
      if ((uint32_t)s.ScanLine < min_scanline) {
        min_scanline = (uint32_t)s.ScanLine;
      }
      if ((uint32_t)s.ScanLine > max_scanline) {
        max_scanline = (uint32_t)s.ScanLine;
      }
    }

    Sleep((DWORD)((i * 7) % 5));
  }

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  NTSTATUS close_st = f.CloseAdapter(&close);
  FreeLibrary(f.gdi32);

  if (!NT_SUCCESS(close_st)) {
    if (rc == 0) {
      rc = aerogpu_test::Fail(kTestName,
                              "D3DKMTCloseAdapter failed with %s",
                              NtStatusToString(close_st, f.RtlNtStatusToDosError).c_str());
    } else {
      aerogpu_test::PrintfStdout("WARN: %s: D3DKMTCloseAdapter failed with %s",
                                 kTestName,
                                 NtStatusToString(close_st, f.RtlNtStatusToDosError).c_str());
    }
  }

  if (rc != 0) {
    return rc;
  }

  std::vector<uint32_t> distinct = out_scanlines;
  std::sort(distinct.begin(), distinct.end());
  distinct.erase(std::unique(distinct.begin(), distinct.end()), distinct.end());

  aerogpu_test::PrintfStdout(
      "INFO: %s: samples=%u screen_height=%d in_vblank=%u out_vblank=%u out_scanline[min=%u max=%u] distinct_out_scanlines=%u",
      kTestName,
      (unsigned)samples,
      screen_height,
      (unsigned)in_vblank,
      (unsigned)out_vblank,
      (unsigned)(out_vblank ? min_scanline : 0),
      (unsigned)(out_vblank ? max_scanline : 0),
      (unsigned)distinct.size());

  if (in_vblank == 0) {
    aerogpu_test::PrintfStdout("WARN: %s: never observed InVerticalBlank=TRUE (may be normal with short vblank)", kTestName);
  }

  if (out_vblank == 0) {
    return aerogpu_test::Fail(kTestName, "never observed InVerticalBlank=FALSE");
  }

  if (distinct.size() <= 1) {
    return aerogpu_test::Fail(kTestName, "ScanLine appears static (distinct out-of-vblank scanlines=%u)", (unsigned)distinct.size());
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunGetScanlineSanity(argc, argv);
}
