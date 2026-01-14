#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static volatile LONG g_emergency_restore_needed = 0;
static volatile LONG g_emergency_restore_attempted = 0;
static DEVMODEW g_emergency_restore_mode;

static bool ApplyDisplayModeAndWaitEx(const DEVMODEW& target,
                                      DWORD timeout_ms,
                                      bool* out_change_timed_out,
                                      std::string* err);

static const char* DispChangeCodeToString(LONG code) {
  switch (code) {
    case DISP_CHANGE_SUCCESSFUL:
      return "DISP_CHANGE_SUCCESSFUL";
    case DISP_CHANGE_RESTART:
      return "DISP_CHANGE_RESTART";
    case DISP_CHANGE_FAILED:
      return "DISP_CHANGE_FAILED";
    case DISP_CHANGE_BADMODE:
      return "DISP_CHANGE_BADMODE";
    case DISP_CHANGE_NOTUPDATED:
      return "DISP_CHANGE_NOTUPDATED";
    case DISP_CHANGE_BADFLAGS:
      return "DISP_CHANGE_BADFLAGS";
    case DISP_CHANGE_BADPARAM:
      return "DISP_CHANGE_BADPARAM";
    case DISP_CHANGE_BADDUALVIEW:
      return "DISP_CHANGE_BADDUALVIEW";
    default:
      return "DISP_CHANGE_<unknown>";
  }
}

static void AttemptEmergencyModeRestore() {
  if (InterlockedCompareExchange(&g_emergency_restore_needed, 0, 0) == 0) {
    return;
  }
  if (InterlockedCompareExchange(&g_emergency_restore_attempted, 1, 0) != 0) {
    return;
  }

  bool timed_out = false;
  std::string err;
  (void)ApplyDisplayModeAndWaitEx(g_emergency_restore_mode, 2000, &timed_out, &err);
  if (timed_out) {
    // Avoid potentially deadlocking teardown paths if the restore attempt itself timed out.
    InterlockedExchange(&aerogpu_test::kmt::g_skip_close_adapter, 1);
  }
}

static BOOL WINAPI ConsoleCtrlHandler(DWORD ctrl_type) {
  switch (ctrl_type) {
    case CTRL_C_EVENT:
    case CTRL_BREAK_EVENT:
    case CTRL_CLOSE_EVENT:
    case CTRL_LOGOFF_EVENT:
    case CTRL_SHUTDOWN_EVENT:
      AttemptEmergencyModeRestore();
      break;
    default:
      break;
  }
  // Return FALSE so default handling (process termination) still occurs.
  return FALSE;
}

static LONG WINAPI UnhandledExceptionFilterProc(EXCEPTION_POINTERS* /*info*/) {
  AttemptEmergencyModeRestore();
  return EXCEPTION_CONTINUE_SEARCH;
}

struct ChangeDisplaySettingsCtx {
  DEVMODEW dm;
  LONG result;
};

static DWORD WINAPI ChangeDisplaySettingsThreadProc(LPVOID param) {
  ChangeDisplaySettingsCtx* ctx = (ChangeDisplaySettingsCtx*)param;
  if (!ctx) {
    return 0;
  }
  // Note: ChangeDisplaySettingsExW takes a non-const DEVMODEW*.
  ctx->result = ChangeDisplaySettingsExW(NULL, &ctx->dm, NULL, 0, NULL);
  return 0;
}

