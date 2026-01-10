@echo off
setlocal enabledelayedexpansion

set "TIMEOUT_MS=%AEROGPU_TEST_TIMEOUT_MS%"
if "%TIMEOUT_MS%"=="" set "TIMEOUT_MS=30000"

if /I "%~1"=="--help" goto :help
if /I "%~1"=="-h" goto :help
if "%~1"=="/?" goto :help

set "BIN=%~dp0bin"
set "RUNNER=%BIN%\\aerogpu_timeout_runner.exe"
set /a FAILURES=0

call :run_test d3d9ex_dwm_probe %*
call :run_test d3d9ex_triangle %*
call :run_test d3d11_triangle %*
call :run_test readback_sanity %*

echo.
if %FAILURES%==0 (
  echo ALL TESTS PASSED
  exit /b 0
) else (
  echo %FAILURES% TEST^(S^) FAILED
  exit /b 1
)

:run_test
set "NAME=%~1"
set "EXE=%BIN%\\%NAME%.exe"
shift
echo.
echo === Running %NAME% ===
if not exist "%EXE%" (
  echo FAIL: %NAME% ^(missing binary: %EXE%^) 
  set /a FAILURES+=1
  exit /b 0
)

if exist "%RUNNER%" (
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
echo Usage: run_all.cmd [--dump] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--allow-remote]
echo.
echo Notes:
echo   --require-vid/--require-did helps avoid false PASS when AeroGPU isn't active.
echo   Rendering tests expect adapter description to contain "AeroGPU" unless --allow-non-aerogpu is provided.
echo   --allow-remote only affects d3d9ex_dwm_probe; other tests ignore it.
echo   Set AEROGPU_TEST_TIMEOUT_MS to override the default per-test timeout (%TIMEOUT_MS% ms) when aerogpu_timeout_runner.exe is present.
exit /b 0

