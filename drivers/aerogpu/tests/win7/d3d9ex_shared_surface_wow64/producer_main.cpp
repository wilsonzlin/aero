#include "wow64_shared_surface_common.h"

#if defined(_WIN64) || defined(_M_X64)
#error This target must be built as x86 (the WOW64 producer process).
#endif

using namespace d3d9ex_shared_surface_wow64;

static int WaitForGpuIdle(aerogpu_test::TestReporter* reporter, const char* test_name, IDirect3DDevice9Ex* dev) {
  if (!dev) {
    if (reporter) {
      return reporter->Fail("internal: WaitForGpuIdle dev == NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: WaitForGpuIdle dev == NULL");
  }

  ComPtr<IDirect3DQuery9> q;
  HRESULT hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    if (reporter) {
      return reporter->FailHresult("CreateQuery(D3DQUERYTYPE_EVENT)", hr);
    }
    return aerogpu_test::FailHresult(test_name, "CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }
  hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DQuery9::Issue", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::Issue", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      return 0;
    }
    if (hr != S_FALSE) {
      if (reporter) {
        return reporter->FailHresult("IDirect3DQuery9::GetData", hr);
      }
      return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > 5000) {
      if (reporter) {
        return reporter->Fail("GPU event query timed out");
      }
      return aerogpu_test::Fail(test_name, "GPU event query timed out");
    }
    Sleep(0);
  }
}

