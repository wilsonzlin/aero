#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static const NTSTATUS kStatusInsufficientResources = (NTSTATUS)0xC000009AL;

static bool MapSharedHandleDebugToken(const D3DKMT_FUNCS* f,
                                      D3DKMT_HANDLE adapter,
                                      HANDLE shared_handle,
                                      uint32_t* out_token,
                                      NTSTATUS* out_status) {
  if (out_token) {
    *out_token = 0;
  }

  aerogpu_escape_map_shared_handle_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.shared_handle = (uint64_t)(uintptr_t)shared_handle;
  q.debug_token = 0;
  q.reserved0 = 0;

  if (!aerogpu_test::kmt::AerogpuEscape(f, adapter, &q, sizeof(q), out_status)) {
    return false;
  }

  if (out_token) {
    *out_token = q.debug_token;
  }
  return q.debug_token != 0;
}

static HANDLE CreateAnonymousSection(uint32_t size_bytes) {
  // Create an unnamed, pagefile-backed section handle.
  return CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, size_bytes, NULL);
}

static int RunMapSharedHandleStress(int argc, char** argv) {
  const char* kTestName = "map_shared_handle_stress";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--iters=N] [--unique=N] [--json[=PATH]]", kTestName);
    aerogpu_test::PrintfStdout("Calls AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE in a loop to sanity-check that the driver");
    aerogpu_test::PrintfStdout("returns a stable token for the same handle and remains responsive under many unique");
    aerogpu_test::PrintfStdout("section handles (cap/eviction behavior is accepted).");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  uint32_t iters = 10000;
  uint32_t unique = 4096;

  std::string iters_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--iters", &iters_str) && !iters_str.empty()) {
    std::string err;
    if (!aerogpu_test::ParseUint32(iters_str, &iters, &err) || iters == 0) {
      return reporter.Fail("invalid --iters: %s", err.c_str());
    }
  }
  std::string unique_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--unique", &unique_str) && !unique_str.empty()) {
    std::string err;
    if (!aerogpu_test::ParseUint32(unique_str, &unique, &err)) {
      return reporter.Fail("invalid --unique: %s", err.c_str());
    }
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

  // Stable handle/token sanity: MAP_SHARED_HANDLE should return a consistent token when the object is cached.
  HANDLE stable = CreateAnonymousSection(4096);
  if (!stable) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("CreateFileMappingW(stable) failed: %s",
                         aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  uint32_t stable_token = 0;
  NTSTATUS st = 0;
  if (!MapSharedHandleDebugToken(&kmt, adapter, stable, &stable_token, &st)) {
    CloseHandle(stable);
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);

    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }

    return reporter.Fail("MAP_SHARED_HANDLE(stable) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }
  if (stable_token == 0) {
    CloseHandle(stable);
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("MAP_SHARED_HANDLE(stable) returned debug_token=0");
  }

  uint32_t unique_attempted = 0;
  uint32_t unique_success = 0;
  uint32_t unique_failed = 0;
  bool stop_unique = false;

  for (uint32_t i = 0; i < iters; ++i) {
    uint32_t tok = 0;
    st = 0;
    if (!MapSharedHandleDebugToken(&kmt, adapter, stable, &tok, &st)) {
      CloseHandle(stable);
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("iter %lu: MAP_SHARED_HANDLE(stable) failed (NTSTATUS=0x%08lX)",
                           (unsigned long)i,
                           (unsigned long)st);
    }
    if (tok != stable_token) {
      CloseHandle(stable);
      aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
      aerogpu_test::kmt::UnloadD3DKMT(&kmt);
      return reporter.Fail("iter %lu: stable token mismatch: got=%lu expected=%lu",
                           (unsigned long)i,
                           (unsigned long)tok,
                           (unsigned long)stable_token);
    }

    // Unique-handle stress: create/close many section handles to try and exceed the driver's cache cap.
    if (!stop_unique && unique != 0 && i < unique) {
      HANDLE h = CreateAnonymousSection(4096);
      if (!h) {
        CloseHandle(stable);
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("CreateFileMappingW(unique) failed at iter %lu: %s",
                             (unsigned long)i,
                             aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      }

      unique_attempted++;
      uint32_t unique_tok = 0;
      st = 0;
      if (MapSharedHandleDebugToken(&kmt, adapter, h, &unique_tok, &st)) {
        unique_success++;
      } else {
        unique_failed++;
        // If the driver chose the "fail once cap is reached" strategy, accept STATUS_INSUFFICIENT_RESOURCES and stop
        // creating new handles. Still continue the stable mapping loop to ensure the driver remains responsive.
        if (st == kStatusInsufficientResources) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: unique MAP_SHARED_HANDLE hit cap at iter %lu (NTSTATUS=0x%08lX); continuing stable loop",
              kTestName,
              (unsigned long)i,
              (unsigned long)st);
          stop_unique = true;
        } else {
          CloseHandle(h);
          CloseHandle(stable);
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("iter %lu: MAP_SHARED_HANDLE(unique) failed (NTSTATUS=0x%08lX)",
                               (unsigned long)i,
                               (unsigned long)st);
        }
      }
      CloseHandle(h);
    }
  }

  CloseHandle(stable);
  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  aerogpu_test::PrintfStdout("INFO: %s: stable_token=%lu iters=%lu unique_attempted=%lu unique_success=%lu unique_failed=%lu",
                             kTestName,
                             (unsigned long)stable_token,
                             (unsigned long)iters,
                             (unsigned long)unique_attempted,
                             (unsigned long)unique_success,
                             (unsigned long)unique_failed);

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunMapSharedHandleStress(argc, argv);
}

