#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunPerfStateSanity(int argc, char** argv) {
  const char* kTestName = "perf_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--samples=N] [--interval-ms=N] [--json[=PATH]] [--allow-remote]",
                               kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=5 --interval-ms=100");
    aerogpu_test::PrintfStdout("Queries the AeroGPU QUERY_PERF escape repeatedly and validates basic invariants.");
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

  uint32_t samples = 5;
  uint32_t interval_ms = 100;
  (void)aerogpu_test::GetArgUint32(argc, argv, "--samples", &samples);
  (void)aerogpu_test::GetArgUint32(argc, argv, "--interval-ms", &interval_ms);
  if (samples < 1) {
    samples = 1;
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

  bool saw_nonzero_fence = false;

  for (uint32_t i = 0; i < samples; ++i) {
    aerogpu_escape_query_perf_out q;
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryPerf(&kmt, adapter, &q, &st)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: QUERY_PERF escape not supported; skipping", kTestName);
        reporter.SetSkipped("not_supported");
        return reporter.Pass();
      }
      return reporter.Fail("D3DKMTEscape(query-perf) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }

    if (i == 0) {
      aerogpu_test::PrintfStdout("INFO: %s: samples=%lu interval_ms=%lu", kTestName, (unsigned long)samples,
                                 (unsigned long)interval_ms);
      aerogpu_test::PrintfStdout("INFO: %s: hdr.size=%lu (expected=%lu)", kTestName, (unsigned long)q.hdr.size,
                                 (unsigned long)sizeof(q));
    }

    if (q.hdr.version != AEROGPU_ESCAPE_VERSION || q.hdr.op != AEROGPU_ESCAPE_OP_QUERY_PERF) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("Invalid QUERY_PERF header (version=%lu op=%lu size=%lu)",
                           (unsigned long)q.hdr.version,
                           (unsigned long)q.hdr.op,
                           (unsigned long)q.hdr.size);
    }

    // Ensure the returned size covers the stable base portion of the struct.
    const uint32_t kMinSize = (uint32_t)(offsetof(aerogpu_escape_query_perf_out, reserved0) + sizeof(q.reserved0));
    if (q.hdr.size < kMinSize || q.hdr.size > sizeof(q)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("Unexpected QUERY_PERF size=%lu (min=%lu max=%lu)",
                           (unsigned long)q.hdr.size,
                           (unsigned long)kMinSize,
                           (unsigned long)sizeof(q));
    }

    if ((uint64_t)q.last_submitted_fence != 0 || (uint64_t)q.last_completed_fence != 0) {
      saw_nonzero_fence = true;
    }

    if (q.last_completed_fence > q.last_submitted_fence) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("Invalid fence state in QUERY_PERF: completed > submitted (%I64u > %I64u)",
                           (unsigned long long)q.last_completed_fence,
                           (unsigned long long)q.last_submitted_fence);
    }

    // Flags are appended; require a VALID bit when present.
    const bool have_flags =
        (q.hdr.size >= offsetof(aerogpu_escape_query_perf_out, flags) + sizeof(q.flags));
    if (!have_flags) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("QUERY_PERF did not include flags field (hdr.size=%lu)", (unsigned long)q.hdr.size);
    }
    if ((q.flags & AEROGPU_DBGCTL_QUERY_PERF_FLAGS_VALID) == 0) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("QUERY_PERF flags missing VALID bit (flags=0x%08lX)", (unsigned long)q.flags);
    }

    // If the ring snapshot is marked valid, check that the implied pending range is sane.
    //
    // Note: ring0 indices in QUERY_PERF are format-dependent:
    // - V1 ring ABI: `head`/`tail` are monotonically increasing u32 indices (not masked).
    // - Legacy ring registers: `head`/`tail` are masked indices in `[0, entry_count)`.
    if ((q.flags & AEROGPU_DBGCTL_QUERY_PERF_FLAG_RING_VALID) != 0 && q.ring0_entry_count != 0) {
      uint32_t pending = 0;
      // If either index is out of the masked range, assume monotonic semantics and compute the
      // pending count via wrapping u32 subtraction.
      if (q.ring0_head >= q.ring0_entry_count || q.ring0_tail >= q.ring0_entry_count) {
        pending = (uint32_t)(q.ring0_tail - q.ring0_head);
      } else if (q.ring0_tail >= q.ring0_head) {
        pending = q.ring0_tail - q.ring0_head;
      } else {
        pending = q.ring0_tail + q.ring0_entry_count - q.ring0_head;
      }
      if (pending > q.ring0_entry_count) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("Ring pending out of range (head=%lu tail=%lu pending=%lu entry_count=%lu)",
                             (unsigned long)q.ring0_head,
                             (unsigned long)q.ring0_tail,
                             (unsigned long)pending,
                             (unsigned long)q.ring0_entry_count);
      }
    }

    aerogpu_test::PrintfStdout(
        "INFO: %s: [%lu] fences(submitted=%I64u completed=%I64u) submits(total=%I64u presents=%I64u) irqs(fence=%I64u vblank=%I64u spurious=%I64u) flags=0x%08lX",
        kTestName,
        (unsigned long)i,
        (unsigned long long)q.last_submitted_fence,
        (unsigned long long)q.last_completed_fence,
        (unsigned long long)q.total_submissions,
        (unsigned long long)q.total_presents,
        (unsigned long long)q.irq_fence_delivered,
        (unsigned long long)q.irq_vblank_delivered,
        (unsigned long long)q.irq_spurious,
        (unsigned long)q.flags);

    if (i + 1 < samples) {
      Sleep(interval_ms);
    }
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!saw_nonzero_fence) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: fence counters remained 0 across all samples (no GPU submissions observed)", kTestName);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunPerfStateSanity(argc, argv);
}
