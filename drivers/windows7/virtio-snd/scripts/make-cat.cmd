@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Generates a driver catalog for Windows 7 (x86 + x64) using Inf2Cat.
rem
rem Prerequisites:
rem   - Inf2Cat.exe available in PATH (run from a WDK command prompt)
rem   - inf\aero-virtio-snd.inf and/or inf\virtio-snd.inf exists
rem   - All files referenced by the INF exist in inf\ (at minimum virtiosnd.sys)

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf
set AERO_INF=%INF_DIR%\aero-virtio-snd.inf
set AERO_CAT=%INF_DIR%\aero-virtio-snd.cat
set LEGACY_INF=%INF_DIR%\virtio-snd.inf
set LEGACY_CAT=%INF_DIR%\virtio-snd.cat
set SYS_FILE=%INF_DIR%\virtiosnd.sys

if not exist "%AERO_INF%" (
  if not exist "%LEGACY_INF%" (
    echo ERROR: No INF found under: "%INF_DIR%"
    echo        Expected one or both of:
    echo          - "%AERO_INF%"
    echo          - "%LEGACY_INF%"
    exit /b 1
  )
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

set MISSING_CAT=0

if exist "%AERO_INF%" (
  if not exist "%AERO_CAT%" (
    echo ERROR: Expected catalog not found: "%AERO_CAT%"
    set MISSING_CAT=1
  )
)

if exist "%LEGACY_INF%" (
  if not exist "%LEGACY_CAT%" (
    echo ERROR: Expected catalog not found: "%LEGACY_CAT%"
    set MISSING_CAT=1
  )
)

if "%MISSING_CAT%"=="1" (
  exit /b 1
)

echo.
echo OK: Created catalog(s):
if exist "%AERO_CAT%" echo   - "%AERO_CAT%"
if exist "%LEGACY_CAT%" echo   - "%LEGACY_CAT%"
exit /b 0

