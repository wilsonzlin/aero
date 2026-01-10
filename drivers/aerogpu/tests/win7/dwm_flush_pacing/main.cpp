#include "..\\common\\aerogpu_test_common.h"

#include <dwmapi.h>

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static int RunDwmFlushPacing(int argc, char** argv) {
  const char* kTestName = "dwm_flush_pacing";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--samples=N] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=120");
    aerogpu_test::PrintfStdout("Measures DWM pacing by timing successive DwmFlush() calls.");
    return 0;
  }
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  uint32_t samples = 120;
  std::string samples_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--samples", &samples_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --samples: %s", err.c_str());
    }
  }

  // DWM is per-session; composition is typically disabled in RDP sessions.
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

  // Ensure DWM composition is enabled (otherwise DwmFlush can return immediately).
  BOOL enabled = FALSE;
  HRESULT hr = DwmIsCompositionEnabled(&enabled);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "DwmIsCompositionEnabled", hr);
  }
  if (!enabled) {
    aerogpu_test::PrintfStdout("INFO: %s: composition disabled; attempting to enable...", kTestName);
    hr = DwmEnableComposition(DWM_EC_ENABLECOMPOSITION);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "DwmEnableComposition(ENABLE)", hr);
    }
    // Poll for up to ~5 seconds.
    const DWORD start = GetTickCount();
    while (!enabled && (GetTickCount() - start) <= 5000) {
      Sleep(100);
      enabled = FALSE;
      hr = DwmIsCompositionEnabled(&enabled);
      if (FAILED(hr)) {
        return aerogpu_test::FailHresult(kTestName, "DwmIsCompositionEnabled(after enable)", hr);
      }
    }
  }

  if (!enabled) {
    return aerogpu_test::Fail(kTestName, "composition is DISABLED; cannot measure DwmFlush pacing");
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  // Warm up once to avoid counting first-time initialization.
  hr = DwmFlush();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "DwmFlush(warmup)", hr);
  }

  if (samples < 5) {
    samples = 5;
  }

  std::vector<double> deltas_ms;
  deltas_ms.reserve(samples);

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);
  for (uint32_t i = 0; i < samples; ++i) {
    hr = DwmFlush();
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "DwmFlush", hr);
    }
    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt = QpcToMs(now.QuadPart - last.QuadPart, qpc_freq);
    deltas_ms.push_back(dt);
    last = now;
  }

  double sum = 0.0;
  double min_ms = 1e9;
  double max_ms = 0.0;
  for (size_t i = 0; i < deltas_ms.size(); ++i) {
    const double v = deltas_ms[i];
    sum += v;
    if (v < min_ms) min_ms = v;
    if (v > max_ms) max_ms = v;
  }
  const double avg_ms = sum / (double)deltas_ms.size();

  aerogpu_test::PrintfStdout("INFO: %s: DwmFlush pacing over %u samples: avg=%.3fms min=%.3fms max=%.3fms",
                             kTestName,
                             (unsigned)samples,
                             avg_ms,
                             min_ms,
                             max_ms);

  // Heuristic pass/fail:
  //
  // - If DwmFlush returns almost immediately, DWM isn't pacing on vblank (or composition isn't really active).
  // - If we see multi-hundred-ms gaps, something is stalling the compositor path (often missing/broken vblank).
  //
  // Keep these thresholds generous: this test is intended to detect "completely broken" pacing, not to
  // enforce perfect refresh accuracy.
  if (avg_ms < 2.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly fast DwmFlush pacing (avg=%.3fms)", avg_ms);
  }
  if (max_ms > 250.0) {
    return aerogpu_test::Fail(kTestName, "unexpectedly large DwmFlush gap (max=%.3fms)", max_ms);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  return RunDwmFlushPacing(argc, argv);
}
