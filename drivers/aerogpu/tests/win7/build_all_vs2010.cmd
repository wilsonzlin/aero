@echo off
setlocal

echo === Building AeroGPU Win7 test suite (VS2010) ===

set "ROOT=%~dp0"
set "MANIFEST=%ROOT%tests_manifest.txt"

if not exist "%MANIFEST%" (
  echo ERROR: tests manifest not found: %MANIFEST%
  exit /b 1
)

echo.
echo === Building timeout_runner ===
call "%ROOT%timeout_runner\build_vs2010.cmd" || exit /b 1

for /f "usebackq tokens=1" %%A in ("%MANIFEST%") do (
  call :build_test "%%A" || exit /b 1
)

echo.
echo Build complete. Binaries are in: %~dp0bin\
exit /b 0

:build_test
set "NAME=%~1"
if "%NAME%"=="" exit /b 0
if "%NAME:~0,1%"=="#" exit /b 0
if "%NAME:~0,1%"==";" exit /b 0
if /I "%NAME%"=="rem" exit /b 0
if "%NAME:~0,2%"=="::" exit /b 0

set "TESTDIR=%ROOT%%NAME%"
if not exist "%TESTDIR%\" (
  echo INFO: skipping %NAME% ^(not present^)
  exit /b 0
)

set "BUILDCMD=%TESTDIR%\build_vs2010.cmd"
if not exist "%BUILDCMD%" (
  echo ERROR: %NAME%: missing build script: %BUILDCMD%
  exit /b 1
)

echo.
echo === Building %NAME% ===
call "%BUILDCMD%" || exit /b 1
exit /b 0

