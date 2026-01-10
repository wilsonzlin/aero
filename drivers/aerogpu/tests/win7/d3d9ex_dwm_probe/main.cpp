#include "..\\common\\aerogpu_test_common.h"

#include <dwmapi.h>

static int RunDwmProbe(int argc, char** argv) {
  const char* kTestName = "d3d9ex_dwm_probe";
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--allow-remote]", kTestName);
    return 0;
  }

  // DWM is per-session; composition is typically disabled in RDP sessions.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping composition check", kTestName);
      aerogpu_test::PrintfStdout("PASS: %s", kTestName);
      return 0;
    }
    return aerogpu_test::Fail(
        kTestName,
        "running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  BOOL enabled = FALSE;
  HRESULT hr = DwmIsCompositionEnabled(&enabled);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "DwmIsCompositionEnabled", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: composition initially %s",
                             kTestName,
                             enabled ? "ENABLED" : "DISABLED");

  if (!enabled) {
    aerogpu_test::PrintfStdout("INFO: %s: attempting to enable composition...", kTestName);
    hr = DwmEnableComposition(DWM_EC_ENABLECOMPOSITION);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "DwmEnableComposition(ENABLE)", hr);
    }

    // Give DWM a moment to apply changes (poll up to ~5s).
    const DWORD start = GetTickCount();
    while (TRUE) {
      Sleep(100);
      enabled = FALSE;
      hr = DwmIsCompositionEnabled(&enabled);
      if (FAILED(hr)) {
        return aerogpu_test::FailHresult(kTestName, "DwmIsCompositionEnabled(after enable)", hr);
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
    return aerogpu_test::Fail(kTestName, "composition is DISABLED");
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  return RunDwmProbe(argc, argv);
}
