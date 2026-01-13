#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

namespace {

static const uint64_t kOneMiB = 1024ull * 1024ull;

struct SegmentGroupSize {
  uint64_t LocalMemorySize;
  uint64_t NonLocalMemorySize;
};

static bool IsOs64Bit() {
  // If this is a native 64-bit process OR a 32-bit process running under WOW64, the OS is x64.
  return aerogpu_test::Is64BitProcess() || aerogpu_test::IsRunningUnderWow64();
}

static uint64_t ClampMaxNonLocalBytesForOs() {
  return (IsOs64Bit() ? 2048ull : 1024ull) * kOneMiB;
}

static bool ProbeGetSegmentGroupSizeType(const D3DKMT_FUNCS* kmt,
                                        D3DKMT_HANDLE adapter,
                                        UINT* out_type,
                                        SegmentGroupSize* out_sizes,
                                        NTSTATUS* out_last_status) {
  if (out_type) {
    *out_type = 0xFFFFFFFFu;
  }
  if (out_sizes) {
    ZeroMemory(out_sizes, sizeof(*out_sizes));
  }
  if (out_last_status) {
    *out_last_status = 0;
  }

  if (!kmt || !kmt->QueryAdapterInfo || !adapter) {
    if (out_last_status) {
      *out_last_status = aerogpu_test::kmt::kStatusInvalidParameter;
    }
    return false;
  }

  // Avoid hard-coding the WDK's numeric KMTQAITYPE_GETSEGMENTGROUPSIZE constant; probe a small
  // range of values and look for a plausible 2xU64 layout.
  SegmentGroupSize sizes;
  NTSTATUS last_status = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&sizes, sizeof(sizes));

    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::D3DKMTQueryAdapterInfoWithTimeout(
            kmt, adapter, type, &sizes, (UINT)sizeof(sizes), 2000, &st)) {
      last_status = st;
      if (st == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
        break;
      }
      continue;
    }
    last_status = st;

    const uint64_t local = sizes.LocalMemorySize;
    const uint64_t nonlocal = sizes.NonLocalMemorySize;
    const uint64_t sum = local + nonlocal;

    if (sum == 0) {
      continue;
    }
    // Heuristic: segment sizes are typically multiples of MiB and not enormous.
    if ((local % kOneMiB) != 0 || (nonlocal % kOneMiB) != 0) {
      continue;
    }
    // Avoid mis-identifying unrelated query types with small integer payloads.
    if (sum < 16ull * kOneMiB) {
      continue;
    }
    // Guard against insane values (e.g. treating a pointer as a size).
    if (local > (1ull << 50) || nonlocal > (1ull << 50)) {
      continue;
    }

    if (out_type) {
      *out_type = type;
    }
    if (out_sizes) {
      *out_sizes = sizes;
    }
    if (out_last_status) {
      *out_last_status = st;
    }
    return true;
  }

  if (out_last_status) {
    *out_last_status = last_status;
  }
  return false;
}

static int RunSegmentBudgetSanity(int argc, char** argv) {
  const char* kTestName = "segment_budget_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--json[=PATH]] [--allow-remote] [--strict-default] [--min-nonlocal-mb=N]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Queries WDDM segment budget via D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE) and validates that the non-local "
        "segment size is sane. For AeroGPU, this budget is controlled by the registry value "
        "HKR\\Parameters\\NonLocalMemorySizeMB (default 512; clamped 128..1024 on x86, 128..2048 on x64).");
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

  uint32_t min_nonlocal_mb = 128;
  const bool strict_default = aerogpu_test::HasArg(argc, argv, "--strict-default");
  if (strict_default) {
    min_nonlocal_mb = 512;
  }
  std::string min_mb_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--min-nonlocal-mb", &min_mb_str)) {
    std::string err;
    uint32_t v = 0;
    if (!aerogpu_test::ParseUint32(min_mb_str, &v, &err)) {
      return reporter.Fail("invalid --min-nonlocal-mb: %s", err.c_str());
    }
    if (v < 128) {
      return reporter.Fail("--min-nonlocal-mb must be >= 128 (got %lu)", (unsigned long)v);
    }
    min_nonlocal_mb = v;
  }

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return reporter.Fail("%s", kmt_err.c_str());
  }
  if (!kmt.QueryAdapterInfo) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("D3DKMTQueryAdapterInfo not available (missing gdi32 export)");
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", open_err.c_str());
  }

  UINT seg_group_type = 0xFFFFFFFFu;
  SegmentGroupSize sizes;
  ZeroMemory(&sizes, sizeof(sizes));
  NTSTATUS last_status = 0;

  const bool have_sizes = ProbeGetSegmentGroupSizeType(&kmt, adapter, &seg_group_type, &sizes, &last_status);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!have_sizes || seg_group_type == 0xFFFFFFFFu) {
    if (last_status == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
      return reporter.Fail("D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE) timed out");
    }
    return reporter.Fail("failed to query GETSEGMENTGROUPSIZE (probe last NTSTATUS=0x%08lX)", (unsigned long)last_status);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: GETSEGMENTGROUPSIZE type=%lu local=%I64u MiB nonlocal=%I64u MiB (local=%I64u bytes nonlocal=%I64u bytes)",
      kTestName,
      (unsigned long)seg_group_type,
      (unsigned long long)(sizes.LocalMemorySize / kOneMiB),
      (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB),
      (unsigned long long)sizes.LocalMemorySize,
      (unsigned long long)sizes.NonLocalMemorySize);

  if (sizes.NonLocalMemorySize == 0) {
    return reporter.Fail("NonLocalMemorySize==0 (expected a nonzero system-memory-backed segment budget)");
  }

  const uint64_t min_nonlocal_bytes = (uint64_t)min_nonlocal_mb * kOneMiB;
  if (sizes.NonLocalMemorySize < min_nonlocal_bytes) {
    return reporter.Fail("NonLocalMemorySize too small: %I64u MiB < %lu MiB (use HKR\\\\Parameters\\\\NonLocalMemorySizeMB)",
                         (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB),
                         (unsigned long)min_nonlocal_mb);
  }

  // Default budget is 512MiB. Values below that can be intentional, but often lead to allocation failures under
  // real workloads. Always warn so the user notices.
  if (sizes.NonLocalMemorySize < 512ull * kOneMiB) {
    aerogpu_test::PrintfStdout(
        "WARN: %s: NonLocalMemorySize is below the default 512 MiB (%I64u MiB). "
        "D3D9/D3D11 workloads may fail allocations. Set HKR\\\\Parameters\\\\NonLocalMemorySizeMB to increase it "
        "(or pass --strict-default/--min-nonlocal-mb to enforce a minimum).",
        kTestName,
        (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB));
    if (strict_default && min_nonlocal_mb == 512) {
      // This path should already be caught by the min_nonlocal_mb check above, but keep the logic explicit.
      return reporter.Fail("NonLocalMemorySize below 512 MiB and --strict-default was supplied");
    }
  }

  const uint64_t max_expected = ClampMaxNonLocalBytesForOs();
  if (sizes.NonLocalMemorySize > max_expected) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: NonLocalMemorySize exceeds expected clamp for this OS (%s, max %I64u MiB): %I64u MiB. "
        "This may indicate the KMD clamp changed or is not being applied.",
        kTestName,
        IsOs64Bit() ? "x64" : "x86",
        (unsigned long long)(max_expected / kOneMiB),
        (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB));
  }

  return reporter.Pass();
}

}  // namespace

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSegmentBudgetSanity(argc, argv);
}

