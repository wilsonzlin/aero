#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_ring.h"
#include "..\\..\\..\\protocol\\aerogpu_cmd.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;
using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int WaitForGpuEventQuery(const char* test_name, IDirect3DDevice9Ex* dev, IDirect3DQuery9* q, DWORD timeout_ms) {
  if (!dev || !q) {
    return aerogpu_test::Fail(test_name, "WaitForGpuEventQuery called with NULL");
  }

  HRESULT hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::Issue", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      break;
    }
    if (hr != S_FALSE) {
      return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > timeout_ms) {
      return aerogpu_test::Fail(test_name, "GPU event query timed out");
    }
    Sleep(0);
  }

  return 0;
}

static bool CmdStreamHasWritebackCopy(const unsigned char* data, uint32_t bytes) {
  if (!data || bytes < sizeof(struct aerogpu_cmd_stream_header)) {
    return false;
  }

  aerogpu_cmd_stream_header sh;
  ZeroMemory(&sh, sizeof(sh));
  memcpy(&sh, data, sizeof(sh));
  if (sh.magic != AEROGPU_CMD_STREAM_MAGIC) {
    return false;
  }
  if (sh.size_bytes < sizeof(struct aerogpu_cmd_stream_header)) {
    return false;
  }

  uint32_t offset = sizeof(struct aerogpu_cmd_stream_header);
  uint32_t stream_size = sh.size_bytes;
  // READ_GPA is bounded; tolerate truncated streams when scanning for COPY_* WRITEBACK_DST.
  if (stream_size > bytes) {
    stream_size = bytes;
  }
  while (offset + sizeof(struct aerogpu_cmd_hdr) <= stream_size) {
    aerogpu_cmd_hdr hdr;
    ZeroMemory(&hdr, sizeof(hdr));
    memcpy(&hdr, data + offset, sizeof(hdr));
    if (hdr.size_bytes < sizeof(struct aerogpu_cmd_hdr) || (hdr.size_bytes & 3u) != 0) {
      return false;
    }
    const uint32_t end = offset + hdr.size_bytes;
    if (end > stream_size) {
      // Truncated packet; stop scanning.
      break;
    }

    if (hdr.opcode == AEROGPU_CMD_COPY_BUFFER) {
      if (hdr.size_bytes >= sizeof(struct aerogpu_cmd_copy_buffer)) {
        aerogpu_cmd_copy_buffer cmd;
        memcpy(&cmd, data + offset, sizeof(cmd));
        if ((cmd.flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0) {
          return true;
        }
      }
    } else if (hdr.opcode == AEROGPU_CMD_COPY_TEXTURE2D) {
      if (hdr.size_bytes >= sizeof(struct aerogpu_cmd_copy_texture2d)) {
        aerogpu_cmd_copy_texture2d cmd;
        memcpy(&cmd, data + offset, sizeof(cmd));
        if ((cmd.flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0) {
          return true;
        }
      }
    }

    offset = end;
  }

  return false;
}

static int RunAllocTableReadonlySanity(int argc, char** argv) {
  const char* kTestName = "alloc_table_readonly_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--json[=PATH]] [--allow-remote] [--require-agpu]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Triggers a D3D9Ex GPU->CPU readback (GetRenderTargetData) which uses an AeroGPU COPY_TEXTURE2D "
        "WRITEBACK_DST submission when transfer is supported. Reads back the per-submit allocation table via "
        "dbgctl READ_GPA and validates that it contains a mix of READONLY and writable entries, verifying "
        "propagation of WDDM DXGK_ALLOCATIONLIST WriteOperation semantics into alloc_table.flags.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
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

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_AllocTableReadonlySanity",
                                              L"AeroGPU alloc table readonly sanity",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("Direct3DCreate9Ex", hr);
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = kWidth;
  pp.BackBufferHeight = kHeight;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                           D3DDEVTYPE_HAL,
                           hwnd,
                           D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES,
                           &pp,
                           NULL,
                           dev.put());
  if (FAILED(hr)) {
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES,
                             &pp,
                             NULL,
                             dev.put());
  }
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  // Open the adapter via KMT so we can dump the ring and read alloc tables.
  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return reporter.Fail("%s", kmt_err.c_str());
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenAdapterFromHwnd(&kmt, hwnd, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", open_err.c_str());
  }

  aerogpu_escape_dump_ring_v2_inout before;
  NTSTATUS st = 0;
  if (!aerogpu_test::kmt::AerogpuDumpRingV2(&kmt, adapter, /*ring_id=*/0, &before, &st)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: DUMP_RING_V2 escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("D3DKMTEscape(dump-ring-v2) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  const bool is_agpu = (before.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU);
  if (!is_agpu) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (require_agpu) {
      return reporter.Fail("expected AGPU ring format, got %lu", (unsigned long)before.ring_format);
    }
    aerogpu_test::PrintfStdout("INFO: %s: not running on AGPU ring; skipping", kTestName);
    reporter.SetSkipped("not_agpu");
    return reporter.Pass();
  }

  const uint32_t tail_before = (uint32_t)before.tail;

  // Trigger a GPU->CPU readback (GetRenderTargetData) which should emit a COPY_TEXTURE2D WRITEBACK_DST submission
  // when transfer is supported.
  ComPtr<IDirect3DSurface9> rt;
  hr = dev->CreateRenderTarget(kWidth,
                               kHeight,
                               D3DFMT_A8R8G8B8,
                               D3DMULTISAMPLE_NONE,
                               0,
                               FALSE,
                               rt.put(),
                               NULL);
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("CreateRenderTarget", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(kWidth,
                                        kHeight,
                                        D3DFMT_A8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("CreateQuery(D3DQUERYTYPE_EVENT)", FAILED(hr) ? hr : E_FAIL);
  }

  ComPtr<IDirect3DSurface9> prev_rt;
  hr = dev->GetRenderTarget(0, prev_rt.put());
  if (FAILED(hr) || !prev_rt) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("GetRenderTarget(0)", FAILED(hr) ? hr : E_FAIL);
  }

  hr = dev->SetRenderTarget(0, rt.get());
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("SetRenderTarget(rt)", hr);
  }

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_ARGB(0xFF, 0x12, 0x34, 0x56), 1.0f, 0);
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("Device::Clear", hr);
  }

  // Flush the clear so it lands in a separate submission; this ensures the subsequent readback submission
  // treats `rt` as READONLY (source) while still writing `sysmem` (destination).
  int rc = WaitForGpuEventQuery(kTestName, dev.get(), q.get(), 5000);
  if (rc != 0) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return rc;
  }

  // Restore the previous render target.
  hr = dev->SetRenderTarget(0, prev_rt.get());
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("SetRenderTarget(prev)", hr);
  }

  hr = dev->GetRenderTargetData(rt.get(), sysmem.get());
  if (FAILED(hr)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.FailHresult("GetRenderTargetData", hr);
  }

  aerogpu_escape_dump_ring_v2_inout after;
  if (!aerogpu_test::kmt::AerogpuDumpRingV2(&kmt, adapter, /*ring_id=*/0, &after, &st)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("D3DKMTEscape(dump-ring-v2 after) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  const uint32_t tail_after = (uint32_t)after.tail;
  if (tail_after <= tail_before) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("ring tail did not advance (before=%lu after=%lu)", (unsigned long)tail_before, (unsigned long)tail_after);
  }

  // Find the newest descriptor in the post-submit dump that is newer than tail_before and has an alloc table.
  const uint32_t desc_count = (uint32_t)after.desc_count;
  const uint32_t start_index = (desc_count <= tail_after) ? (tail_after - desc_count) : 0;

  bool found_desc = false;
  aerogpu_dbgctl_ring_desc_v2 d = {};
  uint32_t ring_index = 0;
  for (int j = (int)desc_count - 1; j >= 0; --j) {
    const uint32_t idx = start_index + (uint32_t)j;
    if (idx < tail_before) {
      continue;
    }
    const aerogpu_dbgctl_ring_desc_v2& cur = after.desc[j];
    if (cur.alloc_table_gpa == 0 || cur.alloc_table_size_bytes == 0) {
      continue;
    }

    // Ensure this is a writeback submission by scanning the command stream for COPY_* WRITEBACK_DST.
    // This should correspond to GetRenderTargetData's copy path when transfer is supported.
    if (cur.cmd_gpa != 0 && cur.cmd_size_bytes != 0) {
      aerogpu_escape_read_gpa_inout cmd_read;
      NTSTATUS st2 = 0;
      const uint32_t cmd_read_bytes =
          (cur.cmd_size_bytes < AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) ? cur.cmd_size_bytes : AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
      if (aerogpu_test::kmt::AerogpuReadGpa(&kmt, adapter, cur.cmd_gpa, cmd_read_bytes, &cmd_read, &st2)) {
        if (cmd_read.bytes_copied >= sizeof(struct aerogpu_cmd_stream_header) &&
            CmdStreamHasWritebackCopy((const unsigned char*)cmd_read.data, (uint32_t)cmd_read.bytes_copied)) {
          d = cur;
          ring_index = idx;
          found_desc = true;
          break;
        }
      }
    }
  }

  if (!found_desc) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("failed to locate a new WRITEBACK_DST ring descriptor with alloc table (tail_before=%lu tail_after=%lu desc_count=%lu)",
                         (unsigned long)tail_before,
                         (unsigned long)tail_after,
                         (unsigned long)desc_count);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: selected desc: ring_index=%lu fence=%I64u cmd_gpa=0x%I64X cmd_size_bytes=%lu alloc_table_gpa=0x%I64X alloc_table_size_bytes=%lu",
      kTestName,
      (unsigned long)ring_index,
      (unsigned long long)d.fence,
      (unsigned long long)d.cmd_gpa,
      (unsigned long)d.cmd_size_bytes,
      (unsigned long long)d.alloc_table_gpa,
      (unsigned long)d.alloc_table_size_bytes);

  if (d.alloc_table_size_bytes < sizeof(struct aerogpu_alloc_table_header)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("alloc_table_size_bytes too small (%lu < %lu)",
                         (unsigned long)d.alloc_table_size_bytes,
                         (unsigned long)sizeof(struct aerogpu_alloc_table_header));
  }

  aerogpu_escape_read_gpa_inout read;
  const uint32_t to_read =
      (d.alloc_table_size_bytes < AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) ? d.alloc_table_size_bytes : AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
  if (!aerogpu_test::kmt::AerogpuReadGpa(&kmt, adapter, d.alloc_table_gpa, to_read, &read, &st)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: READ_GPA not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      return reporter.Pass();
    }
    return reporter.Fail("READ_GPA alloc table failed (NTSTATUS=0x%08lX)", (unsigned long)st);
  }

  if (read.bytes_copied < sizeof(struct aerogpu_alloc_table_header)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("READ_GPA returned too few bytes (%lu)", (unsigned long)read.bytes_copied);
  }

  const struct aerogpu_alloc_table_header* hdr = (const struct aerogpu_alloc_table_header*)read.data;
  if (hdr->magic != AEROGPU_ALLOC_TABLE_MAGIC) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("alloc table magic mismatch (got 0x%08lX expected 0x%08lX)",
                         (unsigned long)hdr->magic,
                         (unsigned long)AEROGPU_ALLOC_TABLE_MAGIC);
  }
  if (hdr->entry_stride_bytes < sizeof(struct aerogpu_alloc_entry)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("alloc table entry_stride_bytes too small (%lu < %lu)",
                         (unsigned long)hdr->entry_stride_bytes,
                         (unsigned long)sizeof(struct aerogpu_alloc_entry));
  }

  const uint32_t stride = hdr->entry_stride_bytes;
  uint32_t entry_count = hdr->entry_count;
  const size_t avail = (size_t)read.bytes_copied - sizeof(*hdr);
  const uint32_t max_entries_in_buf = (stride != 0) ? (uint32_t)(avail / stride) : 0;
  if (entry_count > max_entries_in_buf) {
    entry_count = max_entries_in_buf;
  }

  uint32_t readonly_count = 0;
  uint32_t writable_count = 0;

  const unsigned char* entries = (const unsigned char*)(hdr + 1);
  for (uint32_t i = 0; i < entry_count; ++i) {
    const struct aerogpu_alloc_entry* e = (const struct aerogpu_alloc_entry*)(entries + (size_t)i * (size_t)stride);
    if ((e->flags & AEROGPU_ALLOC_FLAG_READONLY) != 0) {
      readonly_count++;
    } else {
      writable_count++;
    }
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: alloc_table entries=%lu (parsed=%lu) readonly=%lu writable=%lu",
      kTestName,
      (unsigned long)hdr->entry_count,
      (unsigned long)entry_count,
      (unsigned long)readonly_count,
      (unsigned long)writable_count);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (entry_count == 0) {
    return reporter.Fail("alloc table had 0 parseable entries");
  }
  if (readonly_count == 0) {
    return reporter.Fail("expected at least one READONLY allocation in alloc table, got 0");
  }
  if (writable_count == 0) {
    return reporter.Fail("expected at least one writable allocation in alloc table, got 0");
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunAllocTableReadonlySanity(argc, argv);
}
