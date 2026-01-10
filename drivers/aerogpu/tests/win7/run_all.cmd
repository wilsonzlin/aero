@echo off
setlocal enabledelayedexpansion

set "BIN=%~dp0bin"
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

"%EXE%" %*
if errorlevel 1 (
  echo FAIL: %NAME%
  set /a FAILURES+=1
) else (
  echo PASS: %NAME%
)
exit /b 0

