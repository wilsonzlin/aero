#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include <cmath>
#include <vector>

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunVblankStateSanity(int argc, char** argv) {
  const char* kTestName = "vblank_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--samples=N] [--interval-ms=N] [--json[=PATH]] [--allow-remote]",
        kTestName);
    aerogpu_test::PrintfStdout("Default: --samples=10 --interval-ms=100");
    aerogpu_test::PrintfStdout("Aliases: --vblank-samples, --vblank-interval-ms");
    aerogpu_test::PrintfStdout(
        "Queries vblank counters via a driver-private escape and validates basic monotonicity/pacing.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  uint32_t samples = 10;
  uint32_t interval_ms = 100;

  const char* samples_key = "--samples";
  std::string samples_str;
  bool got_samples = aerogpu_test::GetArgValue(argc, argv, samples_key, &samples_str);
  if (!got_samples) {
    samples_key = "--vblank-samples";
    got_samples = aerogpu_test::GetArgValue(argc, argv, samples_key, &samples_str);
  }
  if (got_samples) {
    if (samples_str.empty()) {
      return reporter.Fail("%s missing value", samples_key);
    }
    std::string err;
    if (!aerogpu_test::ParseUint32(samples_str, &samples, &err)) {
      return reporter.Fail("invalid %s: %s", samples_key, err.c_str());
    }
  }

  const char* interval_key = "--interval-ms";
  std::string interval_str;
  bool got_interval = aerogpu_test::GetArgValue(argc, argv, interval_key, &interval_str);
  if (!got_interval) {
    interval_key = "--vblank-interval-ms";
    got_interval = aerogpu_test::GetArgValue(argc, argv, interval_key, &interval_str);
  }
  if (got_interval) {
    if (interval_str.empty()) {
      return reporter.Fail("%s missing value", interval_key);
    }
    std::string err;
    if (!aerogpu_test::ParseUint32(interval_str, &interval_ms, &err)) {
      return reporter.Fail("invalid %s: %s", interval_key, err.c_str());
    }
  }
  if (samples < 2) {
    samples = 2;
  }
  if (interval_ms < 1) {
    interval_ms = 1;
  }

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

  std::vector<aerogpu_escape_query_vblank_out> snaps;
  snaps.reserve(samples);

  for (uint32_t i = 0; i < samples; ++i) {
    aerogpu_escape_query_vblank_out q;
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryVblank(&kmt, adapter, 0, &q, &st)) {
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: QUERY_VBLANK escape not supported; skipping", kTestName);
        reporter.SetSkipped("not_supported");
        return reporter.Pass();
      }
      return reporter.Fail("D3DKMTEscape(query-vblank) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }

    if (i == 0) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: flags=0x%08lX period_ns=%lu irq_enable=0x%08lX irq_status=0x%08lX",
          kTestName,
          (unsigned long)q.flags,
          (unsigned long)q.vblank_period_ns,
          (unsigned long)q.irq_enable,
          (unsigned long)q.irq_status);
    }

    snaps.push_back(q);
    if (i + 1 < samples) {
      Sleep(interval_ms);
    }
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (snaps.empty()) {
    return reporter.Fail("no vblank samples collected");
  }

  std::vector<double> vblank_period_samples_ms;
  if (snaps.size() > 1) {
    vblank_period_samples_ms.reserve(snaps.size() - 1);
    for (size_t i = 1; i < snaps.size(); ++i) {
      const unsigned long long dseq =
          (unsigned long long)snaps[i].vblank_seq - (unsigned long long)snaps[i - 1].vblank_seq;
      const unsigned long long dt_ns =
          (unsigned long long)snaps[i].last_vblank_time_ns - (unsigned long long)snaps[i - 1].last_vblank_time_ns;
      if (dseq == 0 || dt_ns == 0) {
        continue;
      }
      vblank_period_samples_ms.push_back(((double)dt_ns / (double)dseq) / 1000000.0);
    }
  }
  if (!vblank_period_samples_ms.empty()) {
    reporter.SetTimingSamplesMs(vblank_period_samples_ms);
  }

  const aerogpu_escape_query_vblank_out& first = snaps.front();
  const aerogpu_escape_query_vblank_out& last = snaps.back();

  if ((first.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) == 0) {
    return reporter.Fail("QUERY_VBLANK returned flags without VALID bit set (flags=0x%08lX)",
                         (unsigned long)first.flags);
  }
  if ((first.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) == 0) {
    return reporter.Fail("QUERY_VBLANK reports vblank not supported (flags=0x%08lX)",
                         (unsigned long)first.flags);
  }
  if (first.vblank_period_ns == 0) {
    return reporter.Fail("vblank_period_ns==0");
  }
  if (first.vblank_period_ns < 1000000u || first.vblank_period_ns > 1000000000u) {
    return reporter.Fail("vblank_period_ns out of expected range: %lu", (unsigned long)first.vblank_period_ns);
  }

  bool monotonic_seq = true;
  bool monotonic_time = true;
  for (size_t i = 1; i < snaps.size(); ++i) {
    if (snaps[i].vblank_seq < snaps[i - 1].vblank_seq) {
      monotonic_seq = false;
    }
    if (snaps[i].last_vblank_time_ns < snaps[i - 1].last_vblank_time_ns) {
      monotonic_time = false;
    }
  }
  if (!monotonic_seq) {
    return reporter.Fail("vblank_seq is not monotonic");
  }
  if (!monotonic_time) {
    return reporter.Fail("last_vblank_time_ns is not monotonic");
  }

  const unsigned long long seq0 = (unsigned long long)first.vblank_seq;
  const unsigned long long seq1 = (unsigned long long)last.vblank_seq;
  if (seq1 <= seq0) {
    return reporter.Fail("vblank_seq did not advance (%I64u -> %I64u)", seq0, seq1);
  }

  const unsigned long long t0 = (unsigned long long)first.last_vblank_time_ns;
  const unsigned long long t1 = (unsigned long long)last.last_vblank_time_ns;
  if (t1 <= t0) {
    return reporter.Fail("last_vblank_time_ns did not advance (%I64u -> %I64u)", t0, t1);
  }

  const unsigned long long dseq = seq1 - seq0;
  const unsigned long long dt_ns = t1 - t0;
  const double estimated_period_ns = (double)dt_ns / (double)dseq;
  const double reported_period_ns = (double)first.vblank_period_ns;
  const double rel_err = fabs(estimated_period_ns - reported_period_ns) / reported_period_ns;

  aerogpu_test::PrintfStdout(
      "INFO: %s: seq_delta=%I64u dt_ns=%I64u estimated_period_ns=%.1f reported_period_ns=%.1f rel_err=%.3f",
      kTestName,
      dseq,
      dt_ns,
      estimated_period_ns,
      reported_period_ns,
      rel_err);

  if (estimated_period_ns < 2000000.0 || estimated_period_ns > 250000000.0) {
    return reporter.Fail("estimated vblank period out of range: %.1f ns", estimated_period_ns);
  }

  // Keep the tolerance wide: the virtual vblank clock may have jitter, but it should be broadly
  // consistent with the advertised period.
  if (rel_err > 0.25) {
    return reporter.Fail("vblank period mismatch: estimated=%.1f ns reported=%lu ns (rel_err=%.3f)",
                         estimated_period_ns,
                         (unsigned long)first.vblank_period_ns,
                         rel_err);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunVblankStateSanity(argc, argv);
}
