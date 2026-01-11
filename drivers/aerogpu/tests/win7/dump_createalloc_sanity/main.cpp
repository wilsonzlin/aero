#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunDumpCreateallocSanity(int argc, char** argv) {
  const char* kTestName = "dump_createalloc_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Dumps the KMD CreateAllocation trace via a driver-private escape and validates it is non-empty and sane.");
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

  aerogpu_escape_dump_createallocation_inout dump;
  NTSTATUS st = 0;
  const bool ok = aerogpu_test::kmt::AerogpuDumpCreateAllocationTrace(&kmt, adapter, &dump, &st);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!ok) {
    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: DUMP_CREATEALLOCATION escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("D3DKMTEscape(dump-createalloc) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  aerogpu_test::PrintfStdout("INFO: %s: write_index=%lu entry_count=%lu",
                             kTestName,
                             (unsigned long)dump.write_index,
                             (unsigned long)dump.entry_count);

  if (dump.entry_count == 0) {
    return reporter.Fail("CreateAllocation trace is empty (entry_count==0)");
  }
  if (dump.entry_count > dump.entry_capacity || dump.entry_capacity == 0) {
    return reporter.Fail("invalid CreateAllocation trace counts: entry_count=%lu entry_capacity=%lu",
                         (unsigned long)dump.entry_count,
                         (unsigned long)dump.entry_capacity);
  }
  if (dump.write_index < dump.entry_count) {
    return reporter.Fail("write_index < entry_count (%lu < %lu)",
                         (unsigned long)dump.write_index,
                         (unsigned long)dump.entry_count);
  }

  uint32_t prev_seq = 0;
  for (uint32_t i = 0; i < dump.entry_count && i < dump.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
       ++i) {
    const aerogpu_dbgctl_createallocation_desc& e = dump.entries[i];
    if (e.alloc_id == 0) {
      return reporter.Fail("trace entry[%lu]: alloc_id==0", (unsigned long)i);
    }
    if (e.num_allocations == 0) {
      return reporter.Fail("trace entry[%lu]: num_allocations==0", (unsigned long)i);
    }
    if (e.alloc_index >= e.num_allocations) {
      return reporter.Fail("trace entry[%lu]: alloc_index out of range (%lu/%lu)",
                           (unsigned long)i,
                           (unsigned long)e.alloc_index,
                           (unsigned long)e.num_allocations);
    }

    if (i > 0 && e.seq <= prev_seq) {
      return reporter.Fail("trace entry[%lu]: seq not increasing (%lu -> %lu)",
                           (unsigned long)i,
                           (unsigned long)prev_seq,
                           (unsigned long)e.seq);
    }
    prev_seq = e.seq;
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunDumpCreateallocSanity(argc, argv);
}
