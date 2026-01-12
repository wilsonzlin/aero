@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Signs the Aero virtio-input driver package with a test certificate.
rem
rem Expects:
rem   cert\aero-virtio-input-test.pfx
rem   inf\aero_virtio_input.sys
rem   inf\aero_virtio_input.cat
rem
rem Usage:
rem   sign-driver.cmd [PFX_PASSWORD]
rem
rem Alternatively set an environment variable:
rem   set PFX_PASSWORD=...
rem   sign-driver.cmd

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf
set SYS_FILE=%INF_DIR%\aero_virtio_input.sys
set CAT_FILE=%INF_DIR%\aero_virtio_input.cat
set PFX_FILE=%ROOT_DIR%\cert\aero-virtio-input-test.pfx

if not exist "%PFX_FILE%" (
  echo ERROR: PFX not found: "%PFX_FILE%"
  echo        Run: powershell -ExecutionPolicy Bypass -File "%SCRIPT_DIR%make-cert.ps1"
  exit /b 1
)

if not exist "%SYS_FILE%" (
  echo ERROR: Driver binary not found: "%SYS_FILE%"
  echo        Build the driver and copy aero_virtio_input.sys into the inf\ directory.
  exit /b 1
)

if not exist "%CAT_FILE%" (
  echo ERROR: Catalog not found: "%CAT_FILE%"
  echo        Run: "%SCRIPT_DIR%make-cat.cmd"
  exit /b 1
)

where signtool.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: signtool.exe not found in PATH.
  echo        Install WDK 7.1 or WDK 10 and run this from a WDK Developer Command Prompt.
  exit /b 1
)

if defined PFX_PASSWORD (
  set SIGN_PFX_PASS=%PFX_PASSWORD%
) else (
  if "%~1"=="" (
    set /p SIGN_PFX_PASS=Enter PFX password:
  ) else (
    set SIGN_PFX_PASS=%~1
  )
)

if "%SIGN_PFX_PASS%"=="" (
  echo ERROR: PFX password is required.
  exit /b 1
)

echo.
echo == Signing driver package ==
echo PFX: "%PFX_FILE%"
echo SYS: "%SYS_FILE%"
echo CAT: "%CAT_FILE%"
echo.

rem For maximum Windows 7 SP1 compatibility, use SHA1 file digests for test signing.
signtool.exe sign /v /fd SHA1 /f "%PFX_FILE%" /p "%SIGN_PFX_PASS%" "%SYS_FILE%"
if errorlevel 1 (
  echo ERROR: Failed to sign SYS.
  exit /b 1
)

signtool.exe sign /v /fd SHA1 /f "%PFX_FILE%" /p "%SIGN_PFX_PASS%" "%CAT_FILE%"
if errorlevel 1 (
  echo ERROR: Failed to sign CAT.
  exit /b 1
)

echo.
echo OK: Signed SYS and CAT.
exit /b 0

