#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_ring.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static const char* RingFormatToString(uint32_t fmt) {
  switch (fmt) {
    case AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN:
      return "UNKNOWN";
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      return "LEGACY";
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      return "AGPU";
    default:
      break;
  }
  return "UNKNOWN";
}

static int RunRingStateSanity(int argc, char** argv) {
  const char* kTestName = "ring_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--ring-id=N] [--samples=N] [--interval-ms=N] [--json[=PATH]] [--allow-remote]",
        kTestName);
    aerogpu_test::PrintfStdout("Default: --ring-id=0 --samples=10 --interval-ms=100");
    aerogpu_test::PrintfStdout(
        "Dumps the KMD ring state via DUMP_RING_V2 and validates basic invariants and monotonicity.");
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

  uint32_t ring_id = 0;
  uint32_t samples = 10;
  uint32_t interval_ms = 100;
  (void)aerogpu_test::GetArgUint32(argc, argv, "--ring-id", &ring_id);
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

  aerogpu_escape_dump_ring_v2_inout prev;
  ZeroMemory(&prev, sizeof(prev));
  bool have_prev = false;

  for (uint32_t i = 0; i < samples; ++i) {
    aerogpu_escape_dump_ring_v2_inout dump;
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::AerogpuDumpRingV2(&kmt, adapter, ring_id, &dump, &st)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: DUMP_RING_V2 escape not supported; skipping", kTestName);
        reporter.SetSkipped("not_supported");
        return reporter.Pass();
      }
      return reporter.Fail("D3DKMTEscape(dump-ring-v2) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }

    if (i == 0) {
      aerogpu_test::PrintfStdout("INFO: %s: ring_id=%lu samples=%lu interval_ms=%lu",
                                 kTestName,
                                 (unsigned long)ring_id,
                                 (unsigned long)samples,
                                 (unsigned long)interval_ms);
    }

    aerogpu_test::PrintfStdout(
        "INFO: %s: [%lu] format=%s ring_size=%lu head=%lu tail=%lu desc_count=%lu",
        kTestName,
        (unsigned long)i,
        RingFormatToString((uint32_t)dump.ring_format),
        (unsigned long)dump.ring_size_bytes,
        (unsigned long)dump.head,
        (unsigned long)dump.tail,
        (unsigned long)dump.desc_count);

    if (dump.ring_size_bytes == 0) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("ring_size_bytes==0 (ring not initialized?)");
    }
    if (dump.desc_capacity == 0 || dump.desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("invalid desc_capacity=%lu", (unsigned long)dump.desc_capacity);
    }
    if (dump.desc_count > dump.desc_capacity) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("desc_count > desc_capacity (%lu > %lu)",
                           (unsigned long)dump.desc_count,
                           (unsigned long)dump.desc_capacity);
    }

    const bool is_agpu = (dump.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU);
    if (is_agpu) {
      if (dump.head > dump.tail) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("AGPU ring head > tail (%lu > %lu)", (unsigned long)dump.head, (unsigned long)dump.tail);
      }
      if (have_prev) {
        if (dump.head < prev.head) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("AGPU ring head is not monotonic (%lu -> %lu)",
                               (unsigned long)prev.head,
                               (unsigned long)dump.head);
        }
        if (dump.tail < prev.tail) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("AGPU ring tail is not monotonic (%lu -> %lu)",
                               (unsigned long)prev.tail,
                               (unsigned long)dump.tail);
        }
      }
    }

    for (uint32_t j = 0; j < dump.desc_count && j < dump.desc_capacity && j < AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS; ++j) {
      const aerogpu_dbgctl_ring_desc_v2& d = dump.desc[j];
      const bool cmd_present = (d.cmd_gpa != 0);
      const bool cmd_size_present = (d.cmd_size_bytes != 0);
      if (cmd_present != cmd_size_present) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("desc[%lu]: cmd_gpa/cmd_size mismatch (cmd_gpa=0x%I64X cmd_size=%lu)",
                             (unsigned long)j,
                             (unsigned long long)d.cmd_gpa,
                             (unsigned long)d.cmd_size_bytes);
      }

      if (is_agpu) {
        const bool alloc_table_present = (d.alloc_table_gpa != 0);
        const bool alloc_table_size_present = (d.alloc_table_size_bytes != 0);
        if (alloc_table_present != alloc_table_size_present) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail(
              "desc[%lu]: alloc_table_gpa/alloc_table_size mismatch (gpa=0x%I64X size=%lu)",
              (unsigned long)j,
              (unsigned long long)d.alloc_table_gpa,
              (unsigned long)d.alloc_table_size_bytes);
        }
        if (alloc_table_present && d.alloc_table_size_bytes < sizeof(struct aerogpu_alloc_table_header)) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("desc[%lu]: alloc_table_size_bytes too small (%lu < %lu)",
                               (unsigned long)j,
                               (unsigned long)d.alloc_table_size_bytes,
                               (unsigned long)sizeof(struct aerogpu_alloc_table_header));
        }
      }
    }

    prev = dump;
    have_prev = true;

    if (i + 1 < samples) {
      Sleep(interval_ms);
    }
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunRingStateSanity(argc, argv);
}

