@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Generates a driver catalog for Windows 7 (x86 + x64) using Inf2Cat.
rem
rem Prerequisites:
rem   - Inf2Cat.exe available in PATH (run from a WDK command prompt)
rem   - inf\aero-virtio-snd.inf exists (or the legacy name inf\virtio-snd.inf)
rem   - All files referenced by the INF exist in inf\ (at minimum virtiosnd.sys)

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf
set INF_FILE=%INF_DIR%\aero-virtio-snd.inf
set CAT_FILE=%INF_DIR%\aero-virtio-snd.cat
if not exist "%INF_FILE%" (
  set INF_FILE=%INF_DIR%\virtio-snd.inf
  set CAT_FILE=%INF_DIR%\virtio-snd.cat
)
set SYS_FILE=%INF_DIR%\virtiosnd.sys

if not exist "%INF_FILE%" (
  echo ERROR: INF not found: "%INF_FILE%"
  exit /b 1
)

if not exist "%SYS_FILE%" (
  echo ERROR: Driver binary not found: "%SYS_FILE%"
  echo        Build the driver and copy virtiosnd.sys into the inf\ directory.
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

Inf2Cat.exe /driver:"%INF_DIR%" /os:7_X86,7_X64 /verbose
if errorlevel 1 (
  echo.
  echo ERROR: Inf2Cat failed.
  echo NOTE: Ensure "%SYS_FILE%" exists (and any other files referenced by the INF).
  exit /b 1
)

if not exist "%CAT_FILE%" (
  echo ERROR: Expected catalog not found: "%CAT_FILE%"
  exit /b 1
)

echo.
echo OK: Created "%CAT_FILE%"
exit /b 0

