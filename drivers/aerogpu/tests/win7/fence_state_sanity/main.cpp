#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunFenceStateSanity(int argc, char** argv) {
  const char* kTestName = "fence_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--interval-ms=N] [--json[=PATH]] [--allow-remote]",
        kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=10 --interval-ms=100");
    aerogpu_test::PrintfStdout(
        "Queries the AeroGPU QUERY_FENCE escape repeatedly and validates monotonicity/invariants.");
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

  uint32_t samples = 10;
  uint32_t interval_ms = 100;
  (void)aerogpu_test::GetArgUint32(argc, argv, "--samples", &samples);
  (void)aerogpu_test::GetArgUint32(argc, argv, "--interval-ms", &interval_ms);
  if (samples < 2) {
    samples = 2;
  }
  if (interval_ms < 1) {
    interval_ms = 1;
  }

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

  // Best-effort: also sanity-check QUERY_ERROR doesn't hang. This is particularly important around
  // power-transition windows where MMIO reads can be unsafe.
  {
    aerogpu_escape_query_error_out qe;
    NTSTATUS stErr = 0;
    const bool okErr = aerogpu_test::kmt::AerogpuQueryError(&kmt, adapter, &qe, &stErr);
    if (!okErr) {
      if (stErr == aerogpu_test::kmt::kStatusNotSupported || stErr == aerogpu_test::kmt::kStatusInvalidParameter) {
        aerogpu_test::PrintfStdout("INFO: %s: QUERY_ERROR escape not supported; skipping", kTestName);
      } else {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("D3DKMTEscape(query-error) failed (NTSTATUS=0x%08lX)", (unsigned long)stErr);
      }
    } else if (qe.hdr.version != AEROGPU_ESCAPE_VERSION || qe.hdr.op != AEROGPU_ESCAPE_OP_QUERY_ERROR ||
               qe.hdr.size != sizeof(qe)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("invalid QUERY_ERROR header (version=%lu op=%lu size=%lu)",
                           (unsigned long)qe.hdr.version,
                           (unsigned long)qe.hdr.op,
                           (unsigned long)qe.hdr.size);
    }
  }

  unsigned long long prev_submitted = 0;
  unsigned long long prev_completed = 0;
  bool have_prev = false;
  bool saw_any_nonzero = false;

  for (uint32_t i = 0; i < samples; ++i) {
    unsigned long long submitted = 0;
    unsigned long long completed = 0;
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryFence(&kmt, adapter, &submitted, &completed, &st)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: QUERY_FENCE escape not supported; skipping", kTestName);
        reporter.SetSkipped("not_supported");
        return reporter.Pass();
      }
      return reporter.Fail("D3DKMTEscape(query-fence) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }

    if (i == 0) {
      aerogpu_test::PrintfStdout("INFO: %s: samples=%lu interval_ms=%lu",
                                 kTestName,
                                 (unsigned long)samples,
                                 (unsigned long)interval_ms);
    }
    aerogpu_test::PrintfStdout("INFO: %s: [%lu] submitted=%I64u completed=%I64u",
                               kTestName,
                               (unsigned long)i,
                               submitted,
                               completed);

    if (submitted != 0 || completed != 0) {
      saw_any_nonzero = true;
    }

    if (completed > submitted) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("invalid fence state: completed > submitted (%I64u > %I64u)", completed, submitted);
    }

    if (have_prev) {
      if (submitted < prev_submitted) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("submitted fence is not monotonic (%I64u -> %I64u)", prev_submitted, submitted);
      }
      if (completed < prev_completed) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("completed fence is not monotonic (%I64u -> %I64u)", prev_completed, completed);
      }
    }

    prev_submitted = submitted;
    prev_completed = completed;
    have_prev = true;

    if (i + 1 < samples) {
      Sleep(interval_ms);
    }
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!saw_any_nonzero) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: fence counters remained 0 across all samples (no GPU submissions observed)", kTestName);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunFenceStateSanity(argc, argv);
}