static bool ChangeDisplaySettingsExWithTimeout(const DEVMODEW& target,
                                               DWORD timeout_ms,
                                               LONG* out_result,
                                               bool* out_timed_out,
                                               std::string* err) {
  if (err) {
    err->clear();
  }
  if (out_result) {
    *out_result = DISP_CHANGE_FAILED;
  }
  if (out_timed_out) {
    *out_timed_out = false;
  }

  ChangeDisplaySettingsCtx* ctx = new ChangeDisplaySettingsCtx();
  ctx->dm = target;
  ctx->result = DISP_CHANGE_FAILED;

  HANDLE thread = CreateThread(NULL, 0, ChangeDisplaySettingsThreadProc, ctx, 0, NULL);
  if (!thread) {
    if (err) {
      *err = "CreateThread(ChangeDisplaySettingsExW) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    delete ctx;
    return false;
  }

  DWORD w = WaitForSingleObject(thread, timeout_ms);
  if (w == WAIT_OBJECT_0) {
    CloseHandle(thread);
    if (out_result) {
      *out_result = ctx->result;
    }
    delete ctx;
    return true;
  }

  // Timeout or wait failure. Close the handle but do not free ctx (thread may still access it).
  CloseHandle(thread);
  if (err) {
    if (w == WAIT_TIMEOUT) {
      if (out_timed_out) {
        *out_timed_out = true;
      }
      *err = aerogpu_test::FormatString("ChangeDisplaySettingsExW timed out after %lu ms (target=%lux%lu)",
                                        (unsigned long)timeout_ms,
                                        (unsigned long)target.dmPelsWidth,
                                        (unsigned long)target.dmPelsHeight);
    } else {
      // WaitForSingleObject on a thread handle should not fail in normal circumstances, but if it
      // does, assume the mode-set worker thread might still be executing. Treat this similarly to
      // a timeout so callers avoid running concurrent mode-set attempts.
      if (out_timed_out) {
        *out_timed_out = true;
      }
      *err = "WaitForSingleObject(ChangeDisplaySettingsExW) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
  }
  return false;
}

static void PrintModeInfo(const char* label, const DEVMODEW& dm) {
  aerogpu_test::PrintfStdout(
      "INFO: %s: %lux%lu bpp=%lu freq=%lu fields=0x%08lX",
      label ? label : "<mode>",
      (unsigned long)dm.dmPelsWidth,
      (unsigned long)dm.dmPelsHeight,
      (unsigned long)dm.dmBitsPerPel,
      (unsigned long)dm.dmDisplayFrequency,
      (unsigned long)dm.dmFields);
}

static void PrintScanoutInfo(const char* label, const aerogpu_escape_query_scanout_out_v2& q) {
  const char* name = label ? label : "scanout";
  aerogpu_test::PrintfStdout(
      "INFO: %s: cached: enable=%lu width=%lu height=%lu format=%lu pitch=%lu",
      name,
      (unsigned long)q.base.cached_enable,
      (unsigned long)q.base.cached_width,
      (unsigned long)q.base.cached_height,
      (unsigned long)q.base.cached_format,
      (unsigned long)q.base.cached_pitch_bytes);
  aerogpu_test::PrintfStdout(
      "INFO: %s: mmio:   enable=%lu width=%lu height=%lu format=%lu pitch=%lu fb_gpa=0x%I64X",
      name,
      (unsigned long)q.base.mmio_enable,
      (unsigned long)q.base.mmio_width,
      (unsigned long)q.base.mmio_height,
      (unsigned long)q.base.mmio_format,
      (unsigned long)q.base.mmio_pitch_bytes,
      (unsigned long long)q.base.mmio_fb_gpa);

  const uint32_t flags = q.base.reserved0;
  const bool flags_valid = (flags & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID) != 0;
  const bool cached_fb_gpa_valid = (flags & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID) != 0;
  const bool post_display_released = (flags & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED) != 0;
  if (q.base.hdr.size >= sizeof(aerogpu_escape_query_scanout_out_v2)) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: flags=0x%08lX%s cached_fb_gpa=0x%I64X%s%s",
        name,
        (unsigned long)flags,
        flags_valid ? " (valid)" : " (legacy)",
        (unsigned long long)q.cached_fb_gpa,
        (flags_valid && cached_fb_gpa_valid) ? " (cached_fb_gpa_valid)" : "",
        (flags_valid && post_display_released) ? " (post_display_ownership_released)" : "");
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: flags=0x%08lX%s (no v2 cached_fb_gpa)",  //
                              name,
                              (unsigned long)flags,
                              flags_valid ? " (valid)" : " (legacy)");
  }
}