static void AppendForwardedArgs(const AdapterRequirements& req, bool show, std::wstring* cmdline) {
  if (!cmdline) {
    return;
  }

  if (show) {
    *cmdline += L" --show";
  }
  if (req.allow_microsoft) {
    *cmdline += L" --allow-microsoft";
  }
  if (req.allow_non_aerogpu) {
    *cmdline += L" --allow-non-aerogpu";
  }
  if (req.require_umd) {
    *cmdline += L" --require-umd";
  }
  if (req.has_require_vid) {
    *cmdline += L" --require-vid=";
    *cmdline += std::wstring(req.require_vid_str.begin(), req.require_vid_str.end());
  }
  if (req.has_require_did) {
    *cmdline += L" --require-did=";
    *cmdline += std::wstring(req.require_did_str.begin(), req.require_did_str.end());
  }
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_wow64";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--show] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Note: this binary is 32-bit (WOW64 on Win7 x64) and spawns a 64-bit consumer process.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  // This test only makes sense on a 64-bit OS: the producer is x86 and the consumer is x64.
  if (!aerogpu_test::IsRunningUnderWow64()) {
    aerogpu_test::PrintfStdout("SKIP: %s: requires a 64-bit OS (WOW64)", kTestName);
    reporter.SetSkipped("requires a 64-bit OS (WOW64)");
    return reporter.Pass();
  }

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");
  if (dump) {
    reporter.AddArtifactPathW(
        aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_wow64.bmp"));
  }

  AdapterRequirements req;
  int rc = ParseAdapterRequirements(argc, argv, kTestName, &req, &reporter);
  if (rc != 0) {
    return rc;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceWOW64_Producer",
                                              L"AeroGPU D3D9Ex Shared Surface WOW64 (Producer x86)",
                                              kWidth,
                                              kHeight,
                                              show);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  rc = CreateD3D9ExDevice(kTestName, hwnd, &d3d, &dev, &reporter);
  if (rc != 0) {
    return rc;
  }
  rc = ValidateAdapter(kTestName, d3d.get(), req, &reporter);
  if (rc != 0) {
    return rc;
  }
  if (req.require_umd || (!req.allow_microsoft && !req.allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  HANDLE shared = NULL;
  ComPtr<IDirect3DTexture9> tex;
  HRESULT hr = dev->CreateTexture(kWidth,
                                  kHeight,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                   tex.put(),
                                   &shared);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture(shared)", hr);
  }
  if (!shared) {
    return reporter.Fail("CreateTexture returned NULL shared handle");
  }

  aerogpu_test::PrintfStdout("INFO: %s: producer shared handle=%s (%s%s)",
                             kTestName,
                             FormatHandleHex(shared).c_str(),
                             aerogpu_test::GetProcessBitnessString(),
                             aerogpu_test::GetWow64SuffixString());

  ComPtr<IDirect3DSurface9> rt;
  hr = tex->GetSurfaceLevel(0, rt.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
  }
  hr = dev->SetRenderTarget(0, rt.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderTarget(shared)", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kClearColor, 1.0f, 0);
  HRESULT hr_end = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear(shared)", hr);
  }
  if (FAILED(hr_end)) {
    return reporter.FailHresult("EndScene", hr_end);
  }

  rc = WaitForGpuIdle(&reporter, kTestName, dev.get());
  if (rc != 0) {
    return rc;
  }

  // Cross-bitness-safe IPC: a named file mapping holds the consumer's HANDLE value, and named events
  // coordinate access. Only names are passed on the command line.
  wchar_t map_name[128];
  wchar_t ready_name[128];
  wchar_t done_name[128];
  const DWORD pid = GetCurrentProcessId();
  const DWORD tick = GetTickCount();
  _snwprintf(map_name,
             ARRAYSIZE(map_name),
             L"AeroGPU_%lu_%lu_d3d9ex_shared_surface_wow64_map",
             (unsigned long)pid,
             (unsigned long)tick);
  map_name[ARRAYSIZE(map_name) - 1] = 0;
  _snwprintf(ready_name,
             ARRAYSIZE(ready_name),
             L"AeroGPU_%lu_%lu_d3d9ex_shared_surface_wow64_ready",
             (unsigned long)pid,
             (unsigned long)tick);
  ready_name[ARRAYSIZE(ready_name) - 1] = 0;
  _snwprintf(done_name,
             ARRAYSIZE(done_name),
             L"AeroGPU_%lu_%lu_d3d9ex_shared_surface_wow64_done",
             (unsigned long)pid,
             (unsigned long)tick);
  done_name[ARRAYSIZE(done_name) - 1] = 0;

  HANDLE mapping =
      CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, sizeof(Wow64Ipc), map_name);
  if (!mapping) {
    DWORD err = GetLastError();
    CloseHandle(shared);
    return reporter.Fail("CreateFileMapping failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  Wow64Ipc* ipc = (Wow64Ipc*)MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(Wow64Ipc));
  if (!ipc) {
    DWORD err = GetLastError();
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("MapViewOfFile failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }
  ZeroMemory(ipc, sizeof(*ipc));
  ipc->magic = kIpcMagic;
  ipc->version = kIpcVersion;

  HANDLE ready_event = CreateEventW(NULL, TRUE, FALSE, ready_name);
  HANDLE done_event = CreateEventW(NULL, TRUE, FALSE, done_name);
  if (!ready_event || !done_event) {
    DWORD err = GetLastError();
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("CreateEvent failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  const std::wstring consumer_path = aerogpu_test::JoinPath(
      aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_wow64_consumer_x64.exe");
  DWORD attrs = GetFileAttributesW(consumer_path.c_str());
  if (attrs == INVALID_FILE_ATTRIBUTES) {
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("missing consumer binary: %ls", consumer_path.c_str());
  }

  std::wstring cmdline = L"\"";
  cmdline += consumer_path;
  cmdline += L"\" --ipc-map=";
  cmdline += map_name;
  cmdline += L" --ready-event=";
  cmdline += ready_name;
  cmdline += L" --done-event=";
  cmdline += done_name;
  if (dump) {
    cmdline += L" --dump";
  }
  AppendForwardedArgs(req, show, &cmdline);

  std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
  cmdline_buf.push_back(0);

  STARTUPINFOW si;
  ZeroMemory(&si, sizeof(si));
  si.cb = sizeof(si);

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  BOOL ok = CreateProcessW(consumer_path.c_str(),
                           &cmdline_buf[0],
                           NULL,
                           NULL,
                           FALSE,
                           CREATE_SUSPENDED,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (!ok) {
    DWORD err = GetLastError();
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("CreateProcessW failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (job) {
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
    ZeroMemory(&info, sizeof(info));
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info))) {
      aerogpu_test::PrintfStdout("INFO: %s: SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: %s",
                                 kTestName,
                                 aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      CloseHandle(job);
      job = NULL;
    } else if (!AssignProcessToJobObject(job, pi.hProcess)) {
      aerogpu_test::PrintfStdout("INFO: %s: AssignProcessToJobObject failed: %s",
                                 kTestName,
                                 aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      CloseHandle(job);
      job = NULL;
    }
  }

  const uint64_t producer_hv = (uint64_t)(uintptr_t)shared;
  HANDLE shared_in_child = NULL;
  ok = DuplicateHandle(
      GetCurrentProcess(), shared, pi.hProcess, &shared_in_child, 0, FALSE, DUPLICATE_SAME_ACCESS);
  if (!ok || !shared_in_child) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("DuplicateHandle failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  uint64_t child_hv = (uint64_t)(uintptr_t)shared_in_child;
  aerogpu_test::PrintfStdout(
      "INFO: %s: duplicated shared handle: producer=%s -> consumer=%s",
      kTestName,
      FormatU64Hex(producer_hv).c_str(),
      FormatU64Hex(child_hv).c_str());

  if (child_hv == producer_hv) {
    // Extremely unlikely but possible: the consumer's handle table could allocate the same numeric
    // value. Duplicate again (without closing the first one first) to guarantee a different value.
    HANDLE shared_in_child2 = NULL;
    ok = DuplicateHandle(GetCurrentProcess(),
                         shared,
                         pi.hProcess,
                         &shared_in_child2,
                         0,
                         FALSE,
                         DUPLICATE_SAME_ACCESS);
    if (ok && shared_in_child2) {
      const uint64_t child_hv2 = (uint64_t)(uintptr_t)shared_in_child2;
      aerogpu_test::PrintfStdout(
          "INFO: %s: handle numeric collision; second duplicate: consumer=%s",
          kTestName,
          FormatU64Hex(child_hv2).c_str());

      // Close the first duplicated handle in the consumer (optional cleanup).
      HANDLE tmp = NULL;
      if (DuplicateHandle(pi.hProcess,
                          shared_in_child,
                          GetCurrentProcess(),
                          &tmp,
                          0,
                          FALSE,
                          DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE) &&
          tmp) {
        CloseHandle(tmp);
      }

      shared_in_child = shared_in_child2;
      child_hv = child_hv2;
    }
  }

  if (child_hv == producer_hv) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("refusing to run: shared handle value is numerically identical across processes after retry");
  }

  ipc->producer_handle_value = producer_hv;
  ipc->shared_handle_value = child_hv;
  InterlockedExchange(&ipc->ready, 1);
  SetEvent(ready_event);

  ResumeThread(pi.hThread);

  // Keep this comfortably below the suite's default per-test timeout (30s) so we can clean up the
  // consumer ourselves (and avoid leaving orphaned processes behind).
  const DWORD kChildTimeoutMs = 25000;
  const DWORD start_ticks = GetTickCount();

  HANDLE wait_handles[2] = {done_event, pi.hProcess};
  DWORD wait = WaitForMultipleObjects(2, wait_handles, FALSE, kChildTimeoutMs);
  if (wait == WAIT_TIMEOUT) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("consumer timed out");
  }
  if (wait == WAIT_FAILED) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("WaitForMultipleObjects failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  // Ensure the process has exited before we close the job object handle.
  DWORD wait_budget = RemainingTimeoutMs(start_ticks, kChildTimeoutMs);
  DWORD wait2 = WaitForSingleObject(pi.hProcess, wait_budget);
  if (wait2 != WAIT_OBJECT_0) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    CloseHandle(ready_event);
    CloseHandle(done_event);
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    CloseHandle(shared);
    return reporter.Fail("timeout waiting for consumer exit");
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    exit_code = 1;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (job) {
    CloseHandle(job);
  }
  CloseHandle(ready_event);
  CloseHandle(done_event);
  UnmapViewOfFile(ipc);
  CloseHandle(mapping);
  CloseHandle(shared);

  if (exit_code != 0) {
    return reporter.Fail("consumer failed with exit code %lu", (unsigned long)exit_code);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunProducer(argc, argv);
}
