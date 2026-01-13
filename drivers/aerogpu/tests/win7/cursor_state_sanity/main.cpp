#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static int32_t ToS32(uint32_t v) { return (int32_t)v; }

static int AbsI32(int v) { return (v < 0) ? -v : v; }

static bool WantsJsonReport(int argc, char** argv) {
  for (int i = 1; i < argc; ++i) {
    const char* arg = argv[i];
    if (!arg) {
      continue;
    }
    // Match both `--json` and `--json=PATH`.
    if (aerogpu_test::StrIStartsWith(arg, "--json") && (arg[6] == 0 || arg[6] == '=')) {
      return true;
    }
  }
  return false;
}

static void DiagAppendV(std::string* out, const char* fmt, va_list ap) {
  if (!out || !fmt) {
    return;
  }
  out->append(aerogpu_test::FormatStringV(fmt, ap));
  out->push_back('\n');
}

static void DiagAppend(std::string* out, const char* fmt, ...) {
  if (!out || !fmt) {
    return;
  }
  va_list ap;
  va_start(ap, fmt);
  DiagAppendV(out, fmt, ap);
  va_end(ap);
}

struct TestCursorSpec {
  int width;
  int height;
  int hot_x;
  int hot_y;
};

static bool GetCursorShowing(bool* out_showing, std::string* err) {
  if (err) {
    err->clear();
  }
  if (!out_showing) {
    if (err) {
      *err = "GetCursorShowing: out_showing is NULL";
    }
    return false;
  }
  CURSORINFO ci;
  ZeroMemory(&ci, sizeof(ci));
  ci.cbSize = sizeof(ci);
  if (!GetCursorInfo(&ci)) {
    if (err) {
      *err = "GetCursorInfo failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }
  *out_showing = (ci.flags & CURSOR_SHOWING) != 0;
  return true;
}

// Adjust visibility to the requested state, tracking the number of ShowCursor calls performed.
// The caller must undo these calls in reverse (TRUE calls undone by FALSE calls, and vice versa)
// to restore the original display count.
static bool SetCursorShowing(bool want_showing,
                             int* out_show_calls,
                             int* out_hide_calls,
                             std::string* err) {
  if (err) {
    err->clear();
  }
  if (out_show_calls) {
    *out_show_calls = 0;
  }
  if (out_hide_calls) {
    *out_hide_calls = 0;
  }
  if (!out_show_calls || !out_hide_calls) {
    if (err) {
      *err = "SetCursorShowing: out_show_calls/out_hide_calls is NULL";
    }
    return false;
  }

  bool showing = false;
  if (!GetCursorShowing(&showing, err)) {
    return false;
  }

  // Bound the number of calls to avoid pathological counter values hanging the test.
  for (int i = 0; i < 128 && showing != want_showing; ++i) {
    if (want_showing) {
      ShowCursor(TRUE);
      (*out_show_calls)++;
    } else {
      ShowCursor(FALSE);
      (*out_hide_calls)++;
    }
    if (!GetCursorShowing(&showing, err)) {
      return false;
    }
  }

  if (showing != want_showing) {
    if (err) {
      *err = "failed to change cursor visibility (ShowCursor counter may be out of expected range)";
    }
    return false;
  }

  return true;
}

static void RestoreCursorShowing(int show_calls, int hide_calls) {
  // Undo in reverse: a previous ShowCursor(TRUE) increments the count; undo with FALSE.
  for (int i = 0; i < show_calls; ++i) {
    ShowCursor(FALSE);
  }
  for (int i = 0; i < hide_calls; ++i) {
    ShowCursor(TRUE);
  }
}

static HCURSOR CreateTestCursor(const TestCursorSpec& spec, std::string* err) {
  if (err) {
    err->clear();
  }

  const int w = spec.width;
  const int h = spec.height;
  const int hot_x = spec.hot_x;
  const int hot_y = spec.hot_y;
  if (w <= 0 || h <= 0 || w > 256 || h > 256 || hot_x < 0 || hot_y < 0 || hot_x >= w || hot_y >= h) {
    if (err) {
      *err = "CreateTestCursor: invalid cursor dimensions/hotspot";
    }
    return NULL;
  }

  HDC hdc = GetDC(NULL);
  if (!hdc) {
    if (err) {
      *err = "GetDC(NULL) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return NULL;
  }

  BITMAPV5HEADER bi;
  ZeroMemory(&bi, sizeof(bi));
  bi.bV5Size = sizeof(bi);
  bi.bV5Width = w;
  bi.bV5Height = -h;  // top-down
  bi.bV5Planes = 1;
  bi.bV5BitCount = 32;
  bi.bV5Compression = BI_BITFIELDS;
  bi.bV5RedMask = 0x00FF0000;
  bi.bV5GreenMask = 0x0000FF00;
  bi.bV5BlueMask = 0x000000FF;
  bi.bV5AlphaMask = 0xFF000000;

  void* bits = NULL;
  HBITMAP color = CreateDIBSection(hdc, (BITMAPINFO*)&bi, DIB_RGB_COLORS, &bits, NULL, 0);
  ReleaseDC(NULL, hdc);
  if (!color || !bits) {
    if (err) {
      *err = "CreateDIBSection failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    if (color) {
      DeleteObject(color);
    }
    return NULL;
  }

  // Simple deterministic pattern: diagonal line + colored corners.
  uint32_t* px = (uint32_t*)bits;
  for (int y = 0; y < h; ++y) {
    for (int x = 0; x < w; ++x) {
      uint32_t a = 0xFFu;
      uint32_t r = 0u;
      uint32_t g = 0u;
      uint32_t b = 0u;
      if (x == y || x == (w - 1 - y)) {
        r = 255u;
        g = 255u;
        b = 255u;
      } else if (x < 4 && y < 4) {
        r = 255u;
      } else if (x >= w - 4 && y < 4) {
        g = 255u;
      } else if (x < 4 && y >= h - 4) {
        b = 255u;
      } else if (x >= w - 4 && y >= h - 4) {
        r = 255u;
        g = 255u;
      } else {
        // Transparent background to make it easy to spot if it ever renders.
        a = 0u;
      }
      px[y * w + x] = (a << 24) | (r << 16) | (g << 8) | (b << 0);
    }
  }

  // 1bpp mask bitmap (all zeros). With alpha cursors, the alpha channel is expected to be used.
  HBITMAP mask = CreateBitmap(w, h, 1, 1, NULL);
  if (!mask) {
    if (err) {
      *err = "CreateBitmap(mask) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    DeleteObject(color);
    return NULL;
  }

  ICONINFO ii;
  ZeroMemory(&ii, sizeof(ii));
  ii.fIcon = FALSE;  // cursor
  ii.xHotspot = (DWORD)hot_x;
  ii.yHotspot = (DWORD)hot_y;
  ii.hbmMask = mask;
  ii.hbmColor = color;

  HCURSOR cur = (HCURSOR)CreateIconIndirect(&ii);
  if (!cur) {
    if (err) {
      *err = "CreateIconIndirect failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
  }

  DeleteObject(mask);
  DeleteObject(color);
  return cur;
}

static void PrintCursorQuery(const char* test_name, const aerogpu_escape_query_cursor_out& q) {
  const bool flags_valid = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0;
  const bool supported = flags_valid ? ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0) : true;
  aerogpu_test::PrintfStdout(
      "INFO: %s: cursor: flags=0x%08lX%s%s enable=%lu pos=(%ld,%ld) hot=(%lu,%lu) size=%lux%lu format=%lu pitch=%lu fb_gpa=0x%I64X",
      test_name,
      (unsigned long)q.flags,
      flags_valid ? " (valid)" : " (legacy)",
      supported ? "" : " (unsupported)",
      (unsigned long)q.enable,
      (long)ToS32((uint32_t)q.x),
      (long)ToS32((uint32_t)q.y),
      (unsigned long)q.hot_x,
      (unsigned long)q.hot_y,
      (unsigned long)q.width,
      (unsigned long)q.height,
      (unsigned long)q.format,
      (unsigned long)q.pitch_bytes,
      (unsigned long long)q.fb_gpa);
}

static void DiagAppendCursorQuery(std::string* diag, const char* label, const aerogpu_escape_query_cursor_out& q) {
  if (!diag || !label) {
    return;
  }
  const bool flags_valid = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0;
  const bool supported = flags_valid ? ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0) : true;
  DiagAppend(diag,
             "%s: flags=0x%08lX%s%s enable=%lu pos=(%ld,%ld) hot=(%lu,%lu) size=%lux%lu format=%lu pitch=%lu fb_gpa=0x%I64X",
             label,
             (unsigned long)q.flags,
             flags_valid ? " (valid)" : " (legacy)",
             supported ? "" : " (unsupported)",
             (unsigned long)q.enable,
             (long)ToS32((uint32_t)q.x),
             (long)ToS32((uint32_t)q.y),
             (unsigned long)q.hot_x,
             (unsigned long)q.hot_y,
             (unsigned long)q.width,
             (unsigned long)q.height,
             (unsigned long)q.format,
             (unsigned long)q.pitch_bytes,
             (unsigned long long)q.fb_gpa);
}

static int RunCursorStateSanity(int argc, char** argv) {
  const char* kTestName = "cursor_state_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]] [--allow-remote]", kTestName);
    aerogpu_test::PrintfStdout(
        "Moves the mouse cursor, sets a custom cursor shape, and queries the KMD cursor state via a driver-private "
        "escape.");
    aerogpu_test::PrintfStdout(
        "When run with --json, also writes cursor_state_sanity_diag.txt (cursor query snapshots) next to the exe and "
        "records it in the JSON artifacts array.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);
  const bool want_diag = WantsJsonReport(argc, argv);
  std::string diag;
  if (want_diag) {
    DiagAppend(&diag, "test=%s", kTestName);
  }

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  POINT orig_pos;
  ZeroMemory(&orig_pos, sizeof(orig_pos));
  if (!GetCursorPos(&orig_pos)) {
    return reporter.Fail("GetCursorPos failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  HCURSOR orig_cursor = GetCursor();

  bool orig_showing = true;
  std::string show_err;
  if (!GetCursorShowing(&orig_showing, &show_err)) {
    return reporter.Fail("%s", show_err.c_str());
  }
  aerogpu_test::PrintfStdout("INFO: %s: initial cursor showing=%s", kTestName, orig_showing ? "true" : "false");
  if (want_diag) {
    DiagAppend(&diag, "initial_cursor_showing=%s", orig_showing ? "true" : "false");
    DiagAppend(&diag, "orig_pos=(%ld,%ld)", (long)orig_pos.x, (long)orig_pos.y);
  }

  int ensure_show_calls = 0;
  int ensure_hide_calls = 0;
  if (!SetCursorShowing(true, &ensure_show_calls, &ensure_hide_calls, &show_err)) {
    RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);
    return reporter.Fail("%s", show_err.c_str());
  }
  if (want_diag) {
    DiagAppend(&diag,
               "ensure_showing: show_calls=%d hide_calls=%d",
               ensure_show_calls,
               ensure_hide_calls);
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGpuCursorStateSanityWnd",
                                              L"AeroGPU cursor_state_sanity",
                                              160,
                                              120,
                                              false);
  if (!hwnd) {
    RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);
    return reporter.Fail("CreateBasicWindow failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  std::string cursor_err;
  TestCursorSpec cursor_spec;
  cursor_spec.width = GetSystemMetrics(SM_CXCURSOR);
  cursor_spec.height = GetSystemMetrics(SM_CYCURSOR);
  if (cursor_spec.width <= 0 || cursor_spec.height <= 0 || cursor_spec.width > 256 || cursor_spec.height > 256) {
    cursor_spec.width = 32;
    cursor_spec.height = 32;
  }
  // Use a non-zero hotspot so we can deterministically detect a pointer-shape update.
  cursor_spec.hot_x = cursor_spec.width / 4;
  cursor_spec.hot_y = cursor_spec.height / 3;
  if (cursor_spec.hot_x <= 0) cursor_spec.hot_x = 1;
  if (cursor_spec.hot_y <= 0) cursor_spec.hot_y = 1;
  if (cursor_spec.hot_x >= cursor_spec.width) cursor_spec.hot_x = cursor_spec.width - 1;
  if (cursor_spec.hot_y >= cursor_spec.height) cursor_spec.hot_y = cursor_spec.height - 1;
  aerogpu_test::PrintfStdout("INFO: %s: creating custom cursor %dx%d hot=(%d,%d)",
                            kTestName,
                            cursor_spec.width,
                            cursor_spec.height,
                            cursor_spec.hot_x,
                            cursor_spec.hot_y);
  if (want_diag) {
    DiagAppend(&diag,
               "custom_cursor_spec=%dx%d hot=(%d,%d) SM_CXCURSOR=%d SM_CYCURSOR=%d",
               cursor_spec.width,
               cursor_spec.height,
               cursor_spec.hot_x,
               cursor_spec.hot_y,
               GetSystemMetrics(SM_CXCURSOR),
               GetSystemMetrics(SM_CYCURSOR));
  }

  HCURSOR custom_cursor = CreateTestCursor(cursor_spec, &cursor_err);
  if (!custom_cursor) {
    DestroyWindow(hwnd);
    RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);
    return reporter.Fail("%s", cursor_err.c_str());
  }

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    DestroyIcon(custom_cursor);
    DestroyWindow(hwnd);
    RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);
    return reporter.Fail("%s", kmt_err.c_str());
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    DestroyIcon(custom_cursor);
    DestroyWindow(hwnd);
    RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);
    return reporter.Fail("%s", open_err.c_str());
  }

  int result = 1;
  LONG_PTR prev_class_cursor = 0;
  bool have_prev_class_cursor = false;

  // ----- Move cursor to a deterministic location -----
  const int screen_w = GetSystemMetrics(SM_CXSCREEN);
  const int screen_h = GetSystemMetrics(SM_CYSCREEN);
  int target_x = (screen_w > 0) ? (screen_w / 2) : 100;
  int target_y = (screen_h > 0) ? (screen_h / 2) : 100;
  if (target_x < 16) target_x = 16;
  if (target_y < 16) target_y = 16;
  if (screen_w > 32 && target_x > screen_w - 16) target_x = screen_w - 16;
  if (screen_h > 32 && target_y > screen_h - 16) target_y = screen_h - 16;

  aerogpu_test::PrintfStdout("INFO: %s: moving cursor to (%d,%d) (screen=%dx%d)",
                            kTestName,
                            target_x,
                            target_y,
                            screen_w,
                            screen_h);
  if (want_diag) {
    DiagAppend(&diag, "screen=%dx%d target_pos=(%d,%d)", screen_w, screen_h, target_x, target_y);
  }
  if (!SetCursorPos(target_x, target_y)) {
    reporter.Fail("SetCursorPos failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    goto cleanup;
  }

  Sleep(50);

  aerogpu_escape_query_cursor_out q0;
  NTSTATUS st = 0;
  if (!aerogpu_test::kmt::AerogpuQueryCursor(&kmt, adapter, &q0, &st)) {
    if (st == aerogpu_test::kmt::kStatusNotSupported) {
      aerogpu_test::PrintfStdout("INFO: %s: QUERY_CURSOR escape not supported; skipping", kTestName);
      reporter.SetSkipped("not_supported");
      result = reporter.Pass();
      goto cleanup;
    }
    reporter.Fail("D3DKMTEscape(query-cursor) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    goto cleanup;
  }

  const bool flags_valid = (q0.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0;
  if (flags_valid && (q0.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) == 0) {
    aerogpu_test::PrintfStdout("INFO: %s: cursor MMIO not supported; skipping", kTestName);
    reporter.SetSkipped("not_supported");
    result = reporter.Pass();
    goto cleanup;
  }

  const int tol = 2;
  bool pos_ok = false;
  for (int attempt = 0; attempt < 3; ++attempt) {
    POINT actual_pos;
    ZeroMemory(&actual_pos, sizeof(actual_pos));
    if (!GetCursorPos(&actual_pos)) {
      reporter.Fail("GetCursorPos(after) failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      goto cleanup;
    }

    const int32_t mmio_x = ToS32((uint32_t)q0.x);
    const int32_t mmio_y = ToS32((uint32_t)q0.y);
    const int32_t hot_x = (int32_t)q0.hot_x;
    const int32_t hot_y = (int32_t)q0.hot_y;

    // Cursor position semantics can vary: the device registers may represent either the cursor
    // hot-spot position or the cursor top-left plus a separate hot-spot offset. Be tolerant and
    // accept either interpretation.
    const bool pos_match0 = (AbsI32((int)mmio_x - (int)actual_pos.x) <= tol) &&
                            (AbsI32((int)mmio_y - (int)actual_pos.y) <= tol);
    const bool pos_match1 = (AbsI32((int)(mmio_x + hot_x) - (int)actual_pos.x) <= tol) &&
                            (AbsI32((int)(mmio_y + hot_y) - (int)actual_pos.y) <= tol);
    const bool pos_match = pos_match0 || pos_match1;

    if (pos_match) {
      pos_ok = true;
      break;
    }
    if (attempt + 1 >= 3) {
      break;
    }
    Sleep(50);

    st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryCursor(&kmt, adapter, &q0, &st)) {
      reporter.Fail("D3DKMTEscape(query-cursor retry) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      goto cleanup;
    }
  }

  PrintCursorQuery(kTestName, q0);
  if (want_diag) {
    DiagAppendCursorQuery(&diag, "q0", q0);
  }

  if (!pos_ok) {
    POINT actual_pos;
    ZeroMemory(&actual_pos, sizeof(actual_pos));
    (void)GetCursorPos(&actual_pos);
    const int32_t mmio_x = ToS32((uint32_t)q0.x);
    const int32_t mmio_y = ToS32((uint32_t)q0.y);
    const int32_t hot_x = (int32_t)q0.hot_x;
    const int32_t hot_y = (int32_t)q0.hot_y;
    reporter.Fail("cursor pos mismatch: expected~(%ld,%ld) mmio_pos=(%ld,%ld) hot=(%ld,%ld) => hotspot=(%ld,%ld) or (%ld,%ld) (enable=%lu)",
                  (long)actual_pos.x,
                  (long)actual_pos.y,
                  (long)mmio_x,
                  (long)mmio_y,
                  (long)hot_x,
                  (long)hot_y,
                  (long)mmio_x,
                  (long)mmio_y,
                  (long)(mmio_x + hot_x),
                  (long)(mmio_y + hot_y),
                  (unsigned long)q0.enable);
    goto cleanup;
  }

  // ----- Program a custom cursor shape -----
  // Make the cursor shape update deterministic by ensuring:
  //  - a window owned by this thread is visible,
  //  - the cursor is positioned inside that window, and
  //  - we synchronously process a WM_SETCURSOR to apply the class cursor.
  //
  // This avoids relying on external windows' WM_SETCURSOR behavior (Explorer/desktop), and avoids
  // needing a message pump.
  SetLastError(0);
  prev_class_cursor = SetClassLongPtr(hwnd, GCLP_HCURSOR, (LONG_PTR)custom_cursor);
  const DWORD setclass_err = GetLastError();
  have_prev_class_cursor = (prev_class_cursor != 0 || setclass_err == 0);
  if (!have_prev_class_cursor) {
    reporter.Fail("SetClassLongPtr(GCLP_HCURSOR) failed: %s", aerogpu_test::Win32ErrorToString(setclass_err).c_str());
    goto cleanup;
  }
  (void)SetWindowPos(hwnd,
                     HWND_TOPMOST,
                     target_x - 80,
                     target_y - 60,
                     160,
                     120,
                     SWP_NOACTIVATE | SWP_SHOWWINDOW);
  ShowWindow(hwnd, SW_SHOWNOACTIVATE);
  UpdateWindow(hwnd);

  RECT client;
  ZeroMemory(&client, sizeof(client));
  if (!GetClientRect(hwnd, &client)) {
    reporter.Fail("GetClientRect failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    goto cleanup;
  }
  POINT inside;
  inside.x = (client.right - client.left) / 2;
  inside.y = (client.bottom - client.top) / 2;
  if (!ClientToScreen(hwnd, &inside)) {
    reporter.Fail("ClientToScreen failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    goto cleanup;
  }
  if (!SetCursorPos(inside.x, inside.y)) {
    reporter.Fail("SetCursorPos(to window) failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    goto cleanup;
  }
  (void)SendMessage(hwnd, WM_SETCURSOR, (WPARAM)hwnd, (LPARAM)MAKELONG(HTCLIENT, WM_MOUSEMOVE));

  Sleep(50);

  aerogpu_escape_query_cursor_out q1;
  ZeroMemory(&q1, sizeof(q1));
  bool shape_ok = false;
  for (int attempt = 0; attempt < 3; ++attempt) {
    st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryCursor(&kmt, adapter, &q1, &st)) {
      reporter.Fail("D3DKMTEscape(query-cursor after SetCursor) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      goto cleanup;
    }

    POINT pos;
    ZeroMemory(&pos, sizeof(pos));
    (void)GetCursorPos(&pos);
    const int32_t mmio_x = ToS32((uint32_t)q1.x);
    const int32_t mmio_y = ToS32((uint32_t)q1.y);
    const int32_t hot_x = (int32_t)q1.hot_x;
    const int32_t hot_y = (int32_t)q1.hot_y;
    const bool pos_match0 =
        (AbsI32((int)mmio_x - (int)pos.x) <= tol) && (AbsI32((int)mmio_y - (int)pos.y) <= tol);
    const bool pos_match1 = (AbsI32((int)(mmio_x + hot_x) - (int)pos.x) <= tol) &&
                            (AbsI32((int)(mmio_y + hot_y) - (int)pos.y) <= tol);
    const bool pos_match = pos_match0 || pos_match1;

    const bool shape_sane = (q1.width != 0 && q1.height != 0 && q1.pitch_bytes != 0 && q1.format != 0 && q1.fb_gpa != 0);
    const bool hot_ok = ((int)q1.hot_x == cursor_spec.hot_x && (int)q1.hot_y == cursor_spec.hot_y);

    if (q1.enable != 0 && shape_sane && hot_ok && pos_match) {
      shape_ok = true;
      break;
    }
    if (attempt + 1 >= 3) {
      break;
    }
    Sleep(50);
  }

  PrintCursorQuery(kTestName, q1);
  if (want_diag) {
    DiagAppendCursorQuery(&diag, "q1", q1);
  }

  if (!shape_ok) {
    reporter.Fail("cursor state did not reflect custom cursor within retry window");
    goto cleanup;
  }

  const unsigned long long row_bytes = (unsigned long long)q1.width * 4ull;
  if ((unsigned long long)q1.pitch_bytes < row_bytes) {
    reporter.Fail("cursor pitch too small for width: pitch=%lu width=%lu row_bytes=%I64u",
                  (unsigned long)q1.pitch_bytes,
                  (unsigned long)q1.width,
                  row_bytes);
    goto cleanup;
  }

  // ----- Toggle cursor visibility and validate enable flips -----
  int hide_calls = 0;
  int show_calls = 0;
  if (!SetCursorShowing(false, &show_calls, &hide_calls, &show_err)) {
    // Best-effort undo any counter changes made before the helper failed.
    RestoreCursorShowing(show_calls, hide_calls);
    reporter.Fail("%s", show_err.c_str());
    goto cleanup;
  }
  Sleep(50);

  aerogpu_escape_query_cursor_out q_hidden;
  ZeroMemory(&q_hidden, sizeof(q_hidden));
  bool hidden_ok = false;
  for (int attempt = 0; attempt < 3; ++attempt) {
    st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryCursor(&kmt, adapter, &q_hidden, &st)) {
      RestoreCursorShowing(show_calls, hide_calls);
      reporter.Fail("D3DKMTEscape(query-cursor after hide) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      goto cleanup;
    }
    if (q_hidden.enable == 0) {
      hidden_ok = true;
      break;
    }
    if (attempt + 1 >= 3) {
      break;
    }
    Sleep(50);
  }
  PrintCursorQuery(kTestName, q_hidden);
  if (want_diag) {
    DiagAppendCursorQuery(&diag, "q_hidden", q_hidden);
  }
  if (!hidden_ok) {
    RestoreCursorShowing(show_calls, hide_calls);
    reporter.Fail("cursor enable did not clear after hide (enable=%lu)", (unsigned long)q_hidden.enable);
    goto cleanup;
  }

  // Restore to showing.
  RestoreCursorShowing(show_calls, hide_calls);
  Sleep(50);

  aerogpu_escape_query_cursor_out q_shown;
  ZeroMemory(&q_shown, sizeof(q_shown));
  bool shown_ok = false;
  for (int attempt = 0; attempt < 3; ++attempt) {
    st = 0;
    if (!aerogpu_test::kmt::AerogpuQueryCursor(&kmt, adapter, &q_shown, &st)) {
      reporter.Fail("D3DKMTEscape(query-cursor after show restore) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      goto cleanup;
    }
    if (q_shown.enable != 0) {
      shown_ok = true;
      break;
    }
    if (attempt + 1 >= 3) {
      break;
    }
    Sleep(50);
  }
  PrintCursorQuery(kTestName, q_shown);
  if (want_diag) {
    DiagAppendCursorQuery(&diag, "q_shown", q_shown);
  }
  if (!shown_ok) {
    reporter.Fail("cursor enable did not restore after show (enable=%lu)", (unsigned long)q_shown.enable);
    goto cleanup;
  }

  result = reporter.Pass();

cleanup:
  // Best-effort restore of global state.
  if (orig_cursor) {
    if (SetCapture(hwnd)) {
      (void)SetCursor(orig_cursor);
      ReleaseCapture();
    } else {
      (void)SetCursor(orig_cursor);
    }
  }
  (void)SetCursorPos(orig_pos.x, orig_pos.y);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (have_prev_class_cursor && hwnd) {
    (void)SetClassLongPtr(hwnd, GCLP_HCURSOR, prev_class_cursor);
  }
  if (hwnd) {
    DestroyWindow(hwnd);
    hwnd = NULL;
  }
  if (custom_cursor) {
    DestroyIcon(custom_cursor);
    custom_cursor = NULL;
  }

  // Restore the cursor display counter if we changed it at the start.
  RestoreCursorShowing(ensure_show_calls, ensure_hide_calls);

  if (want_diag) {
    // Capture the final outcome for easier debugging from automation artifacts.
    DiagAppend(&diag, "exit_code=%d", result);
    const std::string last_failure = aerogpu_test::GetLastFailureMessageCopy();
    if (!last_failure.empty()) {
      DiagAppend(&diag, "failure=%s", last_failure.c_str());
    }

    const std::wstring dir = aerogpu_test::GetModuleDir();
    const std::wstring diag_path = aerogpu_test::JoinPath(dir, L"cursor_state_sanity_diag.txt");
    std::string write_err;
    if (aerogpu_test::WriteFileStringW(diag_path, diag, &write_err)) {
      reporter.AddArtifactPathW(diag_path);
      aerogpu_test::PrintfStdout("INFO: %s: wrote diagnostics: %ls", kTestName, diag_path.c_str());
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: failed to write diagnostics: %s", kTestName, write_err.c_str());
    }
  }

  return result;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunCursorStateSanity(argc, argv);
}
