#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

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
  ctx->result = ChangeDisplaySettingsExW(NULL, &ctx->dm, NULL, CDS_FULLSCREEN, NULL);
  return 0;
}

static bool ChangeDisplaySettingsExWithTimeout(const DEVMODEW& target,
                                               DWORD timeout_ms,
                                               LONG* out_result,
                                               std::string* err) {
  if (err) {
    err->clear();
  }
  if (out_result) {
    *out_result = DISP_CHANGE_FAILED;
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
      *err = aerogpu_test::FormatString("ChangeDisplaySettingsExW timed out after %lu ms", (unsigned long)timeout_ms);
    } else {
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

static bool ApplyDisplayModeAndWait(const DEVMODEW& target, DWORD timeout_ms, std::string* err) {
  if (err) {
    err->clear();
  }

  DEVMODEW dm = target;  // ChangeDisplaySettingsExW takes a non-const pointer.
  const DWORD start = GetTickCount();
  LONG r = DISP_CHANGE_FAILED;
  std::string change_err;
  if (!ChangeDisplaySettingsExWithTimeout(dm, timeout_ms, &r, &change_err)) {
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
  for (;;) {
    const int w = GetSystemMetrics(SM_CXSCREEN);
    const int h = GetSystemMetrics(SM_CYSCREEN);
    if (w == (int)target.dmPelsWidth && h == (int)target.dmPelsHeight) {
      return true;
    }
    if (remaining == 0 || GetTickCount() - wait_start >= remaining) {
      break;
    }
    Sleep(50);
  }

  if (err) {
    const int w = GetSystemMetrics(SM_CXSCREEN);
    const int h = GetSystemMetrics(SM_CYSCREEN);
    *err = aerogpu_test::FormatString("desktop resolution did not update within %lu ms (have=%dx%d want=%lux%lu)",
                                      (unsigned long)timeout_ms,
                                      w,
                                      h,
                                      (unsigned long)target.dmPelsWidth,
                                      (unsigned long)target.dmPelsHeight);
  }
  return false;
}

static bool WaitForScanoutMatch(const D3DKMT_FUNCS* kmt,
                                D3DKMT_HANDLE adapter,
                                DWORD expected_w,
                                DWORD expected_h,
                                DWORD timeout_ms,
                                aerogpu_escape_query_scanout_out* out_last,
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
  aerogpu_escape_query_scanout_out last;
  ZeroMemory(&last, sizeof(last));
  NTSTATUS last_status = 0;
  bool got_any = false;

  for (;;) {
    aerogpu_escape_query_scanout_out q;
    NTSTATUS st = 0;
    const bool ok = aerogpu_test::kmt::AerogpuQueryScanout(kmt, adapter, 0, &q, &st);
    last_status = st;
    if (ok) {
      last = q;
      got_any = true;
    }

    const unsigned long long row_bytes = (unsigned long long)expected_w * 4ull;
    const bool match =
        ok && (q.cached_enable != 0) && (q.mmio_enable != 0) && (q.cached_width == expected_w) &&
        (q.cached_height == expected_h) && (q.mmio_width == expected_w) && (q.mmio_height == expected_h) &&
        (q.mmio_fb_gpa != 0) && (q.cached_pitch_bytes != 0) && (q.mmio_pitch_bytes != 0) &&
        (q.cached_pitch_bytes == q.mmio_pitch_bytes) && ((unsigned long long)q.cached_pitch_bytes >= row_bytes);
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
          "scanout did not match within %lu ms (want=%lux%lu cached=%lux%lu pitch=%lu mmio=%lux%lu pitch=%lu fb_gpa=0x%I64X)",
          (unsigned long)timeout_ms,
          (unsigned long)expected_w,
          (unsigned long)expected_h,
          (unsigned long)last.cached_width,
          (unsigned long)last.cached_height,
          (unsigned long)last.cached_pitch_bytes,
          (unsigned long)last.mmio_width,
          (unsigned long)last.mmio_height,
          (unsigned long)last.mmio_pitch_bytes,
          (unsigned long long)last.mmio_fb_gpa);
    }
  }
  return false;
}

struct ScopedModeRestore {
  DEVMODEW original;
  bool armed;

  explicit ScopedModeRestore(const DEVMODEW& dm) : original(dm), armed(false) {}

  void Arm() { armed = true; }
  void Disarm() { armed = false; }

  bool RestoreNow(std::string* err) {
    if (!armed) {
      return true;
    }
    std::string tmp;
    if (!ApplyDisplayModeAndWait(original, 5000, &tmp)) {
      if (err) {
        *err = tmp;
      }
      return false;
    }
    armed = false;
    return true;
  }

  ~ScopedModeRestore() {
    if (!armed) {
      return;
    }
    // Best-effort only (cannot change the test result from a destructor).
    std::string tmp;
    (void)ApplyDisplayModeAndWait(original, 5000, &tmp);
  }
};

static int RunModesetRoundtripSanity(int argc, char** argv) {
  const char* kTestName = "modeset_roundtrip_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Switches the desktop display mode to an alternate supported resolution and back, validating AeroGPU scanout "
        "state (cached/MMIO) tracks the desktop resolution after each switch.");
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

  DEVMODEW alternate;
  std::string alt_err;
  if (!FindAlternateDesktopMode(original, &alternate, &alt_err)) {
    aerogpu_test::PrintfStdout("INFO: %s: %s; skipping", kTestName, alt_err.c_str());
    reporter.SetSkipped("no_alternate_mode");
    return reporter.Pass();
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
  aerogpu_escape_query_scanout_out q0;
  NTSTATUS st0 = 0;
  if (!aerogpu_test::kmt::AerogpuQueryScanout(&kmt, adapter, 0, &q0, &st0)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (st0 == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: QUERY_SCANOUT escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("D3DKMTEscape(query-scanout) failed (NTSTATUS=0x%08lX)", (unsigned long)st0);
  }

  // Ensure we always attempt to restore the original mode on any early-return failure.
  ScopedModeRestore restore(original);
  // Arm the restore guard before attempting the mode set: even if the mode change partially
  // succeeds but our polling times out, we still want a best-effort revert.
  restore.Arm();

  std::string apply_err;
  if (!ApplyDisplayModeAndWait(alternate, 5000, &apply_err)) {
    // Best-effort restore: the mode change may have partially applied even if we timed out waiting
    // for GetSystemMetrics() to update.
    std::string restore_err;
    (void)restore.RestoreNow(&restore_err);
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", apply_err.c_str());
  }

  // Give the driver a moment to program scanout regs before polling.
  Sleep(100);

  const int switched_w = GetSystemMetrics(SM_CXSCREEN);
  const int switched_h = GetSystemMetrics(SM_CYSCREEN);
  aerogpu_test::PrintfStdout("INFO: %s: switched desktop=%dx%d", kTestName, switched_w, switched_h);

  aerogpu_escape_query_scanout_out q1;
  NTSTATUS st1 = 0;
  std::string scanout_err1;
  if (!WaitForScanoutMatch(&kmt,
                           adapter,
                           (DWORD)switched_w,
                           (DWORD)switched_h,
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
                         (unsigned long)q1.cached_width,
                         (unsigned long)q1.cached_height,
                         (unsigned long)q1.mmio_width,
                         (unsigned long)q1.mmio_height);
  }

  // Switch back to the original mode and validate scanout again.
  if (!ApplyDisplayModeAndWait(original, 5000, &apply_err)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("failed to restore original mode: %s", apply_err.c_str());
  }
  restore.Disarm();

  Sleep(100);

  const int restored_w = GetSystemMetrics(SM_CXSCREEN);
  const int restored_h = GetSystemMetrics(SM_CYSCREEN);
  aerogpu_test::PrintfStdout("INFO: %s: restored desktop=%dx%d", kTestName, restored_w, restored_h);

  aerogpu_escape_query_scanout_out q2;
  NTSTATUS st2 = 0;
  std::string scanout_err2;
  if (!WaitForScanoutMatch(&kmt,
                           adapter,
                           (DWORD)restored_w,
                           (DWORD)restored_h,
                           5000,
                           &q2,
                           &st2,
                           &scanout_err2)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s (cached=%lux%lu mmio=%lux%lu)",
                         scanout_err2.c_str(),
                         (unsigned long)q2.cached_width,
                         (unsigned long)q2.cached_height,
                         (unsigned long)q2.mmio_width,
                         (unsigned long)q2.mmio_height);
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunModesetRoundtripSanity(argc, argv);
}
