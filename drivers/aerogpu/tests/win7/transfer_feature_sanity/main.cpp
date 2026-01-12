#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_pci.h"
#include "..\\..\\..\\protocol\\aerogpu_umd_private.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunTransferFeatureSanity(int argc, char** argv) {
  const char* kTestName = "transfer_feature_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote] [--require-agpu]", kTestName);
    aerogpu_test::PrintfStdout(
        "Calls D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) and validates that the AeroGPU discovery blob advertises "
        "AEROGPU_UMDPRIV_FEATURE_TRANSFER when running on an AGPU ABI that should support transfer/copy "
        "(ABI major==AEROGPU_ABI_MAJOR and minor>=1).");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  const bool require_agpu = aerogpu_test::HasArg(argc, argv, "--require-agpu");

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
    return reporter.Fail("blob.size_bytes too small (%lu < %lu)", (unsigned long)blob.size_bytes, (unsigned long)sizeof(blob));
  }
  if (blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
    return reporter.Fail("unexpected blob.struct_version=%lu", (unsigned long)blob.struct_version);
  }
  if (blob.device_mmio_magic == 0) {
    return reporter.Fail("device_mmio_magic==0 (expected AeroGPU MMIO magic)");
  }

  if (blob.device_mmio_magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
    if (require_agpu) {
      return reporter.Fail(
          "expected AGPU device model (magic=0x%08lX), but got magic=0x%08lX. "
          "Ensure you're running the new AeroGPU device model and installed the non-legacy Win7 driver package.",
          (unsigned long)AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU,
          (unsigned long)blob.device_mmio_magic);
    }
    aerogpu_test::PrintfStdout(
        "INFO: %s: legacy/non-AGPU device magic detected; skipping (pass --require-agpu to fail)",
        kTestName);
    reporter.SetSkipped("not_agpu");
    return reporter.Pass();
  }

  const uint32_t abi_major = (uint32_t)blob.device_abi_version_u32 >> 16;
  const uint32_t abi_minor = (uint32_t)blob.device_abi_version_u32 & 0xFFFFu;

  if (abi_major != AEROGPU_ABI_MAJOR) {
    if (require_agpu) {
      return reporter.Fail(
          "AGPU ABI major mismatch: device reports major=%lu (abi=0x%08lX), but this build expects major=%lu. "
          "Ensure the guest driver and emulator/device model are from matching revisions.",
          (unsigned long)abi_major,
          (unsigned long)blob.device_abi_version_u32,
          (unsigned long)AEROGPU_ABI_MAJOR);
    }
    aerogpu_test::PrintfStdout(
        "INFO: %s: AGPU ABI major mismatch (device=%lu expected=%lu); skipping (pass --require-agpu to fail)",
        kTestName,
        (unsigned long)abi_major,
        (unsigned long)AEROGPU_ABI_MAJOR);
    reporter.SetSkipped("abi_major_mismatch");
    return reporter.Pass();
  }

  // Transfer/copy support is defined for ABI 1.1+ (minor >= 1).
  if (abi_minor < 1) {
    return reporter.Fail(
        "AGPU ABI too old for transfer/copy: abi=0x%08lX (major=%lu minor=%lu). "
        "D3D9/D3D11 readback/copy requires ABI minor>=1 + AEROGPU_UMDPRIV_FEATURE_TRANSFER. "
        "Update the emulator/device model and ensure the installed AeroGPU driver stack matches.",
        (unsigned long)blob.device_abi_version_u32,
        (unsigned long)abi_major,
        (unsigned long)abi_minor);
  }

  if ((blob.device_features & AEROGPU_UMDPRIV_FEATURE_TRANSFER) == 0) {
    return reporter.Fail(
        "AEROGPU_UMDPRIV_FEATURE_TRANSFER is missing (device_features=0x%I64X, abi=0x%08lX major=%lu minor=%lu). "
        "This will break D3D9/D3D11 GPU->CPU readback/copy paths. "
        "Ensure you're using an AGPU device model build that supports transfer/copy and that the KMD advertises the "
        "feature bit via DXGKQAITYPE_UMDRIVERPRIVATE.",
        (unsigned long long)blob.device_features,
        (unsigned long)blob.device_abi_version_u32,
        (unsigned long)abi_major,
        (unsigned long)abi_minor);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunTransferFeatureSanity(argc, argv);
}

