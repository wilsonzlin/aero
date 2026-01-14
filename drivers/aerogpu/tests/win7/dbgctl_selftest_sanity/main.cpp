#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static const NTSTATUS kStatusTimeout = (NTSTATUS)0xC0000102L;

static const char* SelftestErrorToString(uint32_t code) {
  switch (code) {
    case AEROGPU_DBGCTL_SELFTEST_OK:
      return "OK";
    case AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE:
      return "INVALID_STATE";
    case AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY:
      return "RING_NOT_READY";
    case AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY:
      return "GPU_BUSY";
    case AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES:
      return "NO_RESOURCES";
    case AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT:
      return "TIMEOUT";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE:
      return "VBLANK_REGS_OUT_OF_RANGE";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK:
      return "VBLANK_SEQ_STUCK";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE:
      return "VBLANK_IRQ_REGS_OUT_OF_RANGE";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED:
      return "VBLANK_IRQ_NOT_LATCHED";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED:
      return "VBLANK_IRQ_NOT_CLEARED";
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE:
      return "CURSOR_REGS_OUT_OF_RANGE";
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH:
      return "CURSOR_RW_MISMATCH";
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED:
      return "VBLANK_IRQ_NOT_DELIVERED";
    case AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED:
      return "TIME_BUDGET_EXHAUSTED";
    default:
      break;
  }
  return "UNKNOWN";
}

static int RunDbgctlSelftestSanity(int argc, char** argv) {
  const char* kTestName = "dbgctl_selftest_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--timeout-ms=N] [--retries=N] [--retry-interval-ms=N] [--json[=PATH]]", kTestName);
    aerogpu_test::PrintfStdout("Default: --timeout-ms=5000 --retries=40 --retry-interval-ms=50");
    aerogpu_test::PrintfStdout("");
    aerogpu_test::PrintfStdout("Runs the KMD dbgctl selftest escape and checks for PASS.");
    aerogpu_test::PrintfStdout("If the adapter is busy (GPU_BUSY), retries for a short window and then skips.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  uint32_t timeout_ms = 5000;
  uint32_t retries = 40;
  uint32_t retry_interval_ms = 50;
  (void)aerogpu_test::GetArgUint32(argc, argv, "--timeout-ms", &timeout_ms);
  (void)aerogpu_test::GetArgUint32(argc, argv, "--retries", &retries);
  (void)aerogpu_test::GetArgUint32(argc, argv, "--retry-interval-ms", &retry_interval_ms);
  if (timeout_ms == 0) {
    timeout_ms = 5000;
  }
  if (timeout_ms > 30000) {
    timeout_ms = 30000;
  }
  if (retry_interval_ms == 0) {
    retry_interval_ms = 1;
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

  // Ensure the host-side escape wrapper timeout is at least as large as the selftest timeout,
  // with some slack for kernel/user transitions.
  DWORD escape_timeout_ms = (DWORD)timeout_ms + 2000u;
  if (escape_timeout_ms < 2000u) {
    escape_timeout_ms = 2000u;
  }
  if (escape_timeout_ms > 60000u) {
    escape_timeout_ms = 60000u;
  }

  for (uint32_t attempt = 0; attempt < retries; ++attempt) {
    aerogpu_escape_selftest_inout q;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.timeout_ms = timeout_ms;
    q.passed = 0;
    q.error_code = 0;
    q.reserved0 = 0;

    NTSTATUS st = 0;
    const bool ok = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &q, sizeof(q), escape_timeout_ms, &st);
    if (!ok) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);

      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: SELFTEST escape not supported; skipping", kTestName);
        reporter.SetSkipped("not_supported");
        return reporter.Pass();
      }
      if (st == kStatusTimeout) {
        return reporter.Fail("D3DKMTEscape(SELFTEST) timed out after %lu ms", (unsigned long)escape_timeout_ms);
      }
      return reporter.Fail("D3DKMTEscape(SELFTEST) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }

    aerogpu_test::PrintfStdout("INFO: %s: attempt=%lu/%lu passed=%lu error_code=%lu (%s)",
                               kTestName,
                               (unsigned long)(attempt + 1),
                               (unsigned long)retries,
                               (unsigned long)q.passed,
                               (unsigned long)q.error_code,
                               SelftestErrorToString((uint32_t)q.error_code));

    if (q.passed) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Pass();
    }

    if (q.error_code == AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY) {
      // Best-effort retry window: allow DWM/desktop activity to quiesce.
      if (attempt + 1 < retries) {
        Sleep(retry_interval_ms);
        continue;
      }
      aerogpu_test::PrintfStdout("INFO: %s: selftest returned GPU_BUSY after %lu attempt(s); skipping", kTestName, (unsigned long)retries);
      reporter.SetSkipped("gpu_busy");
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Pass();
    }

    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("selftest failed: error_code=%lu (%s)",
                         (unsigned long)q.error_code,
                         SelftestErrorToString((uint32_t)q.error_code));
  }

  // Should be unreachable (loop handles exit paths).
  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);
  reporter.SetSkipped("gpu_busy");
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunDbgctlSelftestSanity(argc, argv);
}

