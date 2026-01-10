#ifndef UNICODE
#define UNICODE
#endif

#ifndef _UNICODE
#define _UNICODE
#endif

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

#include "aerogpu_dbgctl_escape.h"

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
  VOID *pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_ESCAPE;

typedef NTSTATUS(WINAPI *PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTEscape)(D3DKMT_ESCAPE *pData);
typedef ULONG(WINAPI *PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
} D3DKMT_FUNCS;

static void PrintUsage() {
  fwprintf(stderr,
           L"Usage:\n"
           L"  aerogpu_dbgctl [--display \\\\.\\DISPLAY1] [--ring-id N] [--timeout-ms N] <command>\n"
           L"\n"
           L"Commands:\n"
           L"  --query-version\n"
           L"  --query-fence\n"
           L"  --dump-ring\n"
           L"  --selftest\n");
}

static void PrintNtStatus(const wchar_t *prefix, const D3DKMT_FUNCS *f, NTSTATUS st) {
  DWORD win32 = 0;
  if (f->RtlNtStatusToDosError) {
    win32 = f->RtlNtStatusToDosError(st);
  }

  if (win32 != 0) {
    wchar_t msg[512];
    DWORD chars = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, win32, 0,
                                 msg, (DWORD)(sizeof(msg) / sizeof(msg[0])), NULL);
    if (chars != 0) {
      while (chars > 0 && (msg[chars - 1] == L'\r' || msg[chars - 1] == L'\n')) {
        msg[--chars] = 0;
      }
      fwprintf(stderr, L"%s: NTSTATUS=0x%08lx (Win32=%lu: %s)\n", prefix, (unsigned long)st,
               (unsigned long)win32, msg);
      return;
    }
  }

  fwprintf(stderr, L"%s: NTSTATUS=0x%08lx\n", prefix, (unsigned long)st);
}

static bool LoadD3DKMT(D3DKMT_FUNCS *out) {
  ZeroMemory(out, sizeof(*out));
  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    fwprintf(stderr, L"Failed to load gdi32.dll\n");
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->Escape = (PFND3DKMTEscape)GetProcAddress(out->gdi32, "D3DKMTEscape");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError = (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->Escape) {
    fwprintf(stderr,
             L"Required D3DKMT* exports not found in gdi32.dll.\n"
             L"This tool requires Windows Vista+ (WDDM).\n");
    return false;
  }

  return true;
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
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
  }

  wcsncpy(out, L"\\\\.\\DISPLAY1", CCHDEVICENAME - 1);
  out[CCHDEVICENAME - 1] = 0;
  return true;
}

static NTSTATUS SendAerogpuEscape(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, void *buf, UINT bufSize) {
  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = hAdapter;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = buf;
  e.PrivateDriverDataSize = bufSize;
  return f->Escape(&e);
}

static int DoQueryVersion(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  AEROGPU_DBGCTL_QUERY_VERSION q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.magic = AEROGPU_DBGCTL_ESCAPE_MAGIC;
  q.hdr.abiVersion = AEROGPU_DBGCTL_ESCAPE_ABI_VERSION;
  q.hdr.op = AEROGPU_DBGCTL_OP_QUERY_VERSION;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-version) failed", f, st);
    return 2;
  }

  wprintf(L"AeroGPU dbgctl ABI: %lu\n", (unsigned long)q.hdr.abiVersion);
  wprintf(L"Device ABI: %lu.%lu\n", (unsigned long)q.deviceAbiMajor, (unsigned long)q.deviceAbiMinor);
  wprintf(L"KMD version: %lu.%lu\n", (unsigned long)q.kmdVersionMajor, (unsigned long)q.kmdVersionMinor);
  wprintf(L"UMD version: %lu.%lu\n", (unsigned long)q.umdVersionMajor, (unsigned long)q.umdVersionMinor);
  return 0;
}

static int DoQueryFence(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  AEROGPU_DBGCTL_QUERY_FENCE q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.magic = AEROGPU_DBGCTL_ESCAPE_MAGIC;
  q.hdr.abiVersion = AEROGPU_DBGCTL_ESCAPE_ABI_VERSION;
  q.hdr.op = AEROGPU_DBGCTL_OP_QUERY_FENCE;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, st);
    return 2;
  }

  wprintf(L"Last submitted fence: 0x%I64x (%I64u)\n", (unsigned long long)q.lastSubmittedFence,
          (unsigned long long)q.lastSubmittedFence);
  wprintf(L"Last completed fence: 0x%I64x (%I64u)\n", (unsigned long long)q.lastCompletedFence,
          (unsigned long long)q.lastCompletedFence);
  return 0;
}

