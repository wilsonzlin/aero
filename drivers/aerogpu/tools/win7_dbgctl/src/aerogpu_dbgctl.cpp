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
#include <string.h>
#include <wchar.h>

#include "aerogpu_pci.h"
#include "aerogpu_dbgctl_escape.h"
#include "aerogpu_umd_private.h"

typedef LONG NTSTATUS;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#endif

#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif

#ifndef STATUS_TIMEOUT
#define STATUS_TIMEOUT ((NTSTATUS)0xC0000102L)
#endif

#ifndef STATUS_INSUFFICIENT_RESOURCES
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#endif

typedef UINT D3DKMT_HANDLE;

static const uint32_t kAerogpuIrqFence = (1u << 0);
static const uint32_t kAerogpuIrqScanoutVblank = (1u << 1);
static const uint32_t kAerogpuIrqError = (1u << 31);

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
  UINT VidPnSourceId;
  BOOL InVerticalBlank;
  UINT ScanLine;
} D3DKMT_GETSCANLINE;

typedef struct D3DKMT_QUERYADAPTERINFO {
  D3DKMT_HANDLE hAdapter;
  UINT Type; // KMTQUERYADAPTERINFOTYPE
  VOID *pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_QUERYADAPTERINFO;

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
typedef NTSTATUS(WINAPI *PFND3DKMTWaitForVerticalBlankEvent)(D3DKMT_WAITFORVERTICALBLANKEVENT *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTGetScanLine)(D3DKMT_GETSCANLINE *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTQueryAdapterInfo)(D3DKMT_QUERYADAPTERINFO *pData);
typedef ULONG(WINAPI *PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFND3DKMTGetScanLine GetScanLine;
  PFND3DKMTQueryAdapterInfo QueryAdapterInfo;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
} D3DKMT_FUNCS;

static uint32_t g_escape_timeout_ms = 0;
static volatile LONG g_skip_close_adapter = 0;

static void PrintUsage() {
  fwprintf(stderr,
           L"Usage:\n"
           L"  aerogpu_dbgctl [--display \\\\.\\DISPLAY1] [--ring-id N] [--timeout-ms N]\n"
           L"               [--vblank-samples N] [--vblank-interval-ms N] <command>\n"
           L"\n"
           L"Commands:\n"
           L"  --list-displays\n"
           L"  --status  (alias: --query-version)\n"
           L"  --query-version  (alias: --query-device)\n"
           L"  --query-umd-private\n"
           L"  --query-fence\n"
           L"  --query-scanout\n"
           L"  --dump-ring\n"
           L"  --dump-createalloc  (DxgkDdiCreateAllocation trace)\n"
              L"  --dump-vblank  (alias: --query-vblank)\n"
              L"  --wait-vblank  (D3DKMTWaitForVerticalBlankEvent)\n"
              L"  --query-scanline  (D3DKMTGetScanLine)\n"
              L"  --map-shared-handle HANDLE\n"
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
  out->WaitForVerticalBlankEvent =
      (PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(out->gdi32, "D3DKMTWaitForVerticalBlankEvent");
  out->GetScanLine = (PFND3DKMTGetScanLine)GetProcAddress(out->gdi32, "D3DKMTGetScanLine");
  out->QueryAdapterInfo = (PFND3DKMTQueryAdapterInfo)GetProcAddress(out->gdi32, "D3DKMTQueryAdapterInfo");

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

typedef struct EscapeThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  void *buf;
  UINT bufSize;
  NTSTATUS status;
  HANDLE done_event;
} EscapeThreadCtx;

static DWORD WINAPI EscapeThreadProc(LPVOID param) {
  EscapeThreadCtx *ctx = (EscapeThreadCtx *)param;
  if (!ctx || !ctx->f || !ctx->f->Escape || !ctx->buf || ctx->bufSize == 0) {
    if (ctx) {
      ctx->status = STATUS_INVALID_PARAMETER;
    }
    return 0;
  }

  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = ctx->hAdapter;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = ctx->buf;
  e.PrivateDriverDataSize = ctx->bufSize;
  ctx->status = ctx->f->Escape(&e);

  if (ctx->done_event) {
    SetEvent(ctx->done_event);
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
  if (g_escape_timeout_ms == 0) {
    return f->Escape(&e);
  }

  // Like the vblank wait helper, run escapes on a worker thread so a buggy kernel driver cannot
  // hang the dbgctl process forever. If the call times out, leak the context (the thread may be
  // blocked inside the kernel thunk) and set a global so we avoid calling D3DKMTCloseAdapter.
  EscapeThreadCtx *ctx = (EscapeThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*ctx));
  if (!ctx) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  void *bufCopy = HeapAlloc(GetProcessHeap(), 0, bufSize);
  if (!bufCopy) {
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  memcpy(bufCopy, buf, bufSize);

  ctx->f = f;
  ctx->hAdapter = hAdapter;
  ctx->buf = bufCopy;
  ctx->bufSize = bufSize;
  ctx->status = 0;
  ctx->done_event = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (!ctx->done_event) {
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  HANDLE thread = CreateThread(NULL, 0, EscapeThreadProc, ctx, 0, NULL);
  if (!thread) {
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  DWORD w = WaitForSingleObject(ctx->done_event, g_escape_timeout_ms);
  if (w == WAIT_OBJECT_0) {
    // Thread completed; safe to copy results back and clean up.
    const NTSTATUS st = ctx->status;
    if (NT_SUCCESS(st)) {
      memcpy(buf, ctx->buf, bufSize);
    }
    CloseHandle(thread);
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, ctx->buf);
    HeapFree(GetProcessHeap(), 0, ctx);
    return st;
  }

  // Timeout or failure; avoid deadlock-prone cleanup.
  CloseHandle(thread);
  InterlockedExchange(&g_skip_close_adapter, 1);
  return (w == WAIT_TIMEOUT) ? STATUS_TIMEOUT : STATUS_INVALID_PARAMETER;
}

typedef struct QueryAdapterInfoThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  UINT type;
  void *buf;
  UINT bufSize;
  NTSTATUS status;
  HANDLE done_event;
} QueryAdapterInfoThreadCtx;

static DWORD WINAPI QueryAdapterInfoThreadProc(LPVOID param) {
  QueryAdapterInfoThreadCtx *ctx = (QueryAdapterInfoThreadCtx *)param;
  if (!ctx || !ctx->f || !ctx->f->QueryAdapterInfo || !ctx->buf || ctx->bufSize == 0) {
    if (ctx) {
      ctx->status = STATUS_INVALID_PARAMETER;
      if (ctx->done_event) {
        SetEvent(ctx->done_event);
      }
    }
    return 0;
  }

  D3DKMT_QUERYADAPTERINFO q;
  ZeroMemory(&q, sizeof(q));
  q.hAdapter = ctx->hAdapter;
  q.Type = ctx->type;
  q.pPrivateDriverData = ctx->buf;
  q.PrivateDriverDataSize = ctx->bufSize;

  ctx->status = ctx->f->QueryAdapterInfo(&q);

  if (ctx->done_event) {
    SetEvent(ctx->done_event);
  }
  return 0;
}

static NTSTATUS QueryAdapterInfoWithTimeout(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, UINT type, void *buf,
                                            UINT bufSize) {
  if (!f || !f->QueryAdapterInfo || !hAdapter || !buf || bufSize == 0) {
    return STATUS_INVALID_PARAMETER;
  }

  D3DKMT_QUERYADAPTERINFO q;
  ZeroMemory(&q, sizeof(q));
  q.hAdapter = hAdapter;
  q.Type = type;
  q.pPrivateDriverData = buf;
  q.PrivateDriverDataSize = bufSize;

  if (g_escape_timeout_ms == 0) {
    return f->QueryAdapterInfo(&q);
  }

  // Run QueryAdapterInfo on a worker thread so a buggy kernel driver cannot hang dbgctl forever. If the call times out,
  // leak the context (the thread may be blocked inside the kernel thunk) and set a global so we avoid calling
  // D3DKMTCloseAdapter.
  QueryAdapterInfoThreadCtx *ctx =
      (QueryAdapterInfoThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*ctx));
  if (!ctx) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  void *bufCopy = HeapAlloc(GetProcessHeap(), 0, bufSize);
  if (!bufCopy) {
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  memcpy(bufCopy, buf, bufSize);

  ctx->f = f;
  ctx->hAdapter = hAdapter;
  ctx->type = type;
  ctx->buf = bufCopy;
  ctx->bufSize = bufSize;
  ctx->status = 0;
  ctx->done_event = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (!ctx->done_event) {
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  HANDLE thread = CreateThread(NULL, 0, QueryAdapterInfoThreadProc, ctx, 0, NULL);
  if (!thread) {
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  DWORD w = WaitForSingleObject(ctx->done_event, g_escape_timeout_ms);
  if (w == WAIT_OBJECT_0) {
    const NTSTATUS st = ctx->status;
    if (NT_SUCCESS(st)) {
      memcpy(buf, ctx->buf, bufSize);
    }
    CloseHandle(thread);
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, ctx->buf);
    HeapFree(GetProcessHeap(), 0, ctx);
    return st;
  }

  CloseHandle(thread);
  InterlockedExchange(&g_skip_close_adapter, 1);
  return (w == WAIT_TIMEOUT) ? STATUS_TIMEOUT : STATUS_INVALID_PARAMETER;
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
  static const uint32_t kLegacyMmioMagic = 0x41524750u; // "ARGP" little-endian
  const auto DumpFenceSnapshot = [&]() {
    aerogpu_escape_query_fence_out qf;
    ZeroMemory(&qf, sizeof(qf));
    qf.hdr.version = AEROGPU_ESCAPE_VERSION;
    qf.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    qf.hdr.size = sizeof(qf);
    qf.hdr.reserved0 = 0;

    NTSTATUS stFence = SendAerogpuEscape(f, hAdapter, &qf, sizeof(qf));
    if (!NT_SUCCESS(stFence)) {
      if (stFence == STATUS_NOT_SUPPORTED) {
        wprintf(L"Fences: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, stFence);
      }
      return;
    }

    wprintf(L"Last submitted fence: 0x%I64x (%I64u)\n",
            (unsigned long long)qf.last_submitted_fence,
            (unsigned long long)qf.last_submitted_fence);
    wprintf(L"Last completed fence: 0x%I64x (%I64u)\n",
            (unsigned long long)qf.last_completed_fence,
            (unsigned long long)qf.last_completed_fence);
  };

  const auto DumpRingSummary = [&]() {
    aerogpu_escape_dump_ring_v2_inout q2;
    ZeroMemory(&q2, sizeof(q2));
    q2.hdr.version = AEROGPU_ESCAPE_VERSION;
    q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
    q2.hdr.size = sizeof(q2);
    q2.hdr.reserved0 = 0;
    q2.ring_id = 0;
    q2.desc_capacity = 1;

    NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
    if (NT_SUCCESS(st)) {
      wprintf(L"Ring0:\n");
      wprintf(L"  format=%lu ring_size_bytes=%lu head=%lu tail=%lu desc_count=%lu\n",
              (unsigned long)q2.ring_format,
              (unsigned long)q2.ring_size_bytes,
              (unsigned long)q2.head,
              (unsigned long)q2.tail,
              (unsigned long)q2.desc_count);
      if (q2.desc_count > 0) {
        const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
        wprintf(L"  last: fence=0x%I64x cmd_gpa=0x%I64x cmd_size=%lu flags=0x%08lx alloc_table_gpa=0x%I64x alloc_table_size=%lu\n",
                (unsigned long long)d.fence,
                (unsigned long long)d.cmd_gpa,
                (unsigned long)d.cmd_size_bytes,
                (unsigned long)d.flags,
                (unsigned long long)d.alloc_table_gpa,
                (unsigned long)d.alloc_table_size_bytes);
      }
      return;
    }

    if (st == STATUS_NOT_SUPPORTED) {
      // Fall back to the legacy dump-ring packet for older drivers.
      aerogpu_escape_dump_ring_inout q1;
      ZeroMemory(&q1, sizeof(q1));
      q1.hdr.version = AEROGPU_ESCAPE_VERSION;
      q1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
      q1.hdr.size = sizeof(q1);
      q1.hdr.reserved0 = 0;
      q1.ring_id = 0;
      q1.desc_capacity = 1;

      NTSTATUS st1 = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
      if (!NT_SUCCESS(st1)) {
        if (st1 == STATUS_NOT_SUPPORTED) {
          wprintf(L"Ring0: (not supported)\n");
        } else {
          PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st1);
        }
        return;
      }

      wprintf(L"Ring0:\n");
      wprintf(L"  ring_size_bytes=%lu head=%lu tail=%lu desc_count=%lu\n",
              (unsigned long)q1.ring_size_bytes,
              (unsigned long)q1.head,
              (unsigned long)q1.tail,
              (unsigned long)q1.desc_count);
      if (q1.desc_count > 0) {
        const aerogpu_dbgctl_ring_desc &d = q1.desc[q1.desc_count - 1];
        wprintf(L"  last: fence=0x%I64x cmd_gpa=0x%I64x cmd_size=%lu flags=0x%08lx\n",
                (unsigned long long)d.signal_fence,
                (unsigned long long)d.cmd_gpa,
                (unsigned long)d.cmd_size_bytes,
                (unsigned long)d.flags);
      }
      return;
    }

    PrintNtStatus(L"D3DKMTEscape(dump-ring-v2) failed", f, st);
  };

  const auto DumpScanoutSnapshot = [&]() {
    aerogpu_escape_query_scanout_out qs;
    ZeroMemory(&qs, sizeof(qs));
    qs.hdr.version = AEROGPU_ESCAPE_VERSION;
    qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    qs.hdr.size = sizeof(qs);
    qs.hdr.reserved0 = 0;
    qs.vidpn_source_id = 0;

    NTSTATUS stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
    if (!NT_SUCCESS(stScanout)) {
      if (stScanout == STATUS_NOT_SUPPORTED) {
        wprintf(L"Scanout0: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, stScanout);
      }
      return;
    }

    wprintf(L"Scanout0:\n");
    wprintf(L"  cached: enable=%lu width=%lu height=%lu format=%lu pitch=%lu\n",
            (unsigned long)qs.cached_enable,
            (unsigned long)qs.cached_width,
            (unsigned long)qs.cached_height,
            (unsigned long)qs.cached_format,
            (unsigned long)qs.cached_pitch_bytes);
    wprintf(L"  mmio:   enable=%lu width=%lu height=%lu format=%lu pitch=%lu fb_gpa=0x%I64x\n",
            (unsigned long)qs.mmio_enable,
            (unsigned long)qs.mmio_width,
            (unsigned long)qs.mmio_height,
            (unsigned long)qs.mmio_format,
            (unsigned long)qs.mmio_pitch_bytes,
            (unsigned long long)qs.mmio_fb_gpa);
  };

  const auto DumpVblankSnapshot = [&]() {
    aerogpu_escape_query_vblank_out qv;
    ZeroMemory(&qv, sizeof(qv));
    qv.hdr.version = AEROGPU_ESCAPE_VERSION;
    qv.hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
    qv.hdr.size = sizeof(qv);
    qv.hdr.reserved0 = 0;
    qv.vidpn_source_id = 0;

    NTSTATUS stVblank = SendAerogpuEscape(f, hAdapter, &qv, sizeof(qv));
    if (!NT_SUCCESS(stVblank)) {
      if (stVblank == STATUS_NOT_SUPPORTED) {
        wprintf(L"Scanout0 vblank: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-vblank) failed", f, stVblank);
      }
      return;
    }

    bool supported = true;
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      supported = (qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }

    wprintf(L"Scanout0 vblank:\n");
    wprintf(L"  irq_enable: 0x%08lx\n", (unsigned long)qv.irq_enable);
    wprintf(L"  irq_status: 0x%08lx\n", (unsigned long)qv.irq_status);
    wprintf(L"  irq_active: 0x%08lx\n", (unsigned long)(qv.irq_enable & qv.irq_status));
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
        wprintf(L"  vblank_interrupt_type: %lu\n", (unsigned long)qv.vblank_interrupt_type);
      } else {
        wprintf(L"  vblank_interrupt_type: (not enabled or not reported)\n");
      }
    }
    if (!supported) {
      wprintf(L"  (not supported)\n");
      return;
    }

    if (qv.vblank_period_ns != 0) {
      const double hz = 1000000000.0 / (double)qv.vblank_period_ns;
      wprintf(L"  vblank_period_ns: %lu (~%.3f Hz)\n", (unsigned long)qv.vblank_period_ns, hz);
    } else {
      wprintf(L"  vblank_period_ns: 0\n");
    }
    wprintf(L"  vblank_seq: 0x%I64x (%I64u)\n", (unsigned long long)qv.vblank_seq, (unsigned long long)qv.vblank_seq);
    wprintf(L"  last_vblank_time_ns: 0x%I64x (%I64u ns)\n",
            (unsigned long long)qv.last_vblank_time_ns,
            (unsigned long long)qv.last_vblank_time_ns);
  };

  const auto DumpCreateAllocationSummary = [&]() {
    aerogpu_escape_dump_createallocation_inout qa;
    ZeroMemory(&qa, sizeof(qa));
    qa.hdr.version = AEROGPU_ESCAPE_VERSION;
    qa.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
    qa.hdr.size = sizeof(qa);
    qa.hdr.reserved0 = 0;
    qa.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;

    NTSTATUS stAlloc = SendAerogpuEscape(f, hAdapter, &qa, sizeof(qa));
    if (!NT_SUCCESS(stAlloc)) {
      if (stAlloc == STATUS_NOT_SUPPORTED) {
        wprintf(L"CreateAllocation trace: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(dump-createalloc) failed", f, stAlloc);
      }
      return;
    }

    wprintf(L"CreateAllocation trace: write_index=%lu entry_count=%lu\n",
            (unsigned long)qa.write_index,
            (unsigned long)qa.entry_count);
  };

  aerogpu_escape_query_device_v2_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    // Fall back to legacy QUERY_DEVICE for older drivers.
    aerogpu_escape_query_device_out q1;
    ZeroMemory(&q1, sizeof(q1));
    q1.hdr.version = AEROGPU_ESCAPE_VERSION;
    q1.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
    q1.hdr.size = sizeof(q1);
    q1.hdr.reserved0 = 0;

    st = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(query-version) failed", f, st);
      return 2;
    }

    const uint32_t major = (uint32_t)(q1.mmio_version >> 16);
    const uint32_t minor = (uint32_t)(q1.mmio_version & 0xFFFFu);
    wprintf(L"AeroGPU escape ABI: %lu\n", (unsigned long)q1.hdr.version);
    wprintf(L"AeroGPU ABI version: 0x%08lx (%lu.%lu)\n",
            (unsigned long)q1.mmio_version,
            (unsigned long)major,
            (unsigned long)minor);

    DumpFenceSnapshot();
    DumpRingSummary();
    DumpScanoutSnapshot();
    DumpVblankSnapshot();
    DumpCreateAllocationSummary();
    return 0;
  }

  const wchar_t *abiStr = L"unknown";
  if (q.detected_mmio_magic == kLegacyMmioMagic) {
    abiStr = L"legacy (ARGP)";
  } else if (q.detected_mmio_magic == AEROGPU_MMIO_MAGIC) {
    abiStr = L"new (AGPU)";
  }

  const uint32_t major = (uint32_t)(q.abi_version_u32 >> 16);
  const uint32_t minor = (uint32_t)(q.abi_version_u32 & 0xFFFFu);

  wprintf(L"AeroGPU escape ABI: %lu\n", (unsigned long)q.hdr.version);
  wprintf(L"AeroGPU device ABI: %s\n", abiStr);
  wprintf(L"AeroGPU MMIO magic: 0x%08lx\n", (unsigned long)q.detected_mmio_magic);
  wprintf(L"AeroGPU ABI version: 0x%08lx (%lu.%lu)\n",
          (unsigned long)q.abi_version_u32,
          (unsigned long)major,
          (unsigned long)minor);

  if (q.features_lo != 0 || q.features_hi != 0) {
    wprintf(L"AeroGPU features:\n");
    wprintf(L"  lo=0x%I64x hi=0x%I64x\n", (unsigned long long)q.features_lo, (unsigned long long)q.features_hi);
    if (q.detected_mmio_magic == kLegacyMmioMagic) {
      wprintf(L"  (note: legacy device; feature bits are best-effort)\n");
    }
    wprintf(L"  decoded:");
    bool any = false;
    if (q.features_lo & AEROGPU_FEATURE_FENCE_PAGE) {
      wprintf(L"%sFENCE_PAGE", any ? L", " : L" ");
      any = true;
    }
    if (q.features_lo & AEROGPU_FEATURE_CURSOR) {
      wprintf(L"%sCURSOR", any ? L", " : L" ");
      any = true;
    }
    if (q.features_lo & AEROGPU_FEATURE_SCANOUT) {
      wprintf(L"%sSCANOUT", any ? L", " : L" ");
      any = true;
    }
    if (q.features_lo & AEROGPU_FEATURE_VBLANK) {
      wprintf(L"%sVBLANK", any ? L", " : L" ");
      any = true;
    }
    if (q.features_lo & AEROGPU_FEATURE_TRANSFER) {
      wprintf(L"%sTRANSFER", any ? L", " : L" ");
      any = true;
    }
    if (!any) {
      wprintf(L" (none)");
    }
  wprintf(L"\n");
  }

  DumpFenceSnapshot();
  DumpRingSummary();
  DumpScanoutSnapshot();
  DumpVblankSnapshot();
  DumpCreateAllocationSummary();

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

static int DoQueryScanout(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId) {
  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    // Older KMDs may only support source 0; retry.
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, st);
    return 2;
  }

  wprintf(L"Scanout%lu:\n", (unsigned long)q.vidpn_source_id);
  wprintf(L"  cached: enable=%lu width=%lu height=%lu format=%lu pitch=%lu\n",
          (unsigned long)q.cached_enable,
          (unsigned long)q.cached_width,
          (unsigned long)q.cached_height,
          (unsigned long)q.cached_format,
          (unsigned long)q.cached_pitch_bytes);
  wprintf(L"  mmio:   enable=%lu width=%lu height=%lu format=%lu pitch=%lu fb_gpa=0x%I64x\n",
          (unsigned long)q.mmio_enable,
          (unsigned long)q.mmio_width,
          (unsigned long)q.mmio_height,
          (unsigned long)q.mmio_format,
          (unsigned long)q.mmio_pitch_bytes,
          (unsigned long long)q.mmio_fb_gpa);
  return 0;
}

static int DoDumpCreateAllocation(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_dump_createallocation_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.write_index = 0;
  q.entry_count = 0;
  q.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
  q.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"CreateAllocation trace: (not supported)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(dump-createalloc) failed", f, st);
    return 2;
  }

  wprintf(L"CreateAllocation trace:\n");
  wprintf(L"  write_index=%lu entry_count=%lu\n", (unsigned long)q.write_index, (unsigned long)q.entry_count);
  for (uint32_t i = 0; i < q.entry_count && i < q.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    wprintf(L"  [%lu] seq=%lu call=%lu create_flags=0x%08lx alloc[%lu/%lu] alloc_id=%lu share_token=0x%I64x size=%I64u priv_flags=0x%08lx pitch=%lu flags=0x%08lx->0x%08lx\n",
            (unsigned long)i,
            (unsigned long)e.seq,
            (unsigned long)e.call_seq,
            (unsigned long)e.create_flags,
            (unsigned long)e.alloc_index,
            (unsigned long)e.num_allocations,
            (unsigned long)e.alloc_id,
            (unsigned long long)e.share_token,
            (unsigned long long)e.size_bytes,
            (unsigned long)e.priv_flags,
            (unsigned long)e.pitch_bytes,
            (unsigned long)e.flags_in,
            (unsigned long)e.flags_out);
  }
  return 0;
}

