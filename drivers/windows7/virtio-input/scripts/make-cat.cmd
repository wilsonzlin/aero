@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Generates a driver catalog for Windows 7 (x86 + x64) using Inf2Cat.
rem
rem Prerequisites:
rem   - Inf2Cat.exe available in PATH (run from a WDK command prompt)
rem   - inf\aero_virtio_input.inf exists
rem   - All files referenced by the INF exist in inf\ (at minimum aero_virtio_input.sys)

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf
set INF_FILE=%INF_DIR%\aero_virtio_input.inf
set CAT_FILE=%INF_DIR%\aero_virtio_input.cat
set ALIAS_INF_DISABLED=%INF_DIR%\virtio-input.inf.disabled

if not exist "%INF_FILE%" (
  echo ERROR: INF not found: "%INF_FILE%"
  if exist "%ALIAS_INF_DISABLED%" (
    echo NOTE: A legacy filename alias INF exists as:
    echo         "%ALIAS_INF_DISABLED%"
    echo       It is intentionally checked in disabled-by-default to avoid shipping/installing two INFs.
  )
  exit /b 1
)

where Inf2Cat.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: Inf2Cat.exe not found in PATH.
  echo        Install WDK 7.1 or WDK 10 and run this from a WDK Developer Command Prompt.
  exit /b 1
)

echo.
echo == Inf2Cat (Windows 7 x86 + x64) ==
echo Driver package dir: "%INF_DIR%"
echo.

rem Delete any existing catalog first so stale artifacts cannot satisfy the
rem post-Inf2Cat existence check.
if exist "%CAT_FILE%" del /f /q "%CAT_FILE%" >nul 2>nul

Inf2Cat.exe /driver:"%INF_DIR%" /os:7_X86,7_X64 /verbose
if errorlevel 1 (
  echo.
  echo ERROR: Inf2Cat failed.
  echo NOTE: Ensure "%INF_DIR%\aero_virtio_input.sys" exists (and any other files referenced by the INF).
  exit /b 1
)

if not exist "%CAT_FILE%" (
  echo ERROR: Expected catalog not found: "%CAT_FILE%"
  exit /b 1
)

echo.
echo OK: Created "%CAT_FILE%"
exit /b 0