static int DoDumpRing(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId) {
  AEROGPU_DBGCTL_DUMP_RING q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.magic = AEROGPU_DBGCTL_ESCAPE_MAGIC;
  q.hdr.abiVersion = AEROGPU_DBGCTL_ESCAPE_ABI_VERSION;
  q.hdr.op = AEROGPU_DBGCTL_OP_DUMP_RING;
  q.ringId = ringId;
  q.descCapacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st);
    return 2;
  }

  wprintf(L"Ring %lu\n", (unsigned long)q.ringId);
  wprintf(L"  size: %lu bytes\n", (unsigned long)q.ringSizeBytes);
  wprintf(L"  head: 0x%08lx\n", (unsigned long)q.head);
  wprintf(L"  tail: 0x%08lx\n", (unsigned long)q.tail);
  wprintf(L"  descriptors: %lu\n", (unsigned long)q.descCount);

  uint32_t count = q.descCount;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }

  for (uint32_t i = 0; i < count; ++i) {
    const AEROGPU_DBGCTL_RING_DESC *d = &q.desc[i];
    wprintf(L"    [%lu] fence=0x%I64x cmdGpuVa=0x%I64x cmdBytes=%lu flags=0x%08lx\n", (unsigned long)i,
            (unsigned long long)d->fence, (unsigned long long)d->cmdGpuVa, (unsigned long)d->cmdBytes,
            (unsigned long)d->flags);
  }

  return 0;
}

static int DoSelftest(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t timeoutMs) {
  AEROGPU_DBGCTL_SELFTEST q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.magic = AEROGPU_DBGCTL_ESCAPE_MAGIC;
  q.hdr.abiVersion = AEROGPU_DBGCTL_ESCAPE_ABI_VERSION;
  q.hdr.op = AEROGPU_DBGCTL_OP_SELFTEST;
  q.timeoutMs = timeoutMs;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(selftest) failed", f, st);
    return 2;
  }

  wprintf(L"Selftest: %s\n", q.passed ? L"PASS" : L"FAIL");
  if (!q.passed) {
    wprintf(L"Error code: 0x%08lx\n", (unsigned long)q.errorCode);
  }
  return q.passed ? 0 : 3;
}

int wmain(int argc, wchar_t **argv) {
  const wchar_t *displayNameOpt = NULL;
  uint32_t ringId = 0;
  uint32_t timeoutMs = 2000;
  enum {
    CMD_NONE = 0,
    CMD_QUERY_VERSION,
    CMD_QUERY_FENCE,
    CMD_DUMP_RING,
    CMD_SELFTEST
  } cmd = CMD_NONE;

  for (int i = 1; i < argc; ++i) {
    const wchar_t *a = argv[i];
    if (wcscmp(a, L"--help") == 0 || wcscmp(a, L"-h") == 0 || wcscmp(a, L"/?") == 0) {
      PrintUsage();
      return 0;
    }

    if (wcscmp(a, L"--display") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--display requires an argument\n");
        PrintUsage();
        return 1;
      }
      displayNameOpt = argv[++i];
      continue;
    }

    if (wcscmp(a, L"--ring-id") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--ring-id requires an argument\n");
        PrintUsage();
        return 1;
      }
      ringId = (uint32_t)wcstoul(argv[++i], NULL, 0);
      continue;
    }

    if (wcscmp(a, L"--timeout-ms") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--timeout-ms requires an argument\n");
        PrintUsage();
        return 1;
      }
      timeoutMs = (uint32_t)wcstoul(argv[++i], NULL, 0);
      continue;
    }

    if (wcscmp(a, L"--query-version") == 0) {
      cmd = (cmd == CMD_NONE) ? CMD_QUERY_VERSION : CMD_NONE;
      continue;
    }
    if (wcscmp(a, L"--query-fence") == 0) {
      cmd = (cmd == CMD_NONE) ? CMD_QUERY_FENCE : CMD_NONE;
      continue;
    }
    if (wcscmp(a, L"--dump-ring") == 0) {
      cmd = (cmd == CMD_NONE) ? CMD_DUMP_RING : CMD_NONE;
      continue;
    }
    if (wcscmp(a, L"--selftest") == 0) {
      cmd = (cmd == CMD_NONE) ? CMD_SELFTEST : CMD_NONE;
      continue;
    }

    fwprintf(stderr, L"Unknown argument: %s\n", a);
    PrintUsage();
    return 1;
  }

  if (cmd == CMD_NONE) {
    PrintUsage();
    return 1;
  }

  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f)) {
    return 1;
  }

  wchar_t displayName[CCHDEVICENAME];
  if (displayNameOpt) {
    wcsncpy(displayName, displayNameOpt, CCHDEVICENAME - 1);
    displayName[CCHDEVICENAME - 1] = 0;
  } else {
    GetPrimaryDisplayName(displayName);
  }

  HDC hdc = CreateDCW(L"DISPLAY", displayName, NULL, NULL);
  if (!hdc) {
    fwprintf(stderr, L"CreateDCW failed for %s (GetLastError=%lu)\n", displayName, (unsigned long)GetLastError());
    return 1;
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  DeleteDC(hdc);
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTOpenAdapterFromHdc failed", &f, st);
    return 1;
  }

  int rc = 0;
  switch (cmd) {
  case CMD_QUERY_VERSION:
    rc = DoQueryVersion(&f, open.hAdapter);
    break;
  case CMD_QUERY_FENCE:
    rc = DoQueryFence(&f, open.hAdapter);
    break;
  case CMD_DUMP_RING:
    rc = DoDumpRing(&f, open.hAdapter, ringId);
    break;
  case CMD_SELFTEST:
    rc = DoSelftest(&f, open.hAdapter, timeoutMs);
    break;
  default:
    rc = 1;
    break;
  }

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  st = f.CloseAdapter(&close);
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTCloseAdapter failed", &f, st);
    if (rc == 0) {
      rc = 4;
    }
  }
  return rc;
}
