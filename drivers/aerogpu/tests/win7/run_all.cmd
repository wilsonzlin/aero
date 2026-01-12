@echo off
setlocal enabledelayedexpansion

set "BIN=%~dp0bin"

rem Prefer the native suite runner when available.
rem It supports timeouts, JSON aggregation, per-test log capture, and optional dbgctl snapshots.
set "SUITE_RUNNER=%BIN%\\aerogpu_test_runner.exe"
if exist "%SUITE_RUNNER%" (
  echo INFO: using suite runner: %SUITE_RUNNER%
  "%SUITE_RUNNER%" %*
  exit /b !errorlevel!
)

set "TIMEOUT_MS=%AEROGPU_TEST_TIMEOUT_MS%"
if "%TIMEOUT_MS%"=="" set "TIMEOUT_MS=30000"

set "SHOW_HELP="
set "NO_TIMEOUT="
set "EXPECT_TIMEOUT_VALUE="
for %%A in (%*) do (
  if defined EXPECT_TIMEOUT_VALUE (
    set "TIMEOUT_VALUE=%%~A"
    if "!TIMEOUT_VALUE:~0,1!"=="-" (
      echo ERROR: --timeout-ms requires a value
      exit /b 1
    )
    set "TIMEOUT_MS=!TIMEOUT_VALUE!"
    set "EXPECT_TIMEOUT_VALUE="
  ) else (
    set "ARG=%%~A"
    set "HAS_EQ="
    if not "!ARG:==!"=="!ARG!" set "HAS_EQ=1"
    if /I "!ARG!"=="--help" set "SHOW_HELP=1"
    if /I "!ARG!"=="-h" set "SHOW_HELP=1"
    if "!ARG!"=="/?" set "SHOW_HELP=1"
    if /I "!ARG!"=="--no-timeout" set "NO_TIMEOUT=1"
    if /I "!ARG!"=="--timeout-ms" set "EXPECT_TIMEOUT_VALUE=1"
    for /f "tokens=1,2 delims==" %%a in ("!ARG!") do (
      if /I "%%a"=="--timeout-ms" (
        set "TIMEOUT_VALUE=%%b"
        if defined HAS_EQ (
          if "!TIMEOUT_VALUE!"=="" (
            echo ERROR: --timeout-ms requires a value
            exit /b 1
          )
          if "!TIMEOUT_VALUE:~0,1!"=="-" (
            echo ERROR: --timeout-ms requires a value
            exit /b 1
          )
        )
        if not "!TIMEOUT_VALUE!"=="" set "TIMEOUT_MS=!TIMEOUT_VALUE!"
      )
    )
  )
)
if defined SHOW_HELP goto :help
if defined EXPECT_TIMEOUT_VALUE (
  echo ERROR: --timeout-ms requires a value
  exit /b 1
)
if defined NO_TIMEOUT (
  rem aerogpu_timeout_runner.exe treats 0xFFFFFFFF as INFINITE, allowing us to keep using the
  rem wrapper (for consistent JSON behavior) while disabling timeout enforcement.
  set "TIMEOUT_MS=4294967295"
)

set "ROOT=%~dp0"
set "MANIFEST=%ROOT%tests_manifest.txt"
if not exist "%MANIFEST%" (
  echo ERROR: tests manifest not found: %MANIFEST%
  exit /b 1
)

rem The suite uses --timeout-ms=NNNN (or --timeout-ms NNNN) to configure aerogpu_timeout_runner.exe; avoid forwarding that
rem flag into tests (vblank_wait_sanity has its own --timeout-ms for per-wait timeouts).
set "TEST_ARGS="
set "EXPECT_TIMEOUT_VALUE="
for %%A in (%*) do (
  set "ARG=%%~A"
  set "RAW=%%A"
  if defined EXPECT_TIMEOUT_VALUE (
    set "EXPECT_TIMEOUT_VALUE="
  ) else (
    set "SKIP_ARG="
    for /f "tokens=1 delims==" %%a in ("!ARG!") do (
      if /I "%%a"=="--timeout-ms" set "SKIP_ARG=1"
    )
    if /I "!ARG!"=="--timeout-ms" set "EXPECT_TIMEOUT_VALUE=1"
    if /I "!ARG!"=="--no-timeout" set "SKIP_ARG=1"
    if not defined SKIP_ARG set "TEST_ARGS=!TEST_ARGS! !RAW!"
  )
)
set "RUNNER=%BIN%\\aerogpu_timeout_runner.exe"
set /a FAILURES=0