static int DoMapSharedHandle(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint64_t sharedHandle) {
  aerogpu_escape_map_shared_handle_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.shared_handle = sharedHandle;
  q.debug_token = 0;
  q.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(map-shared-handle) failed", f, st);
    return 2;
  }

  wprintf(L"debug_token: 0x%08lx (%lu)\n", (unsigned long)q.debug_token, (unsigned long)q.debug_token);
  return 0;
}

static int DoQueryUmdPrivate(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  if (!f->QueryAdapterInfo) {
    fwprintf(stderr, L"D3DKMTQueryAdapterInfo not available (missing gdi32 export)\n");
    return 1;
  }

  aerogpu_umd_private_v1 blob;
  ZeroMemory(&blob, sizeof(blob));

  // We intentionally avoid depending on WDK headers for the numeric
  // KMTQAITYPE_UMDRIVERPRIVATE constant. Instead, probe a small range of values
  // and look for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  UINT foundType = 0xFFFFFFFFu;
  NTSTATUS lastStatus = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&blob, sizeof(blob));
    NTSTATUS st = QueryAdapterInfoWithTimeout(f, hAdapter, type, &blob, sizeof(blob));
    lastStatus = st;
    if (!NT_SUCCESS(st)) {
      if (st == STATUS_TIMEOUT) {
        break;
      }
      continue;
    }

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    foundType = type;
    break;
  }

  if (foundType == 0xFFFFFFFFu) {
    if (lastStatus == STATUS_TIMEOUT) {
      PrintNtStatus(L"D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) timed out", f, lastStatus);
      fwprintf(stderr, L"(note: timed out probing UMDRIVERPRIVATE; KMD may be wedged)\n");
    } else {
      PrintNtStatus(L"D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) failed", f, lastStatus);
      fwprintf(stderr, L"(note: UMDRIVERPRIVATE type probing range exhausted)\n");
    }
    return 2;
  }

  wchar_t magicStr[5] = {0, 0, 0, 0, 0};
  {
    const uint32_t m = blob.device_mmio_magic;
    magicStr[0] = (wchar_t)((m >> 0) & 0xFF);
    magicStr[1] = (wchar_t)((m >> 8) & 0xFF);
    magicStr[2] = (wchar_t)((m >> 16) & 0xFF);
    magicStr[3] = (wchar_t)((m >> 24) & 0xFF);
  }

  wprintf(L"UMDRIVERPRIVATE (type %lu)\n", (unsigned long)foundType);
  wprintf(L"  size_bytes: %lu\n", (unsigned long)blob.size_bytes);
  wprintf(L"  struct_version: %lu\n", (unsigned long)blob.struct_version);
  wprintf(L"  device_mmio_magic: 0x%08lx (%s)\n", (unsigned long)blob.device_mmio_magic, magicStr);

  const uint32_t abiMajor = (uint32_t)(blob.device_abi_version_u32 >> 16);
  const uint32_t abiMinor = (uint32_t)(blob.device_abi_version_u32 & 0xFFFFu);
  wprintf(L"  device_abi_version_u32: 0x%08lx (%lu.%lu)\n",
          (unsigned long)blob.device_abi_version_u32,
          (unsigned long)abiMajor,
          (unsigned long)abiMinor);

  wprintf(L"  device_features: 0x%I64x\n", (unsigned long long)blob.device_features);
  if (blob.device_features != 0) {
    wprintf(L"  decoded_features:");
    bool any = false;
    if (blob.device_features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) {
      wprintf(L"%sFENCE_PAGE", any ? L", " : L" ");
      any = true;
    }
    if (blob.device_features & AEROGPU_UMDPRIV_FEATURE_CURSOR) {
      wprintf(L"%sCURSOR", any ? L", " : L" ");
      any = true;
    }
    if (blob.device_features & AEROGPU_UMDPRIV_FEATURE_SCANOUT) {
      wprintf(L"%sSCANOUT", any ? L", " : L" ");
      any = true;
    }
    if (blob.device_features & AEROGPU_UMDPRIV_FEATURE_VBLANK) {
      wprintf(L"%sVBLANK", any ? L", " : L" ");
      any = true;
    }
    if (blob.device_features & AEROGPU_UMDPRIV_FEATURE_TRANSFER) {
      wprintf(L"%sTRANSFER", any ? L", " : L" ");
      any = true;
    }
    if (!any) {
      wprintf(L" (none)");
    }
    wprintf(L"\n");
  }
  wprintf(L"  flags: 0x%08lx\n", (unsigned long)blob.flags);
  wprintf(L"    is_legacy: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) != 0));
  wprintf(L"    has_vblank: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0));
  wprintf(L"    has_fence_page: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE) != 0));

  return 0;
}

static int DoDumpRing(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId) {
  // Prefer the extended dump-ring packet (supports both legacy and new rings),
  // but fall back to the legacy format for older drivers.
  aerogpu_escape_dump_ring_v2_inout q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.ring_id = ringId;
  q2.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
  if (NT_SUCCESS(st)) {
    const wchar_t *fmt = L"unknown";
    switch (q2.ring_format) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      fmt = L"legacy";
      break;
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      fmt = L"agpu";
      break;
    default:
      fmt = L"unknown";
      break;
    }

    wprintf(L"Ring %lu (%s)\n", (unsigned long)q2.ring_id, fmt);
    wprintf(L"  size: %lu bytes\n", (unsigned long)q2.ring_size_bytes);
    wprintf(L"  head: 0x%08lx\n", (unsigned long)q2.head);
    wprintf(L"  tail: 0x%08lx\n", (unsigned long)q2.tail);
    if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
      wprintf(L"  descriptors (recent tail window): %lu\n", (unsigned long)q2.desc_count);
    } else {
      wprintf(L"  descriptors: %lu\n", (unsigned long)q2.desc_count);
    }

    uint32_t count = q2.desc_count;
    if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
    uint32_t window_start = 0;
    if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU && count != 0) {
      window_start = q2.tail - count;
    }

    for (uint32_t i = 0; i < count; ++i) {
      const aerogpu_dbgctl_ring_desc_v2 *d = &q2.desc[i];
      if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
        wprintf(L"    [%lu] ringIndex=%lu signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx allocTableGpa=0x%I64x allocTableBytes=%lu\n",
                (unsigned long)i, (unsigned long)(window_start + i), (unsigned long long)d->fence, (unsigned long long)d->cmd_gpa,
                (unsigned long)d->cmd_size_bytes, (unsigned long)d->flags,
                (unsigned long long)d->alloc_table_gpa, (unsigned long)d->alloc_table_size_bytes);
      } else {
        wprintf(L"    [%lu] signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx\n",
                (unsigned long)i, (unsigned long long)d->fence, (unsigned long long)d->cmd_gpa,
                (unsigned long)d->cmd_size_bytes, (unsigned long)d->flags);
      }
    }

    return 0;
  }

  aerogpu_escape_dump_ring_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.ring_id = ringId;
  q.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
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
    wprintf(L"    [%lu] signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx\n", (unsigned long)i,
            (unsigned long long)d->signal_fence, (unsigned long long)d->cmd_gpa, (unsigned long)d->cmd_size_bytes,
            (unsigned long)d->flags);
  }

  return 0;
}

