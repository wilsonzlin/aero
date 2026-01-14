#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"
#include "..\\common\\aerogpu_test_scanout_diag.h"

#include <dwmapi.h>

static int RunDwmProbe(int argc, char** argv) {
  const char* kTestName = "d3d9ex_dwm_probe";
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  // DWM is per-session; composition is typically disabled in RDP sessions.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping composition check", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail(
        "running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  aerogpu_test::AerogpuScanoutDiag scanout_diag;
  bool have_scanout_diag = false;
  {
    aerogpu_test::kmt::D3DKMT_FUNCS kmt;
    std::string kmt_err;
    if (aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
      aerogpu_test::kmt::D3DKMT_HANDLE adapter = 0;
      std::string open_err;
      if (aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
        have_scanout_diag = aerogpu_test::TryQueryAerogpuScanoutDiagWithKmt(
            &kmt, (uint32_t)adapter, 0 /* vidpn_source_id */, &scanout_diag);
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      }
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    }
  }
  if (have_scanout_diag) {
    aerogpu_test::PrintfStdout("INFO: %s: scanout: flags=0x%08lX%s%s cached_enable=%lu mmio_enable=%lu",
                               kTestName,
                               (unsigned long)scanout_diag.flags_u32,
                               scanout_diag.flags_valid ? "" : " (flags_invalid)",
                               scanout_diag.post_display_ownership_released ? " (post_display_ownership_released)" : "",
                               (unsigned long)scanout_diag.cached_enable,
                               (unsigned long)scanout_diag.mmio_enable);
    if (scanout_diag.flags_valid && scanout_diag.post_display_ownership_released) {
      return reporter.Fail("post_display_ownership_released flag is set in QUERY_SCANOUT (flags=0x%08lX)",
                           (unsigned long)scanout_diag.flags_u32);
    }
    if (scanout_diag.cached_enable == 0 || scanout_diag.mmio_enable == 0) {
      return reporter.Fail("scanout enable appears off (cached_enable=%lu mmio_enable=%lu flags=0x%08lX)",
                           (unsigned long)scanout_diag.cached_enable,
                           (unsigned long)scanout_diag.mmio_enable,
                           (unsigned long)scanout_diag.flags_u32);
    }
  }

  BOOL enabled = FALSE;
  HRESULT hr = DwmIsCompositionEnabled(&enabled);
  if (FAILED(hr)) {
    return reporter.FailHresult("DwmIsCompositionEnabled", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: composition initially %s",
                             kTestName,
                             enabled ? "ENABLED" : "DISABLED");

  if (!enabled) {
    aerogpu_test::PrintfStdout("INFO: %s: attempting to enable composition...", kTestName);
    hr = DwmEnableComposition(DWM_EC_ENABLECOMPOSITION);
    if (FAILED(hr)) {
      return reporter.FailHresult("DwmEnableComposition(ENABLE)", hr);
    }

    // Give DWM a moment to apply changes (poll up to ~5s).
    const DWORD start = GetTickCount();
    while (TRUE) {
      Sleep(100);
      enabled = FALSE;
      hr = DwmIsCompositionEnabled(&enabled);
      if (FAILED(hr)) {
        return reporter.FailHresult("DwmIsCompositionEnabled(after enable)", hr);
      }
      if (enabled) {
        break;
      }
      if ((GetTickCount() - start) > 5000) {
        break;
      }
    }
  }

  DWORD color = 0;
  BOOL opaque_blend = FALSE;
  hr = DwmGetColorizationColor(&color, &opaque_blend);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: colorization=0x%08lX opaqueBlend=%d",
                               kTestName,
                               (unsigned long)color,
                               (int)opaque_blend);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: DwmGetColorizationColor failed with %s",
                               kTestName,
                               aerogpu_test::HresultToString(hr).c_str());
  }

  if (!enabled) {
    return reporter.Fail("composition is DISABLED");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunDwmProbe(argc, argv);
}
