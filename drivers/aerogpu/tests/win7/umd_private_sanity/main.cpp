#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_umd_private.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunUmdPrivateSanity(int argc, char** argv) {
  const char* kTestName = "umd_private_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Calls D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) and validates the returned aerogpu_umd_private_v1 blob.");
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

  aerogpu_umd_private_v1 blob;
  ZeroMemory(&blob, sizeof(blob));

  // Avoid depending on the WDK numeric KMTQAITYPE_UMDRIVERPRIVATE constant; probe a small range
  // and look for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  UINT found_type = 0xFFFFFFFFu;
  NTSTATUS last_status = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&blob, sizeof(blob));
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::D3DKMTQueryAdapterInfoWithTimeout(&kmt,
                                                              adapter,
                                                              type,
                                                              &blob,
                                                              sizeof(blob),
                                                              2000,
                                                              &st)) {
      last_status = st;
      if (st == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
        break;
      }
      continue;
    }
    last_status = st;

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = (uint32_t)blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    found_type = type;
    break;
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (found_type == 0xFFFFFFFFu) {
    if (last_status == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
      return reporter.Fail("D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) timed out");
    }
    return reporter.Fail("D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) probe failed (last NTSTATUS=0x%08lX)",
                         (unsigned long)last_status);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: type=%lu magic=0x%08lX abi=0x%08lX features=0x%I64X flags=0x%08lX",
      kTestName,
      (unsigned long)found_type,
      (unsigned long)blob.device_mmio_magic,
      (unsigned long)blob.device_abi_version_u32,
      (unsigned long long)blob.device_features,
      (unsigned long)blob.flags);

  if (blob.size_bytes < sizeof(blob)) {
    return reporter.Fail("blob.size_bytes too small (%lu < %lu)",
                         (unsigned long)blob.size_bytes,
                         (unsigned long)sizeof(blob));
  }
  if (blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
    return reporter.Fail("unexpected blob.struct_version=%lu", (unsigned long)blob.struct_version);
  }

  if (blob.device_mmio_magic == 0) {
    return reporter.Fail("device_mmio_magic==0 (expected AeroGPU MMIO magic)");
  }

  // Basic consistency check: legacy devices should set IS_LEGACY; new devices should not.
  if (blob.device_mmio_magic == AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP) {
    if ((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) == 0) {
      return reporter.Fail("expected AEROGPU_UMDPRIV_FLAG_IS_LEGACY for legacy device magic");
    }
  }
  if (blob.device_mmio_magic == AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
    if ((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) != 0) {
      return reporter.Fail("unexpected AEROGPU_UMDPRIV_FLAG_IS_LEGACY for new device magic");
    }
  }

  if ((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0 &&
      (blob.device_features & AEROGPU_UMDPRIV_FEATURE_VBLANK) == 0) {
    return reporter.Fail("HAS_VBLANK set but device_features is missing VBLANK bit");
  }
  if ((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE) != 0 &&
      (blob.device_features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) == 0) {
    return reporter.Fail("HAS_FENCE_PAGE set but device_features is missing FENCE_PAGE bit");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunUmdPrivateSanity(argc, argv);
}