static bool QueryVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId,
                        aerogpu_escape_query_vblank_out *out, bool *supportedOut) {
  ZeroMemory(out, sizeof(*out));
  out->hdr.version = AEROGPU_ESCAPE_VERSION;
  out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
  out->hdr.size = sizeof(*out);
  out->hdr.reserved0 = 0;
  out->vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    wprintf(L"QueryVblank: VidPnSourceId=%lu not supported; retrying with source 0\n", (unsigned long)vidpnSourceId);
    ZeroMemory(out, sizeof(*out));
    out->hdr.version = AEROGPU_ESCAPE_VERSION;
    out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
    out->hdr.size = sizeof(*out);
    out->hdr.reserved0 = 0;
    out->vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  }
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-vblank) failed", f, st);
    return false;
  }

  if (supportedOut) {
    bool supported = true;
    if ((out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      supported = (out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }
    *supportedOut = supported;
  }
  return true;
}

static void PrintIrqMask(const wchar_t *label, uint32_t mask) {
  wprintf(L"  %s: 0x%08lx", label, (unsigned long)mask);
  if (mask != 0) {
    wprintf(L" [");
    bool first = true;
    const auto Emit = [&](uint32_t bit, const wchar_t *name) {
      if ((mask & bit) == 0) {
        return;
      }
      if (!first) {
        wprintf(L"|");
      }
      wprintf(L"%s", name);
      first = false;
    };
    Emit(kAerogpuIrqFence, L"FENCE");
    Emit(kAerogpuIrqScanoutVblank, L"VBLANK");
    Emit(kAerogpuIrqError, L"ERROR");
    wprintf(L"]");
  }
  wprintf(L"\n");
}

static void PrintVblankSnapshot(const aerogpu_escape_query_vblank_out *q, bool supported) {
  wprintf(L"Vblank (VidPn source %lu)\n", (unsigned long)q->vidpn_source_id);
  PrintIrqMask(L"IRQ_ENABLE", q->irq_enable);
  PrintIrqMask(L"IRQ_STATUS", q->irq_status);
  PrintIrqMask(L"IRQ_ACTIVE", q->irq_enable & q->irq_status);
  if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
    if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
      wprintf(L"  vblank_interrupt_type: %lu\n", (unsigned long)q->vblank_interrupt_type);
    } else {
      wprintf(L"  vblank_interrupt_type: (not enabled or not reported)\n");
    }
  }

  if (!supported) {
    if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      wprintf(L"  vblank: not supported (flags=0x%08lx)\n", (unsigned long)q->flags);
    } else {
      wprintf(L"  vblank: not supported\n");
    }
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

