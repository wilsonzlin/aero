@echo off
setlocal enabledelayedexpansion

set "TIMEOUT_MS=%AEROGPU_TEST_TIMEOUT_MS%"
if "%TIMEOUT_MS%"=="" set "TIMEOUT_MS=30000"

set "SHOW_HELP="
set "NO_TIMEOUT="
for %%A in (%*) do (
  if /I "%%~A"=="--help" set "SHOW_HELP=1"
  if /I "%%~A"=="-h" set "SHOW_HELP=1"
  if "%%~A"=="/?" set "SHOW_HELP=1"
  if /I "%%~A"=="--no-timeout" set "NO_TIMEOUT=1"
  for /f "tokens=1,2 delims==" %%a in ("%%~A") do (
    if /I "%%a"=="--timeout-ms" if not "%%b"=="" set "TIMEOUT_MS=%%b"
  )
)
if defined SHOW_HELP goto :help

set "ROOT=%~dp0"
set "MANIFEST=%ROOT%tests_manifest.txt"
if not exist "%MANIFEST%" (
  echo ERROR: tests manifest not found: %MANIFEST%
  exit /b 1
)

rem The suite uses --timeout-ms=NNNN to configure aerogpu_timeout_runner.exe; avoid forwarding that
rem flag into tests (vblank_wait_sanity has its own --timeout-ms for per-wait timeouts).
set "TEST_ARGS="
for %%A in (%*) do (
  set "ARG=%%~A"
  set "SKIP_ARG="
  for /f "tokens=1 delims==" %%a in ("!ARG!") do (
    if /I "%%a"=="--timeout-ms" set "SKIP_ARG=1"
  )
  if not defined SKIP_ARG set "TEST_ARGS=!TEST_ARGS! !ARG!"
)

set "BIN=%~dp0bin"
set "RUNNER=%BIN%\\aerogpu_timeout_runner.exe"
set /a FAILURES=0

if exist "%RUNNER%" (
  if defined NO_TIMEOUT (
    echo INFO: timeout runner found but disabled by --no-timeout
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
  ) else (
    echo INFO: skipping %NAME% ^(not present in this checkout^)
  )
  exit /b 0
)

if exist "%RUNNER%" if not defined NO_TIMEOUT (
  "%RUNNER%" %TIMEOUT_MS% "%EXE%" %*
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
echo Usage: run_all.cmd [--dump] [--hidden] [--show] [--validate-sharing] [--no-validate-sharing] [--samples=N] [--iterations=N] [--wait-timeout-ms=N] [--display \\.\DISPLAYn] [--timeout-ms=NNNN] [--no-timeout] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--allow-remote]
echo.
echo Notes:
echo   --require-vid/--require-did helps avoid false PASS when AeroGPU isn't active.
echo   Rendering tests expect adapter description to contain "AeroGPU" unless --allow-non-aerogpu is provided.
echo   Rendering tests validate that the expected AeroGPU UMD DLL is loaded unless --allow-microsoft/--allow-non-aerogpu is set; use --require-umd to force the UMD check.
echo   --samples affects pacing/sampling tests ^(dwm_flush_pacing, vblank_wait, wait_vblank_pacing, vblank_wait_pacing, vblank_wait_sanity, get_scanline_sanity, d3d9_raster_status_sanity, d3d9_raster_status_pacing^).
echo   --iterations affects d3d9ex_event_query: number of query submissions to run.
echo   --wait-timeout-ms affects wait_vblank_pacing and vblank_wait_sanity: per-wait timeout for D3DKMTWaitForVerticalBlankEvent.
echo   --display affects vblank_wait ^(defaults to primary display: \\.\DISPLAY1^).
echo   --allow-remote skips tests that are not meaningful in RDP sessions ^(SM_REMOTESESSION=1^): d3d9ex_dwm_probe, dwm_flush_pacing, vblank_wait, wait_vblank_pacing, vblank_wait_pacing, vblank_wait_sanity, get_scanline_sanity, d3d9_raster_status_sanity, d3d9_raster_status_pacing.
echo   --show affects d3d9ex_event_query, d3d9ex_shared_surface, and d3d9ex_shared_surface_ipc: show their windows (overrides --hidden).
echo   d3d9ex_shared_surface validates cross-process pixel sharing by default; use --no-validate-sharing to skip readback validation ^(--dump always validates^).
echo   Use --timeout-ms=NNNN or set AEROGPU_TEST_TIMEOUT_MS to override the default per-test timeout (%TIMEOUT_MS% ms) when aerogpu_timeout_runner.exe is present.
echo   Use --no-timeout to run without enforcing a timeout.
exit /b 0

