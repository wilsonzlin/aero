#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int RunScanoutStateSanity(int argc, char** argv) {
  const char* kTestName = "scanout_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Queries AeroGPU scanout state via a driver-private escape and validates it matches the desktop mode.");
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

  const int screen_width = GetSystemMetrics(SM_CXSCREEN);
  const int screen_height = GetSystemMetrics(SM_CYSCREEN);

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

  aerogpu_escape_query_scanout_out_v2 q;
  ZeroMemory(&q, sizeof(q));
  q.base.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.base.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.base.hdr.size = sizeof(q);
  q.base.hdr.reserved0 = 0;
  q.base.vidpn_source_id = 0;

  NTSTATUS st = 0;
  const bool ok = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &q, sizeof(q), 2000, &st);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!ok) {
    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: QUERY_SCANOUT escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("D3DKMTEscape(query-scanout) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  aerogpu_test::PrintfStdout("INFO: %s: screen=%dx%d", kTestName, screen_width, screen_height);
  aerogpu_test::PrintfStdout("INFO: %s: QUERY_SCANOUT hdr.size=%lu", kTestName, (unsigned long)q.base.hdr.size);
  aerogpu_test::PrintfStdout(
      "INFO: %s: cached: enable=%lu width=%lu height=%lu format=%lu pitch=%lu",
      kTestName,
      (unsigned long)q.base.cached_enable,
      (unsigned long)q.base.cached_width,
      (unsigned long)q.base.cached_height,
      (unsigned long)q.base.cached_format,
      (unsigned long)q.base.cached_pitch_bytes);
  aerogpu_test::PrintfStdout(
      "INFO: %s: mmio:   enable=%lu width=%lu height=%lu format=%lu pitch=%lu fb_gpa=0x%I64X",
      kTestName,
      (unsigned long)q.base.mmio_enable,
      (unsigned long)q.base.mmio_width,
      (unsigned long)q.base.mmio_height,
      (unsigned long)q.base.mmio_format,
      (unsigned long)q.base.mmio_pitch_bytes,
      (unsigned long long)q.base.mmio_fb_gpa);

  if (q.base.hdr.size >= sizeof(aerogpu_escape_query_scanout_out_v2)) {
    aerogpu_test::PrintfStdout("INFO: %s: cached_fb_gpa=0x%I64X flags=0x%08lX",
                               kTestName,
                               (unsigned long long)q.cached_fb_gpa,
                               (unsigned long)q.base.reserved0);
  }

  if (q.base.cached_enable == 0) {
    return reporter.Fail("cached_enable==0 (expected scanout enabled)");
  }
  if (q.base.mmio_enable == 0) {
    return reporter.Fail("mmio_enable==0 (expected scanout enabled)");
  }
  if (q.base.mmio_fb_gpa == 0) {
    return reporter.Fail("mmio_fb_gpa==0 (expected framebuffer address programmed)");
  }
  if (q.base.hdr.size < sizeof(aerogpu_escape_query_scanout_out_v2)) {
    return reporter.Fail("QUERY_SCANOUT did not return v2 (hdr.size=%lu expected >=%I64u)",
                         (unsigned long)q.base.hdr.size,
                         (unsigned long long)sizeof(aerogpu_escape_query_scanout_out_v2));
  }

  if (q.base.cached_width == 0 || q.base.cached_height == 0) {
    return reporter.Fail("cached_width/height are zero");
  }
  if (q.base.mmio_width == 0 || q.base.mmio_height == 0) {
    return reporter.Fail("mmio_width/height are zero");
  }

  if (q.base.cached_width != q.base.mmio_width || q.base.cached_height != q.base.mmio_height) {
    return reporter.Fail("cached mode does not match MMIO scanout regs");
  }
  if (q.base.cached_pitch_bytes == 0 || q.base.mmio_pitch_bytes == 0) {
    return reporter.Fail("pitch is zero");
  }
  if (q.base.cached_pitch_bytes != q.base.mmio_pitch_bytes) {
    return reporter.Fail("cached pitch does not match MMIO pitch (%lu vs %lu)",
                         (unsigned long)q.base.cached_pitch_bytes,
                         (unsigned long)q.base.mmio_pitch_bytes);
  }

  if (screen_width > 0 && screen_height > 0) {
    if ((unsigned long)screen_width != (unsigned long)q.base.cached_width ||
        (unsigned long)screen_height != (unsigned long)q.base.cached_height) {
      return reporter.Fail("cached mode does not match desktop resolution (%dx%d)", screen_width, screen_height);
    }
  }

  const unsigned long long row_bytes = (unsigned long long)q.base.cached_width * 4ull;
  if ((unsigned long long)q.base.cached_pitch_bytes < row_bytes) {
    return reporter.Fail("pitch too small for width: pitch=%lu width=%lu row_bytes=%I64u",
                         (unsigned long)q.base.cached_pitch_bytes,
                         (unsigned long)q.base.cached_width,
                         row_bytes);
  }

  // Newer KMDs may return a v2 QUERY_SCANOUT packet with cached framebuffer GPA.
  // If present and scanout is enabled, it must be non-zero.
  if (q.base.hdr.size >= sizeof(aerogpu_escape_query_scanout_out_v2)) {
    const bool flagsValid = (q.base.reserved0 & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID) != 0;
    if (flagsValid) {
      const bool cachedFbGpaValid = (q.base.reserved0 & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID) != 0;
      if (cachedFbGpaValid && q.cached_fb_gpa == 0) {
        return reporter.Fail("cached_fb_gpa is marked valid but is 0");
      }
      if (!cachedFbGpaValid) {
        return reporter.Fail("cached_fb_gpa not marked valid (flags=0x%08lX)", (unsigned long)q.base.reserved0);
      }
    }

    if (q.cached_fb_gpa == 0) {
      return reporter.Fail("cached_fb_gpa==0 (expected framebuffer address when scanout enabled)");
    }
  }

  // Modeset validation sanity: attempt to test an obviously unsupported mode and ensure
  // Windows rejects it cleanly (should not be reported as supported by the driver).
  {
    DEVMODEW dm;
    ZeroMemory(&dm, sizeof(dm));
    dm.dmSize = sizeof(dm);
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
    dm.dmPelsWidth = 1234;
    dm.dmPelsHeight = 777;

    const LONG r = ChangeDisplaySettingsExW(NULL, &dm, NULL, CDS_TEST, NULL);
    aerogpu_test::PrintfStdout(
        "INFO: %s: ChangeDisplaySettingsExW(CDS_TEST) %lux%lu -> %ld",
        kTestName,
        (unsigned long)dm.dmPelsWidth,
        (unsigned long)dm.dmPelsHeight,
        (long)r);
    if (r == DISP_CHANGE_SUCCESSFUL) {
      return reporter.Fail("unsupported mode %lux%lu unexpectedly reported as supported",
                           (unsigned long)dm.dmPelsWidth,
                           (unsigned long)dm.dmPelsHeight);
    }
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunScanoutStateSanity(argc, argv);
}