typedef struct WaitThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  UINT vid_pn_source_id;
  HANDLE request_event;
  HANDLE done_event;
  HANDLE thread;
  volatile LONG stop;
  volatile LONG last_status;
} WaitThreadCtx;

static DWORD WINAPI WaitThreadProc(LPVOID param) {
  WaitThreadCtx *ctx = (WaitThreadCtx *)param;
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

static bool StartWaitThread(WaitThreadCtx *out, const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, UINT vidpnSourceId) {
  ZeroMemory(out, sizeof(*out));
  out->f = f;
  out->hAdapter = hAdapter;
  out->vid_pn_source_id = vidpnSourceId;
  out->stop = 0;
  out->last_status = 0;
  out->request_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  out->done_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  if (!out->request_event || !out->done_event) {
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
    CloseHandle(out->request_event);
    out->request_event = NULL;
    CloseHandle(out->done_event);
    out->done_event = NULL;
    return false;
  }
  return true;
}

static void StopWaitThread(WaitThreadCtx *ctx) {
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

static int DoWaitVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                        uint32_t timeoutMs, bool *skipCloseAdapter) {
  if (skipCloseAdapter) {
    *skipCloseAdapter = false;
  }
  if (!f->WaitForVerticalBlankEvent) {
    fwprintf(stderr, L"D3DKMTWaitForVerticalBlankEvent not available (missing gdi32 export)\n");
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }
  if (timeoutMs == 0) {
    timeoutMs = 1;
  }

  LARGE_INTEGER freq;
  if (!QueryPerformanceFrequency(&freq) || freq.QuadPart <= 0) {
    fwprintf(stderr, L"QueryPerformanceFrequency failed\n");
    return 1;
  }

  // Allocate on heap so we can safely leak on timeout (the wait thread may be
  // blocked inside the kernel thunk; tearing it down can deadlock).
  WaitThreadCtx *waiter = (WaitThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(WaitThreadCtx));
  if (!waiter) {
    fwprintf(stderr, L"HeapAlloc failed\n");
    return 1;
  }

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
    fwprintf(stderr, L"Failed to start wait thread\n");
    HeapFree(GetProcessHeap(), 0, waiter);
    return 1;
  }

  DWORD w = 0;
  NTSTATUS st = 0;
  for (;;) {
    // Prime: perform one wait so subsequent deltas represent full vblank periods.
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      fwprintf(stderr, L"vblank wait timed out after %lu ms (sample 1/%lu)\n", (unsigned long)timeoutMs,
               (unsigned long)samples);
      if (skipCloseAdapter) {
        // The wait thread may be blocked inside the kernel thunk. Avoid calling
        // D3DKMTCloseAdapter in this case; just exit the process.
        *skipCloseAdapter = true;
      }
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      fwprintf(stderr, L"WaitForSingleObject failed (rc=%lu)\n", (unsigned long)w);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      wprintf(L"WaitForVBlank: VidPnSourceId=%lu not supported; retrying with source 0\n",
              (unsigned long)effectiveVidpnSourceId);
      StopWaitThread(waiter);
      effectiveVidpnSourceId = 0;
      if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
        fwprintf(stderr, L"Failed to restart wait thread\n");
        HeapFree(GetProcessHeap(), 0, waiter);
        return 1;
      }
      continue;
    }
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTWaitForVerticalBlankEvent failed", f, st);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }
    break;
  }

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);

  double min_ms = 1e9;
  double max_ms = 0.0;
  double sum_ms = 0.0;
  uint32_t deltas = 0;

  for (uint32_t i = 1; i < samples; ++i) {
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      fwprintf(stderr, L"vblank wait timed out after %lu ms (sample %lu/%lu)\n", (unsigned long)timeoutMs,
               (unsigned long)(i + 1), (unsigned long)samples);
      if (skipCloseAdapter) {
        // The wait thread may be blocked inside the kernel thunk. Avoid calling
        // D3DKMTCloseAdapter in this case; just exit the process.
        *skipCloseAdapter = true;
      }
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      fwprintf(stderr, L"WaitForSingleObject failed (rc=%lu)\n", (unsigned long)w);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTWaitForVerticalBlankEvent failed", f, st);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt_ms = (double)(now.QuadPart - last.QuadPart) * 1000.0 / (double)freq.QuadPart;
    last = now;

    if (dt_ms < min_ms) {
      min_ms = dt_ms;
    }
    if (dt_ms > max_ms) {
      max_ms = dt_ms;
    }
    sum_ms += dt_ms;
    deltas += 1;

    wprintf(L"vblank[%lu/%lu]: %.3f ms\n", (unsigned long)(i + 1), (unsigned long)samples, dt_ms);
  }

  StopWaitThread(waiter);
  HeapFree(GetProcessHeap(), 0, waiter);

  if (deltas != 0) {
    const double avg_ms = sum_ms / (double)deltas;
    const double hz = (avg_ms > 0.0) ? (1000.0 / avg_ms) : 0.0;
    wprintf(L"Summary (%lu waits): avg=%.3f ms min=%.3f ms max=%.3f ms (~%.3f Hz)\n", (unsigned long)samples, avg_ms,
            min_ms, max_ms, hz);
  } else {
    wprintf(L"vblank wait OK\n");
  }

  return 0;
}