if exist "%RUNNER%" (
  call :validate_timeout || exit /b 1
  if defined NO_TIMEOUT (
    echo INFO: using timeout runner: %RUNNER% ^(timeout disabled^)
  ) else (
    echo INFO: using timeout runner: %RUNNER% ^(timeout=%TIMEOUT_MS% ms^)
  )
) else (
echo INFO: timeout runner not found; running tests without enforced timeout
)

for /f "usebackq tokens=1" %%A in ("%MANIFEST%") do (
  call :run_manifest_line "%%A" !TEST_ARGS!
)

echo.
if %FAILURES%==0 (
  echo ALL TESTS PASSED
  exit /b 0
) else (
  echo %FAILURES% TEST^(S^) FAILED
  exit /b 1
)

:validate_timeout
set "NON_DIGIT="
for /f "delims=0123456789" %%X in ("!TIMEOUT_MS!") do set "NON_DIGIT=1"
if defined NON_DIGIT (
  echo ERROR: invalid --timeout-ms value: !TIMEOUT_MS!
  exit /b 1
)
if "!TIMEOUT_MS:0=!"=="" (
  echo ERROR: --timeout-ms must be ^> 0
  exit /b 1
)

rem Must fit in uint32 (timeout_runner parses the timeout as a DWORD).
set "T=!TIMEOUT_MS!"
:validate_timeout_strip_leading_zeros
if not "!T!"=="" if "!T:~0,1!"=="0" (
  set "T=!T:~1!"
  goto validate_timeout_strip_leading_zeros
)
if "!T!"=="" set "T=0"
if not "!T:~10,1!"=="" (
  echo ERROR: invalid --timeout-ms value: !TIMEOUT_MS! ^(must be ^<= 4294967295^)
  exit /b 1
)
if not "!T:~9,1!"=="" (
  rem Compare against 4294967295 without overflow: split into two 5-digit chunks.
  set "HI=!T:~0,5!"
  set "LO=!T:~5,5!"
  if !HI! gtr 42949 (
    echo ERROR: invalid --timeout-ms value: !TIMEOUT_MS! ^(must be ^<= 4294967295^)
    exit /b 1
  )
  if "!HI!"=="42949" (
    if !LO! gtr 67295 (
      echo ERROR: invalid --timeout-ms value: !TIMEOUT_MS! ^(must be ^<= 4294967295^)
      exit /b 1
    )
  )
)
exit /b 0

:run_manifest_line
set "NAME=%~1"
shift
if "%NAME%"=="" exit /b 0
if "%NAME:~0,1%"=="#" exit /b 0
if "%NAME:~0,1%"==";" exit /b 0
if /I "%NAME%"=="rem" exit /b 0
if "%NAME:~0,2%"=="::" exit /b 0

call :run_test "%NAME%" %*
exit /b 0

:run_test
set "NAME=%~1"
set "EXE=%BIN%\\%NAME%.exe"
shift
echo.
echo === Running %NAME% ===
if not exist "%EXE%" (
  if exist "%ROOT%%NAME%\" (
    echo FAIL: %NAME% ^(missing binary: %EXE%^) 
    set /a FAILURES+=1
    rem If JSON output is requested, attempt to write a fallback report via the timeout runner.
    rem (It will fail quickly because the child binary is missing.)
    if exist "%RUNNER%" (
      "%RUNNER%" "!TIMEOUT_MS!" "%EXE%" %* >NUL
    )
  ) else (
    echo INFO: skipping %NAME% ^(not present in this checkout^)
  )
  exit /b 0
)