static bool GetCurrentDesktopMode(DEVMODEW* out, std::string* err) {
  if (err) {
    err->clear();
  }
  if (!out) {
    if (err) {
      *err = "GetCurrentDesktopMode: out == NULL";
    }
    return false;
  }
  ZeroMemory(out, sizeof(*out));
  out->dmSize = sizeof(*out);
  if (!EnumDisplaySettingsW(NULL, ENUM_CURRENT_SETTINGS, out)) {
    if (err) {
      *err = "EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }
  return true;
}

struct ModeScore {
  int score;
  DEVMODEW dm;
  bool present;

  ModeScore() : score(-1000000), present(false) { ZeroMemory(&dm, sizeof(dm)); }
};

static bool FindModeByResolution(DWORD target_w, DWORD target_h, const DEVMODEW& current, DEVMODEW* out) {
  ModeScore best;

  for (DWORD i = 0;; ++i) {
    DEVMODEW dm;
    ZeroMemory(&dm, sizeof(dm));
    dm.dmSize = sizeof(dm);
    if (!EnumDisplaySettingsW(NULL, i, &dm)) {
      break;
    }
    if (dm.dmPelsWidth != target_w || dm.dmPelsHeight != target_h) {
      continue;
    }
    if (dm.dmPelsWidth == current.dmPelsWidth && dm.dmPelsHeight == current.dmPelsHeight) {
      continue;
    }
    // Keep the mode switch conservative: stick to the current desktop bit depth. The scanout
    // validation logic assumes a 32bpp desktop (pitch >= width*4) like scanout_state_sanity.
    if (dm.dmBitsPerPel != current.dmBitsPerPel) {
      continue;
    }

    int score = 0;
    if (dm.dmBitsPerPel == 32) {
      score += 200;
    }
    if (dm.dmDisplayFrequency == 60) {
      score += 20;
    }
    if (dm.dmBitsPerPel == current.dmBitsPerPel) {
      score += 100;
    }
    if (current.dmDisplayFrequency != 0 && dm.dmDisplayFrequency == current.dmDisplayFrequency) {
      score += 10;
    }
    if ((dm.dmDisplayFlags & DM_INTERLACED) == 0) {
      score += 1;
    }

    if (!best.present || score > best.score) {
      best.present = true;
      best.score = score;
      best.dm = dm;
    }
  }

  if (!best.present) {
    return false;
  }
  if (out) {
    *out = best.dm;
  }
  return true;
}

static bool FindAnyAlternateMode(const DEVMODEW& current, DEVMODEW* out) {
  // Conservative fallback: pick the closest different resolution (prefer same bpp).
  bool found = false;
  DEVMODEW best;
  long long best_cost = 0;
  bool best_same_bpp = false;

  for (DWORD i = 0;; ++i) {
    DEVMODEW dm;
    ZeroMemory(&dm, sizeof(dm));
    dm.dmSize = sizeof(dm);
    if (!EnumDisplaySettingsW(NULL, i, &dm)) {
      break;
    }
    if (dm.dmPelsWidth == current.dmPelsWidth && dm.dmPelsHeight == current.dmPelsHeight) {
      continue;
    }
    if (dm.dmPelsWidth == 0 || dm.dmPelsHeight == 0) {
      continue;
    }
    if (dm.dmBitsPerPel != current.dmBitsPerPel) {
      continue;
    }

    const bool same_bpp = (dm.dmBitsPerPel == current.dmBitsPerPel);
    const long long dw = (long long)dm.dmPelsWidth - (long long)current.dmPelsWidth;
    const long long dh = (long long)dm.dmPelsHeight - (long long)current.dmPelsHeight;
    const long long cost = (dw < 0 ? -dw : dw) + (dh < 0 ? -dh : dh);

    if (!found) {
      found = true;
      best = dm;
      best_cost = cost;
      best_same_bpp = same_bpp;
      continue;
    }

    // Prefer same-bpp even if the resolution delta is a bit larger.
    if (same_bpp != best_same_bpp) {
      if (same_bpp) {
        best = dm;
        best_cost = cost;
        best_same_bpp = same_bpp;
      }
      continue;
    }
    if (cost < best_cost) {
      best = dm;
      best_cost = cost;
      best_same_bpp = same_bpp;
    }
  }

  if (!found) {
    return false;
  }
  if (out) {
    *out = best;
  }
  return true;
}

static bool FindAlternateDesktopMode(const DEVMODEW& current, DEVMODEW* out, std::string* err) {
  if (err) {
    err->clear();
  }
  if (!out) {
    if (err) {
      *err = "FindAlternateDesktopMode: out == NULL";
    }
    return false;
  }

  struct TargetRes {
    DWORD w;
    DWORD h;
    TargetRes() : w(0), h(0) {}
    TargetRes(DWORD w_, DWORD h_) : w(w_), h(h_) {}
  };

  // Prefer switching between common, conservative modes. If we're already at one, prefer the other.
  std::vector<TargetRes> targets;
  if (current.dmPelsWidth == 800 && current.dmPelsHeight == 600) {
    targets.push_back(TargetRes(1024, 768));
    targets.push_back(TargetRes(800, 600));
  } else if (current.dmPelsWidth == 1024 && current.dmPelsHeight == 768) {
    targets.push_back(TargetRes(800, 600));
    targets.push_back(TargetRes(1024, 768));
  } else {
    // If we're above ~1024x768, prefer downscaling to 800x600; otherwise, prefer upscaling to 1024x768.
    const unsigned long long cur_area =
        (unsigned long long)current.dmPelsWidth * (unsigned long long)current.dmPelsHeight;
    const unsigned long long ref_area = 1024ull * 768ull;
    if (cur_area >= ref_area) {
      targets.push_back(TargetRes(800, 600));
      targets.push_back(TargetRes(1024, 768));
    } else {
      targets.push_back(TargetRes(1024, 768));
      targets.push_back(TargetRes(800, 600));
    }
  }

  for (size_t i = 0; i < targets.size(); ++i) {
    const TargetRes& t = targets[i];
    if (t.w == current.dmPelsWidth && t.h == current.dmPelsHeight) {
      continue;
    }
    if (FindModeByResolution(t.w, t.h, current, out)) {
      return true;
    }
  }

  if (FindAnyAlternateMode(current, out)) {
    return true;
  }

  if (err) {
    *err = "no alternate display mode found via EnumDisplaySettings";
  }
  return false;
}

static bool ApplyDisplayModeAndWaitEx(const DEVMODEW& target,
                                      DWORD timeout_ms,
                                      bool* out_change_timed_out,
                                      std::string* err) {
  if (err) {
    err->clear();
  }
  if (out_change_timed_out) {
    *out_change_timed_out = false;
  }

  DEVMODEW dm = target;  // ChangeDisplaySettingsExW takes a non-const pointer.
  const DWORD start = GetTickCount();
  LONG r = DISP_CHANGE_FAILED;
  std::string change_err;
  bool change_timed_out = false;
  if (!ChangeDisplaySettingsExWithTimeout(dm, timeout_ms, &r, &change_timed_out, &change_err)) {
    if (out_change_timed_out) {
      *out_change_timed_out = change_timed_out;
    }
    if (err) {
      *err = change_err;
    }
    return false;
  }
  if (r != DISP_CHANGE_SUCCESSFUL) {
    if (err) {
      *err = aerogpu_test::FormatString("ChangeDisplaySettingsExW failed (%ld: %s)",
                                        (long)r,
                                        DispChangeCodeToString(r));
    }
    return false;
  }

  const DWORD elapsed = GetTickCount() - start;
  DWORD remaining = 0;
  if (elapsed < timeout_ms) {
    remaining = timeout_ms - elapsed;
  }
  DWORD wait_start = GetTickCount();
  DEVMODEW last_mode;
  ZeroMemory(&last_mode, sizeof(last_mode));
  last_mode.dmSize = sizeof(last_mode);
  bool have_last_mode = false;
  for (;;) {
    // Prefer EnumDisplaySettingsW(ENUM_CURRENT_SETTINGS) over GetSystemMetrics: metrics can lag or
    // reflect virtualized work areas in some configurations.
    DEVMODEW cur;
    ZeroMemory(&cur, sizeof(cur));
    cur.dmSize = sizeof(cur);
    if (EnumDisplaySettingsW(NULL, ENUM_CURRENT_SETTINGS, &cur)) {
      last_mode = cur;
      have_last_mode = true;
      if (cur.dmPelsWidth == target.dmPelsWidth && cur.dmPelsHeight == target.dmPelsHeight) {
        return true;
      }
    } else {
      // Fallback signal: desktop metrics. Only trust this when EnumDisplaySettingsW itself fails.
      const int w = GetSystemMetrics(SM_CXSCREEN);
      const int h = GetSystemMetrics(SM_CYSCREEN);
      if (w == (int)target.dmPelsWidth && h == (int)target.dmPelsHeight) {
        return true;
      }
    }
    if (remaining == 0 || GetTickCount() - wait_start >= remaining) {
      break;
    }
    Sleep(50);
  }

  if (err) {
    const int w = GetSystemMetrics(SM_CXSCREEN);
    const int h = GetSystemMetrics(SM_CYSCREEN);
    if (have_last_mode) {
      *err = aerogpu_test::FormatString(
          "desktop resolution did not update within %lu ms (metrics=%dx%d mode=%lux%lu want=%lux%lu)",
          (unsigned long)timeout_ms,
          w,
          h,
          (unsigned long)last_mode.dmPelsWidth,
          (unsigned long)last_mode.dmPelsHeight,
          (unsigned long)target.dmPelsWidth,
          (unsigned long)target.dmPelsHeight);
    } else {
      *err = aerogpu_test::FormatString(
          "desktop resolution did not update within %lu ms (metrics=%dx%d want=%lux%lu)",
          (unsigned long)timeout_ms,
          w,
          h,
          (unsigned long)target.dmPelsWidth,
          (unsigned long)target.dmPelsHeight);
    }
  }
  return false;
}

static bool WaitForScanoutMatch(const D3DKMT_FUNCS* kmt,
                                D3DKMT_HANDLE adapter,
                                DWORD expected_w,
                                DWORD expected_h,
                                DWORD timeout_ms,
                                aerogpu_escape_query_scanout_out_v2* out_last,
                                NTSTATUS* out_status,
                                std::string* err) {
  if (err) {
    err->clear();
  }
  if (out_last) {
    ZeroMemory(out_last, sizeof(*out_last));
  }
  if (out_status) {
    *out_status = 0;
  }
  if (!kmt || !adapter) {
    if (err) {
      *err = "WaitForScanoutMatch: invalid kmt/adapter";
    }
    return false;
  }

  DWORD start = GetTickCount();
  aerogpu_escape_query_scanout_out_v2 last;
  ZeroMemory(&last, sizeof(last));
  NTSTATUS last_status = 0;
  bool got_any = false;

  for (;;) {
    aerogpu_escape_query_scanout_out_v2 q;
    NTSTATUS st = 0;
    const bool ok = aerogpu_test::kmt::AerogpuQueryScanoutV2(kmt, adapter, 0, &q, &st);
    last_status = st;
    if (ok) {
      last = q;
      got_any = true;
    }

    const uint32_t flags = q.base.reserved0;
    const bool flags_valid = (flags & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID) != 0;
    const bool post_display_released =
        (flags & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED) != 0;

    const unsigned long long row_bytes = (unsigned long long)expected_w * 4ull;
    bool format_ok = true;
    if (q.base.cached_format != 0 && q.base.mmio_format != 0 && q.base.cached_format != q.base.mmio_format) {
      format_ok = false;
    }
    const bool released_ok = flags_valid ? !post_display_released : true;
    const bool match =
        ok && released_ok && (q.base.cached_enable != 0) && (q.base.mmio_enable != 0) && (q.base.cached_width == expected_w) &&
        (q.base.cached_height == expected_h) && (q.base.mmio_width == expected_w) && (q.base.mmio_height == expected_h) &&
        (q.base.mmio_fb_gpa != 0) && (q.base.cached_pitch_bytes != 0) && (q.base.mmio_pitch_bytes != 0) &&
        (q.base.cached_pitch_bytes == q.base.mmio_pitch_bytes) && ((unsigned long long)q.base.cached_pitch_bytes >= row_bytes) &&
        format_ok;
    if (match) {
      if (out_last) {
        *out_last = q;
      }
      if (out_status) {
        *out_status = st;
      }
      return true;
    }

    if (GetTickCount() - start >= timeout_ms) {
      break;
    }
    Sleep(100);
  }

  if (out_last) {
    *out_last = last;
  }
  if (out_status) {
    *out_status = last_status;
  }
    if (err) {
      if (!got_any) {
        *err = aerogpu_test::FormatString("D3DKMTEscape(query-scanout) failed (NTSTATUS=0x%08lX)",
                                          (unsigned long)last_status);
      } else {
        *err = aerogpu_test::FormatString(
            "scanout did not match within %lu ms (want=%lux%lu flags=0x%08lX cached_fb_gpa=0x%I64X cached: en=%lu %lux%lu fmt=%lu pitch=%lu mmio: en=%lu %lux%lu fmt=%lu pitch=%lu fb_gpa=0x%I64X)",
            (unsigned long)timeout_ms,
            (unsigned long)expected_w,
            (unsigned long)expected_h,
            (unsigned long)last.base.reserved0,
            (unsigned long long)last.cached_fb_gpa,
            (unsigned long)last.base.cached_enable,
            (unsigned long)last.base.cached_width,
            (unsigned long)last.base.cached_height,
            (unsigned long)last.base.cached_format,
            (unsigned long)last.base.cached_pitch_bytes,
            (unsigned long)last.base.mmio_enable,
            (unsigned long)last.base.mmio_width,
            (unsigned long)last.base.mmio_height,
            (unsigned long)last.base.mmio_format,
            (unsigned long)last.base.mmio_pitch_bytes,
            (unsigned long long)last.base.mmio_fb_gpa);
      }
    }
    return false;
  }

static bool ApplyDisplayModeAndWait(const DEVMODEW& target, DWORD timeout_ms, std::string* err) {
  return ApplyDisplayModeAndWaitEx(target, timeout_ms, NULL, err);
}

struct ScopedModeRestore {
  DEVMODEW original;
  bool armed;

  explicit ScopedModeRestore(const DEVMODEW& dm) : original(dm), armed(false) {}

  void Arm() {
    g_emergency_restore_mode = original;
    InterlockedExchange(&g_emergency_restore_needed, 1);
    armed = true;
  }
  void Disarm() {
    armed = false;
    InterlockedExchange(&g_emergency_restore_needed, 0);
  }

  bool RestoreNow(std::string* err) {
    if (!armed) {
      return true;
    }
    std::string tmp;
    bool timed_out = false;
    if (!ApplyDisplayModeAndWaitEx(original, 5000, &timed_out, &tmp)) {
      // If the restore attempt itself timed out (call didn't return), avoid retrying in a destructor
      // while the timed-out worker thread may still be executing.
      if (timed_out) {
        armed = false;
        // Mirror the safety behavior in aerogpu_test_kmt.h: if a timed call may still be executing
        // inside gdi/user32 paths, avoid teardown that can deadlock (CloseAdapter/FreeLibrary).
        InterlockedExchange(&aerogpu_test::kmt::g_skip_close_adapter, 1);
        InterlockedExchange(&g_emergency_restore_needed, 0);
      }
      if (err) {
        *err = tmp;
      }
      return false;
    }
    armed = false;
    InterlockedExchange(&g_emergency_restore_needed, 0);
    return true;
  }

  ~ScopedModeRestore() {
    if (!armed) {
      return;
    }
    // Best-effort only (cannot change the test result from a destructor).
    std::string tmp;
    bool timed_out = false;
    (void)ApplyDisplayModeAndWaitEx(original, 5000, &timed_out, &tmp);
  }
};

static int RunModesetRoundtripSanity(int argc, char** argv) {
  const char* kTestName = "modeset_roundtrip_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Switches the desktop display mode to an alternate supported resolution and back, validating AeroGPU scanout "
        "state (cached/MMIO) tracks the desktop resolution after each switch.");
    aerogpu_test::PrintfStdout("Notes:");
    aerogpu_test::PrintfStdout("  - Requires a 32bpp desktop mode (dmBitsPerPel=32).");
    aerogpu_test::PrintfStdout("  - Requires at least two modes reported by EnumDisplaySettingsW.");
    aerogpu_test::PrintfStdout("  - Temporarily changes the desktop resolution; will best-effort restore on exit/crash.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  const int initial_w = GetSystemMetrics(SM_CXSCREEN);
  const int initial_h = GetSystemMetrics(SM_CYSCREEN);

  DEVMODEW original;
  std::string mode_err;
  if (!GetCurrentDesktopMode(&original, &mode_err)) {
    return reporter.Fail("%s", mode_err.c_str());
  }
  PrintModeInfo("original", original);
  aerogpu_test::PrintfStdout("INFO: %s: GetSystemMetrics: %dx%d", kTestName, initial_w, initial_h);
  if (initial_w != (int)original.dmPelsWidth || initial_h != (int)original.dmPelsHeight) {
    aerogpu_test::PrintfStdout("INFO: %s: WARNING: GetSystemMetrics != EnumDisplaySettingsW (metrics=%dx%d mode=%lux%lu)",
                               kTestName,
                               initial_w,
                               initial_h,
                               (unsigned long)original.dmPelsWidth,
                               (unsigned long)original.dmPelsHeight);
  }
  if (original.dmBitsPerPel != 32) {
    return reporter.Fail("expected a 32bpp desktop mode (dmBitsPerPel=32), but got %lu",
                         (unsigned long)original.dmBitsPerPel);
  }

  // Best-effort: attempt to restore the original mode if the process receives Ctrl-C/close or
  // crashes with an unhandled exception.
  g_emergency_restore_mode = original;
  (void)SetConsoleCtrlHandler(ConsoleCtrlHandler, TRUE);
  (void)SetUnhandledExceptionFilter(UnhandledExceptionFilterProc);

  DEVMODEW alternate;
  std::string alt_err;
  if (!FindAlternateDesktopMode(original, &alternate, &alt_err)) {
    return reporter.Fail("%s (need at least two reported modes for a roundtrip)", alt_err.c_str());
  }
  PrintModeInfo("alternate", alternate);

  // Open the adapter once; it should remain valid across mode sets.
  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return reporter.Fail("%s", kmt_err.c_str());
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", open_err.c_str());
  }

  // Validate that the scanout query escape exists before attempting to mode-set.
  aerogpu_escape_query_scanout_out_v2 q0;
  NTSTATUS st0 = 0;
  if (!aerogpu_test::kmt::AerogpuQueryScanoutV2(&kmt, adapter, 0, &q0, &st0)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (st0 == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: QUERY_SCANOUT escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("D3DKMTEscape(query-scanout) failed (NTSTATUS=0x%08lX)", (unsigned long)st0);
  }

  // Baseline sanity: scanout should already match the current desktop mode before we mode-set.
  aerogpu_escape_query_scanout_out_v2 q_init;
  NTSTATUS st_init = 0;
  std::string scanout_err0;
  if (!WaitForScanoutMatch(&kmt,
                           adapter,
                           (DWORD)original.dmPelsWidth,
                           (DWORD)original.dmPelsHeight,
                           2000,
                           &q_init,
                           &st_init,
                           &scanout_err0)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s (cached=%lux%lu mmio=%lux%lu)",
                         scanout_err0.c_str(),
                         (unsigned long)q_init.base.cached_width,
                         (unsigned long)q_init.base.cached_height,
                         (unsigned long)q_init.base.mmio_width,
                         (unsigned long)q_init.base.mmio_height);
  }
  PrintScanoutInfo("baseline_scanout", q_init);

  // Ensure we always attempt to restore the original mode on any early-return failure.
  ScopedModeRestore restore(original);
  // Arm the restore guard before attempting the mode set: even if the mode change partially
  // succeeds but our polling times out, we still want a best-effort revert.
  restore.Arm();

  std::string apply_err;
  bool modeset_timed_out = false;
  if (!ApplyDisplayModeAndWaitEx(alternate, 5000, &modeset_timed_out, &apply_err)) {
    if (!modeset_timed_out) {
      // Best-effort restore: the mode change may have partially applied even if we timed out waiting
      // for GetSystemMetrics() to update.
      std::string restore_err;
      (void)restore.RestoreNow(&restore_err);
    } else {
      // Avoid spawning a second concurrent mode-set while the timed-out worker thread may still be running.
      restore.Disarm();
      InterlockedExchange(&aerogpu_test::kmt::g_skip_close_adapter, 1);
    }
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", apply_err.c_str());
  }

  // Give the driver a moment to program scanout regs before polling.
  Sleep(100);

  const int switched_metrics_w = GetSystemMetrics(SM_CXSCREEN);
  const int switched_metrics_h = GetSystemMetrics(SM_CYSCREEN);
  DEVMODEW switched_mode;
  std::string switched_mode_err;
  if (!GetCurrentDesktopMode(&switched_mode, &switched_mode_err)) {
    std::string restore_err;
    (void)restore.RestoreNow(&restore_err);
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", switched_mode_err.c_str());
  }
  PrintModeInfo("switched", switched_mode);
  aerogpu_test::PrintfStdout("INFO: %s: switched GetSystemMetrics=%dx%d", kTestName, switched_metrics_w, switched_metrics_h);
  if (switched_metrics_w != (int)switched_mode.dmPelsWidth || switched_metrics_h != (int)switched_mode.dmPelsHeight) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: WARNING: switched GetSystemMetrics != EnumDisplaySettingsW (metrics=%dx%d mode=%lux%lu)",
        kTestName,
        switched_metrics_w,
        switched_metrics_h,
        (unsigned long)switched_mode.dmPelsWidth,
        (unsigned long)switched_mode.dmPelsHeight);
  }

  aerogpu_escape_query_scanout_out_v2 q1;
  NTSTATUS st1 = 0;
  std::string scanout_err1;
  if (!WaitForScanoutMatch(&kmt,
                           adapter,
                           (DWORD)switched_mode.dmPelsWidth,
                           (DWORD)switched_mode.dmPelsHeight,
                           5000,
                           &q1,
                           &st1,
                           &scanout_err1)) {
    // Best-effort restore before failing.
    std::string restore_err;
    (void)restore.RestoreNow(&restore_err);
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s (cached=%lux%lu mmio=%lux%lu)",
                         scanout_err1.c_str(),
                         (unsigned long)q1.base.cached_width,
                         (unsigned long)q1.base.cached_height,
                         (unsigned long)q1.base.mmio_width,
                         (unsigned long)q1.base.mmio_height);
  }
  PrintScanoutInfo("switched_scanout", q1);

  // Switch back to the original mode and validate scanout again.
  bool restore_timed_out = false;
  if (!ApplyDisplayModeAndWaitEx(original, 5000, &restore_timed_out, &apply_err)) {
    if (restore_timed_out) {
      // Avoid retrying in a destructor while the timed-out worker thread may still be executing.
      restore.Disarm();
      InterlockedExchange(&aerogpu_test::kmt::g_skip_close_adapter, 1);
    }
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("failed to restore original mode: %s", apply_err.c_str());
  }
  restore.Disarm();

  Sleep(100);

  const int restored_metrics_w = GetSystemMetrics(SM_CXSCREEN);
  const int restored_metrics_h = GetSystemMetrics(SM_CYSCREEN);
  DEVMODEW restored_mode;
  std::string restored_mode_err;
  if (!GetCurrentDesktopMode(&restored_mode, &restored_mode_err)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", restored_mode_err.c_str());
  }
  PrintModeInfo("restored", restored_mode);
  aerogpu_test::PrintfStdout("INFO: %s: restored GetSystemMetrics=%dx%d", kTestName, restored_metrics_w, restored_metrics_h);
  if (restored_metrics_w != (int)restored_mode.dmPelsWidth || restored_metrics_h != (int)restored_mode.dmPelsHeight) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: WARNING: restored GetSystemMetrics != EnumDisplaySettingsW (metrics=%dx%d mode=%lux%lu)",
        kTestName,
        restored_metrics_w,
        restored_metrics_h,
        (unsigned long)restored_mode.dmPelsWidth,
        (unsigned long)restored_mode.dmPelsHeight);
  }

  aerogpu_escape_query_scanout_out_v2 q2;
  NTSTATUS st2 = 0;
  std::string scanout_err2;
  if (!WaitForScanoutMatch(&kmt,
                           adapter,
                           (DWORD)restored_mode.dmPelsWidth,
                           (DWORD)restored_mode.dmPelsHeight,
                           5000,
                           &q2,
                           &st2,
                           &scanout_err2)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s (cached=%lux%lu mmio=%lux%lu)",
                         scanout_err2.c_str(),
                         (unsigned long)q2.base.cached_width,
                         (unsigned long)q2.base.cached_height,
                         (unsigned long)q2.base.mmio_width,
                         (unsigned long)q2.base.mmio_height);
  }
  PrintScanoutInfo("restored_scanout", q2);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunModesetRoundtripSanity(argc, argv);
}