static int DoQueryScanline(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                           uint32_t intervalMs) {
  if (!f->GetScanLine) {
    fwprintf(stderr, L"D3DKMTGetScanLine not available (missing gdi32 export)\n");
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  uint32_t inVblank = 0;
  uint32_t outVblank = 0;
  uint32_t minLine = 0xFFFFFFFFu;
  uint32_t maxLine = 0;

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  for (uint32_t i = 0; i < samples; ++i) {
    D3DKMT_GETSCANLINE s;
    ZeroMemory(&s, sizeof(s));
    s.hAdapter = hAdapter;
    s.VidPnSourceId = effectiveVidpnSourceId;

    NTSTATUS st = f->GetScanLine(&s);
    if (!NT_SUCCESS(st) && st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      wprintf(L"GetScanLine: VidPnSourceId=%lu not supported; retrying with source 0\n",
              (unsigned long)effectiveVidpnSourceId);
      effectiveVidpnSourceId = 0;
      s.VidPnSourceId = effectiveVidpnSourceId;
      st = f->GetScanLine(&s);
    }
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTGetScanLine failed", f, st);
      return 2;
    }

    wprintf(L"scanline[%lu/%lu]: %lu%s\n", (unsigned long)(i + 1), (unsigned long)samples, (unsigned long)s.ScanLine,
            s.InVerticalBlank ? L" (vblank)" : L"");

    if (s.InVerticalBlank) {
      inVblank += 1;
    } else {
      outVblank += 1;
      if ((uint32_t)s.ScanLine < minLine) {
        minLine = (uint32_t)s.ScanLine;
      }
      if ((uint32_t)s.ScanLine > maxLine) {
        maxLine = (uint32_t)s.ScanLine;
      }
    }

    if (i + 1 < samples && intervalMs != 0) {
      Sleep(intervalMs);
    }
  }

  wprintf(L"Summary: in_vblank=%lu out_vblank=%lu", (unsigned long)inVblank, (unsigned long)outVblank);
  if (outVblank != 0) {
    wprintf(L" out_scanline_range=[%lu, %lu]", (unsigned long)minLine, (unsigned long)maxLine);
  }
  wprintf(L"\n");
  return 0;
}