if exist "%RUNNER%" (
  "%RUNNER%" "!TIMEOUT_MS!" "%EXE%" %*
) else (
  "%EXE%" %*
)
if errorlevel 1 (
  echo FAIL: %NAME%
  set /a FAILURES+=1
) else (
  echo PASS: %NAME%
)
exit /b 0

:help
echo Usage: run_all.cmd [--dump] [--hidden] [--show] [--validate-sharing] [--no-validate-sharing] [--producers=N] [--samples=N] [--interval-ms=N] [--iterations=N] [--stress-iterations=N] [--wait-timeout-ms=N] [--display \\.\DISPLAYn] [--ring-id=N] [--timeout-ms=NNNN] [--timeout-ms NNNN] [--no-timeout] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--require-agpu] [--allow-remote]
echo.
echo Notes:
echo   --require-vid/--require-did helps avoid false PASS when AeroGPU isn't active.
echo   Rendering tests expect adapter description to contain "AeroGPU" unless --allow-non-aerogpu is provided.
echo   Rendering tests validate that the expected AeroGPU UMD DLL is loaded unless --allow-microsoft/--allow-non-aerogpu is set; use --require-umd to force the UMD check.
echo   --require-agpu forces AGPU-only validation paths (e.g. ring descriptor PRESENT/alloc table checks) to fail instead of skipping on legacy device/ring formats.
echo   --samples affects pacing/sampling tests ^(dwm_flush_pacing, vblank_wait, wait_vblank_pacing, vblank_wait_pacing, vblank_wait_sanity, vblank_state_sanity, fence_state_sanity, ring_state_sanity, get_scanline_sanity, d3d9_raster_status_sanity, d3d9_raster_status_pacing^).
echo   --interval-ms affects vblank_state_sanity, fence_state_sanity, and ring_state_sanity: delay between escape samples.
echo   --iterations affects d3d9ex_event_query and d3d9ex_submit_fence_stress: number of iterations/submissions to run.
echo   --producers affects d3d9ex_shared_surface_many_producers: number of producer processes (default 8).
echo   --wait-timeout-ms affects wait_vblank_pacing and vblank_wait_sanity: per-wait timeout for D3DKMTWaitForVerticalBlankEvent.
echo   --display affects vblank_wait ^(defaults to primary display: \\.\DISPLAY1^).
echo   --ring-id affects ring_state_sanity: which ring ID to dump (default 0).
  echo   --allow-remote skips tests that are not meaningful in RDP sessions ^(SM_REMOTESESSION=1^): device_state_sanity, d3d9ex_dwm_probe, d3d9ex_submit_fence_stress, fence_state_sanity, ring_state_sanity, dwm_flush_pacing, vblank_wait, wait_vblank_pacing, vblank_wait_pacing, vblank_wait_sanity, vblank_state_sanity, get_scanline_sanity, scanout_state_sanity, dump_createalloc_sanity, umd_private_sanity, transfer_feature_sanity, d3d9_raster_status_sanity, d3d9_raster_status_pacing.
echo   --show affects d3d9ex_event_query, d3d9ex_submit_fence_stress, d3d9ex_shared_surface, d3d9ex_shared_surface_ipc, d3d9ex_shared_surface_wow64, and d3d9ex_shared_surface_many_producers: show their windows (overrides --hidden).
echo   d3d9ex_shared_surface validates cross-process pixel sharing by default; use --no-validate-sharing to skip readback validation ^(--dump always validates^).
echo   --json emits machine-readable JSON (forwarded to each test). To get an aggregated suite report, run bin\\aerogpu_test_runner.exe directly.
echo   Use --timeout-ms=NNNN ^(or --timeout-ms NNNN^) or set AEROGPU_TEST_TIMEOUT_MS to override the default per-test timeout (%TIMEOUT_MS% ms) when aerogpu_timeout_runner.exe is present.
echo   Use --no-timeout to run without enforcing a timeout.
exit /b 0

