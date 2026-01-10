#include "..\\common\\aerogpu_test_common.h"

#include <dwmapi.h>

static int RunDwmProbe() {
  const char* kTestName = "d3d9ex_dwm_probe";

  // DWM is per-session; composition is typically disabled in RDP sessions.
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    return aerogpu_test::Fail(kTestName, "running in a remote session (SM_REMOTESESSION=1)");
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

    // Give DWM a moment to apply changes.
    Sleep(250);

    enabled = FALSE;
    hr = DwmIsCompositionEnabled(&enabled);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "DwmIsCompositionEnabled(after enable)", hr);
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

int main() {
  return RunDwmProbe();
}

