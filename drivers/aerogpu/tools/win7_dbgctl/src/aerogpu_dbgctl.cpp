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
           L"  aerogpu_dbgctl [--display \\\\.\\DISPLAY1] [--ring-id N] [--timeout-ms N]\n"
           L"               [--vblank-samples N] [--vblank-interval-ms N] <command>\n"
           L"\n"
           L"Commands:\n"
           L"  --list-displays\n"
           L"  --query-version\n"
           L"  --query-fence\n"
           L"  --dump-ring\n"
           L"  --dump-vblank\n"
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

static int ListDisplays() {
  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  wprintf(L"Display devices:\n");
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    const bool primary = (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
    const bool active = (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0;
    wprintf(L"  [%lu] %s%s%s\n",
            (unsigned long)i,
            dd.DeviceName,
            primary ? L" (primary)" : L"",
            active ? L" (active)" : L"");
    wprintf(L"       %s\n", dd.DeviceString);

    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  return 0;
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

static const wchar_t *SelftestErrorToString(uint32_t code) {
  switch (code) {
  case AEROGPU_DBGCTL_SELFTEST_OK:
    return L"OK";
  case AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE:
    return L"INVALID_STATE";
  case AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY:
    return L"RING_NOT_READY";
  case AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY:
    return L"GPU_BUSY";
  case AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES:
    return L"NO_RESOURCES";
  case AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT:
    return L"TIMEOUT";
  default:
    return L"UNKNOWN";
  }
}

static int DoQueryVersion(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_query_device_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-version) failed", f, st);
    return 2;
  }

  uint32_t major = (uint32_t)(q.mmio_version >> 16);
  uint32_t minor = (uint32_t)(q.mmio_version & 0xFFFFu);

  wprintf(L"AeroGPU escape ABI: %lu\n", (unsigned long)q.hdr.version);
  wprintf(L"AeroGPU MMIO version: 0x%08lx (%lu.%lu)\n",
          (unsigned long)q.mmio_version,
          (unsigned long)major,
          (unsigned long)minor);
  return 0;
}

static int DoQueryFence(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_query_fence_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, st);
    return 2;
  }

  wprintf(L"Last submitted fence: 0x%I64x (%I64u)\n", (unsigned long long)q.last_submitted_fence,
          (unsigned long long)q.last_submitted_fence);
  wprintf(L"Last completed fence: 0x%I64x (%I64u)\n", (unsigned long long)q.last_completed_fence,
          (unsigned long long)q.last_completed_fence);
  return 0;
}

static int DoDumpRing(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId) {
  aerogpu_escape_dump_ring_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.ring_id = ringId;
  q.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st);
    return 2;
  }

  wprintf(L"Ring %lu\n", (unsigned long)q.ring_id);
  wprintf(L"  size: %lu bytes\n", (unsigned long)q.ring_size_bytes);
  wprintf(L"  head: 0x%08lx\n", (unsigned long)q.head);
  wprintf(L"  tail: 0x%08lx\n", (unsigned long)q.tail);
  wprintf(L"  descriptors: %lu\n", (unsigned long)q.desc_count);

  uint32_t count = q.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }

  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_ring_desc *d = &q.desc[i];
    wprintf(L"    [%lu] fence=0x%I64x descGpa=0x%I64x descBytes=%lu flags=0x%08lx\n", (unsigned long)i,
            (unsigned long long)d->fence, (unsigned long long)d->desc_gpa, (unsigned long)d->desc_size_bytes,
            (unsigned long)d->flags);
  }

  return 0;
}

static bool QueryVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId,
                        aerogpu_escape_dump_vblank_inout *out) {
  ZeroMemory(out, sizeof(*out));
  out->hdr.version = AEROGPU_ESCAPE_VERSION;
  out->hdr.op = AEROGPU_ESCAPE_OP_DUMP_VBLANK;
  out->hdr.size = sizeof(*out);
  out->hdr.reserved0 = 0;
  out->vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-vblank) failed", f, st);
    return false;
  }
  return true;
}

static void PrintVblankSnapshot(const aerogpu_escape_dump_vblank_inout *q) {
  wprintf(L"Vblank (VidPn source %lu)\n", (unsigned long)q->vidpn_source_id);
  wprintf(L"  IRQ_STATUS: 0x%08lx\n", (unsigned long)q->irq_status);
  wprintf(L"  IRQ_ENABLE: 0x%08lx\n", (unsigned long)q->irq_enable);

  if ((q->flags & AEROGPU_DBGCTL_VBLANK_SUPPORTED) == 0) {
    wprintf(L"  vblank: not supported (AEROGPU_FEATURE_VBLANK not set)\n");
    return;
  }

  wprintf(L"  vblank_seq: 0x%I64x (%I64u)\n", (unsigned long long)q->vblank_seq, (unsigned long long)q->vblank_seq);
  wprintf(L"  last_vblank_time_ns: 0x%I64x (%I64u ns)\n",
          (unsigned long long)q->last_vblank_time_ns,
          (unsigned long long)q->last_vblank_time_ns);

  if (q->vblank_period_ns != 0) {
    const double hz = 1000000000.0 / (double)q->vblank_period_ns;
    wprintf(L"  vblank_period_ns: %lu (~%.3f Hz)\n", (unsigned long)q->vblank_period_ns, hz);
  } else {
    wprintf(L"  vblank_period_ns: 0\n");
  }
}

