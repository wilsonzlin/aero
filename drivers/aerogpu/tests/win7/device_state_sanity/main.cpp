#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_umd_private.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunDeviceStateSanity(int argc, char** argv) {
  const char* kTestName = "device_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Queries basic device/ABI state via the AeroGPU QUERY_DEVICE(_V2) escape and validates the response.");
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

  aerogpu_escape_query_device_v2_out q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.detected_mmio_magic = 0;
  q2.abi_version_u32 = 0;
  q2.features_lo = 0;
  q2.features_hi = 0;
  q2.reserved0 = 0;

  NTSTATUS st = 0;
  bool have_v2 = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &q2, sizeof(q2), 2000, &st);
  if (have_v2) {
    // Also sanity-check QUERY_ERROR doesn't hang. This is particularly important around
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

    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);

    aerogpu_test::PrintfStdout(
        "INFO: %s: magic=0x%08lX abi=0x%08lX features_lo=0x%I64X features_hi=0x%I64X",
        kTestName,
        (unsigned long)q2.detected_mmio_magic,
        (unsigned long)q2.abi_version_u32,
        (unsigned long long)q2.features_lo,
        (unsigned long long)q2.features_hi);

    if (q2.hdr.version != AEROGPU_ESCAPE_VERSION || q2.hdr.op != AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2 ||
        q2.hdr.size != sizeof(q2)) {
      return reporter.Fail("invalid QUERY_DEVICE_V2 header (version=%lu op=%lu size=%lu)",
                           (unsigned long)q2.hdr.version,
                           (unsigned long)q2.hdr.op,
                           (unsigned long)q2.hdr.size);
    }

    const uint32_t magic = (uint32_t)q2.detected_mmio_magic;
    if (magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      return reporter.Fail("unexpected MMIO magic: 0x%08lX", (unsigned long)magic);
    }
    if (q2.abi_version_u32 == 0) {
      return reporter.Fail("abi_version_u32==0");
    }
    return reporter.Pass();
  }

  // If QUERY_DEVICE_V2 isn't supported (older KMD), fall back to the legacy QUERY_DEVICE packet.
  if (st != aerogpu_test::kmt::kStatusNotSupported && st != aerogpu_test::kmt::kStatusInvalidParameter) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("D3DKMTEscape(query-device-v2) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  aerogpu_escape_query_device_out q1;
  ZeroMemory(&q1, sizeof(q1));
  q1.hdr.version = AEROGPU_ESCAPE_VERSION;
  q1.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
  q1.hdr.size = sizeof(q1);
  q1.hdr.reserved0 = 0;
  q1.mmio_version = 0;
  q1.reserved0 = 0;

  st = 0;
  bool have_v1 = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &q1, sizeof(q1), 2000, &st);

  // Best-effort: also call QUERY_ERROR for timeout/hang coverage on older KMDs that still support it.
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

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!have_v1) {
    return reporter.Fail("D3DKMTEscape(query-device) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  aerogpu_test::PrintfStdout("INFO: %s: legacy mmio_version=0x%08lX", kTestName, (unsigned long)q1.mmio_version);
  if (q1.mmio_version == 0) {
    return reporter.Fail("mmio_version==0");
  }
  if (q1.hdr.version != AEROGPU_ESCAPE_VERSION || q1.hdr.op != AEROGPU_ESCAPE_OP_QUERY_DEVICE || q1.hdr.size != sizeof(q1)) {
    return reporter.Fail("invalid QUERY_DEVICE header (version=%lu op=%lu size=%lu)",
                         (unsigned long)q1.hdr.version,
                         (unsigned long)q1.hdr.op,
                         (unsigned long)q1.hdr.size);
  }
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunDeviceStateSanity(argc, argv);
}