static int DoDumpVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                        uint32_t intervalMs) {
  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  aerogpu_escape_query_vblank_out q;
  aerogpu_escape_query_vblank_out prev;
  bool supported = false;
  bool prevSupported = false;
  bool havePrev = false;
  uint32_t stallCount = 0;
  uint64_t perVblankUsMin = 0;
  uint64_t perVblankUsMax = 0;
  uint64_t perVblankUsSum = 0;
  uint64_t perVblankUsSamples = 0;

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  bool scanlineFallbackToSource0 = false;
  for (uint32_t i = 0; i < samples; ++i) {
    if (!QueryVblank(f, hAdapter, effectiveVidpnSourceId, &q, &supported)) {
      return 2;
    }
    effectiveVidpnSourceId = q.vidpn_source_id;

    if (samples > 1) {
      wprintf(L"Sample %lu/%lu:\n", (unsigned long)(i + 1), (unsigned long)samples);
    }
    PrintVblankSnapshot(&q, supported);
    if (f->GetScanLine) {
      D3DKMT_GETSCANLINE s;
      ZeroMemory(&s, sizeof(s));
      s.hAdapter = hAdapter;
      s.VidPnSourceId = scanlineFallbackToSource0 ? 0 : effectiveVidpnSourceId;
      NTSTATUS st = f->GetScanLine(&s);
      if (!NT_SUCCESS(st) && st == STATUS_INVALID_PARAMETER && s.VidPnSourceId != 0) {
        wprintf(L"  GetScanLine: VidPnSourceId=%lu not supported; retrying with source 0\n",
                (unsigned long)s.VidPnSourceId);
        scanlineFallbackToSource0 = true;
        s.VidPnSourceId = 0;
        st = f->GetScanLine(&s);
      }
      if (NT_SUCCESS(st)) {
        wprintf(L"  scanline: %lu%s\n", (unsigned long)s.ScanLine, s.InVerticalBlank ? L" (vblank)" : L"");
      } else if (st == STATUS_NOT_SUPPORTED) {
        wprintf(L"  scanline: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTGetScanLine failed", f, st);
      }
    }

    if (!supported) {
      PrintNtStatus(L"Vblank not supported by device/KMD", f, STATUS_NOT_SUPPORTED);
      return 2;
    }

    if (havePrev && supported && prevSupported) {
      if (q.vblank_seq < prev.vblank_seq || q.last_vblank_time_ns < prev.last_vblank_time_ns) {
        wprintf(L"  delta: counters reset (prev seq=0x%I64x time=0x%I64x, now seq=0x%I64x time=0x%I64x)\n",
                (unsigned long long)prev.vblank_seq,
                (unsigned long long)prev.last_vblank_time_ns,
                (unsigned long long)q.vblank_seq,
                (unsigned long long)q.last_vblank_time_ns);
      } else {
        const uint64_t dseq = q.vblank_seq - prev.vblank_seq;
        const uint64_t dt = q.last_vblank_time_ns - prev.last_vblank_time_ns;
        wprintf(L"  delta: seq=%I64u time=%I64u ns\n", (unsigned long long)dseq, (unsigned long long)dt);
        if (dseq != 0 && dt != 0) {
          const double hz = (double)dseq * 1000000000.0 / (double)dt;
          wprintf(L"  observed: ~%.3f Hz\n", hz);

          const uint64_t perVblankUs = (dt / dseq) / 1000ull;
          if (perVblankUsSamples == 0) {
            perVblankUsMin = perVblankUs;
            perVblankUsMax = perVblankUs;
          } else {
            if (perVblankUs < perVblankUsMin) {
              perVblankUsMin = perVblankUs;
            }
            if (perVblankUs > perVblankUsMax) {
              perVblankUsMax = perVblankUs;
            }
          }
          perVblankUsSum += perVblankUs;
          perVblankUsSamples += 1;
        } else if (dseq == 0) {
          stallCount += 1;
        }
      }
    }

    prev = q;
    prevSupported = supported;
    havePrev = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }

  if (samples > 1 && perVblankUsSamples != 0) {
    const uint64_t avg = perVblankUsSum / perVblankUsSamples;
    wprintf(L"Summary (%I64u deltas): per-vblank ~%I64u us (min=%I64u max=%I64u), stalls=%lu\n",
            (unsigned long long)perVblankUsSamples,
            (unsigned long long)avg,
            (unsigned long long)perVblankUsMin,
            (unsigned long long)perVblankUsMax,
            (unsigned long)stallCount);
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
  uint64_t mapSharedHandle = 0;
  enum {
    CMD_NONE = 0,
    CMD_LIST_DISPLAYS,
    CMD_QUERY_VERSION,
    CMD_QUERY_UMD_PRIVATE,
    CMD_QUERY_FENCE,
    CMD_QUERY_SCANOUT,
    CMD_DUMP_RING,
    CMD_DUMP_CREATEALLOCATION,
    CMD_DUMP_VBLANK,
    CMD_WAIT_VBLANK,
    CMD_QUERY_SCANLINE,
    CMD_MAP_SHARED_HANDLE,
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

    if (wcscmp(a, L"--map-shared-handle") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--map-shared-handle requires an argument\n");
        PrintUsage();
        return 1;
      }
      if (!SetCommand(CMD_MAP_SHARED_HANDLE)) {
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      mapSharedHandle = (uint64_t)_wcstoui64(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --map-shared-handle value: %s\n", arg);
        return 1;
      }
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

    if (wcscmp(a, L"--query-version") == 0 || wcscmp(a, L"--query-device") == 0) {
      if (!SetCommand(CMD_QUERY_VERSION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--status") == 0) {
      if (!SetCommand(CMD_QUERY_VERSION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-umd-private") == 0) {
      if (!SetCommand(CMD_QUERY_UMD_PRIVATE)) {
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
    if (wcscmp(a, L"--query-scanout") == 0) {
      if (!SetCommand(CMD_QUERY_SCANOUT)) {
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
    if (wcscmp(a, L"--dump-createalloc") == 0 || wcscmp(a, L"--dump-createallocation") == 0 ||
        wcscmp(a, L"--dump-allocations") == 0) {
      if (!SetCommand(CMD_DUMP_CREATEALLOCATION)) {
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
    if (wcscmp(a, L"--query-vblank") == 0) {
      if (!SetCommand(CMD_DUMP_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--wait-vblank") == 0) {
      if (!SetCommand(CMD_WAIT_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-scanline") == 0) {
      if (!SetCommand(CMD_QUERY_SCANLINE)) {
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

  // Use the user-provided timeout for escapes as well (prevents hangs on buggy KMD escape paths).
  g_escape_timeout_ms = timeoutMs;

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
  bool skipCloseAdapter = false;
  switch (cmd) {
  case CMD_QUERY_VERSION:
    rc = DoQueryVersion(&f, open.hAdapter);
    break;
  case CMD_QUERY_UMD_PRIVATE:
    rc = DoQueryUmdPrivate(&f, open.hAdapter);
    break;
  case CMD_QUERY_FENCE:
    rc = DoQueryFence(&f, open.hAdapter);
    break;
  case CMD_QUERY_SCANOUT:
    rc = DoQueryScanout(&f, open.hAdapter, (uint32_t)open.VidPnSourceId);
    break;
  case CMD_DUMP_RING:
    rc = DoDumpRing(&f, open.hAdapter, ringId);
    break;
  case CMD_DUMP_CREATEALLOCATION:
    rc = DoDumpCreateAllocation(&f, open.hAdapter);
    break;
  case CMD_DUMP_VBLANK:
    rc = DoDumpVblank(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs);
    break;
  case CMD_WAIT_VBLANK:
    rc = DoWaitVblank(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, timeoutMs, &skipCloseAdapter);
    break;
  case CMD_QUERY_SCANLINE:
    rc = DoQueryScanline(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs);
    break;
  case CMD_MAP_SHARED_HANDLE:
    rc = DoMapSharedHandle(&f, open.hAdapter, mapSharedHandle);
    break;
  case CMD_SELFTEST:
    rc = DoSelftest(&f, open.hAdapter, timeoutMs);
    break;
  default:
    rc = 1;
    break;
  }

  if (skipCloseAdapter || InterlockedCompareExchange(&g_skip_close_adapter, 0, 0) != 0) {
    // Avoid deadlock-prone cleanup when the vblank wait thread is potentially
    // stuck inside a kernel thunk (or when an escape call timed out).
    return rc;
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