static int DoDumpVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                        uint32_t intervalMs) {
  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  aerogpu_escape_dump_vblank_inout q;
  aerogpu_escape_dump_vblank_inout prev;
  bool havePrev = false;

  for (uint32_t i = 0; i < samples; ++i) {
    if (!QueryVblank(f, hAdapter, vidpnSourceId, &q)) {
      return 2;
    }

    if (samples > 1) {
      wprintf(L"Sample %lu/%lu:\n", (unsigned long)(i + 1), (unsigned long)samples);
    }
    PrintVblankSnapshot(&q);

    if (havePrev && (q.flags & AEROGPU_DBGCTL_VBLANK_SUPPORTED) != 0 &&
        (prev.flags & AEROGPU_DBGCTL_VBLANK_SUPPORTED) != 0) {
      const uint64_t dseq = q.vblank_seq - prev.vblank_seq;
      const uint64_t dt = q.last_vblank_time_ns - prev.last_vblank_time_ns;
      wprintf(L"  delta: seq=%I64u time=%I64u ns\n", (unsigned long long)dseq, (unsigned long long)dt);
      if (dseq != 0 && dt != 0) {
        const double hz = (double)dseq * 1000000000.0 / (double)dt;
        wprintf(L"  observed: ~%.3f Hz\n", hz);
      }
    }

    prev = q;
    havePrev = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }

  return 0;
}

static int DoSelftest(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t timeoutMs) {
  aerogpu_escape_selftest_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.timeout_ms = timeoutMs;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(selftest) failed", f, st);
    return 2;
  }

  wprintf(L"Selftest: %s\n", q.passed ? L"PASS" : L"FAIL");
  if (!q.passed) {
    wprintf(L"Error code: %lu (%s)\n", (unsigned long)q.error_code, SelftestErrorToString(q.error_code));
  }
  return q.passed ? 0 : 3;
}

int wmain(int argc, wchar_t **argv) {
  const wchar_t *displayNameOpt = NULL;
  uint32_t ringId = 0;
  uint32_t timeoutMs = 2000;
  uint32_t vblankSamples = 1;
  uint32_t vblankIntervalMs = 250;
  enum {
    CMD_NONE = 0,
    CMD_LIST_DISPLAYS,
    CMD_QUERY_VERSION,
    CMD_QUERY_FENCE,
    CMD_DUMP_RING,
    CMD_DUMP_VBLANK,
    CMD_SELFTEST
  } cmd = CMD_NONE;

  const auto SetCommand = [&](int newCmd) -> bool {
    if (cmd != CMD_NONE) {
      fwprintf(stderr, L"Multiple commands specified.\n");
      PrintUsage();
      return false;
    }
    cmd = (decltype(cmd))newCmd;
    return true;
  };

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

    if (wcscmp(a, L"--vblank-samples") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--vblank-samples requires an argument\n");
        PrintUsage();
        return 1;
      }
      vblankSamples = (uint32_t)wcstoul(argv[++i], NULL, 0);
      continue;
    }

    if (wcscmp(a, L"--vblank-interval-ms") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--vblank-interval-ms requires an argument\n");
        PrintUsage();
        return 1;
      }
      vblankIntervalMs = (uint32_t)wcstoul(argv[++i], NULL, 0);
      continue;
    }

    if (wcscmp(a, L"--query-version") == 0) {
      if (!SetCommand(CMD_QUERY_VERSION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-fence") == 0) {
      if (!SetCommand(CMD_QUERY_FENCE)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-ring") == 0) {
      if (!SetCommand(CMD_DUMP_RING)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-vblank") == 0) {
      if (!SetCommand(CMD_DUMP_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--selftest") == 0) {
      if (!SetCommand(CMD_SELFTEST)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--list-displays") == 0) {
      if (!SetCommand(CMD_LIST_DISPLAYS)) {
        return 1;
      }
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

  if (cmd == CMD_LIST_DISPLAYS) {
    return ListDisplays();
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
  case CMD_DUMP_VBLANK:
    rc = DoDumpVblank(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs);
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
