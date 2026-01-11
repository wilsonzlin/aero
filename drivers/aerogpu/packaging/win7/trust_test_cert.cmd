@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem trust_test_cert.cmd
rem
rem Imports a test-signing certificate into:
rem   - LocalMachine\Root            (Trusted Root Certification Authorities)
rem   - LocalMachine\TrustedPublisher (Trusted Publishers)
rem and (optionally) enables Windows test-signing mode.
rem
rem This is intended for Win7 VMs when the driver package was signed on a build
rem host (e.g. via ci/sign-drivers.ps1, which produces out/certs/aero-test.cer).
rem -----------------------------------------------------------------------------

set "SCRIPT_DIR=%~dp0"
rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"
set "BCDEDIT=%SYS32%\bcdedit.exe"
set "CERTUTIL=%SYS32%\certutil.exe"

pushd "%SCRIPT_DIR%" >nul

if /i "%~1"=="--help" call :usage & exit /b 0
if /i "%~1"=="-h" call :usage & exit /b 0
if /i "%~1"=="/?" call :usage & exit /b 0

set "CERT_FILE="
set "NO_BCDEDIT=0"

for %%A in ("%~1" "%~2") do (
  if /i "%%~A"=="--no-bcdedit" (
    set "NO_BCDEDIT=1"
  ) else (
    if not "%%~A"=="" (
      if not defined CERT_FILE set "CERT_FILE=%%~A"
    )
  )
)

if not defined CERT_FILE (
  if exist "aero-test.cer" set "CERT_FILE=aero-test.cer"
)
if not defined CERT_FILE (
  rem CI artifacts usually place aero-test.cer at the root of the extracted bundle,
  rem while this script lives under drivers\<driver>\<arch>\packaging\win7\.
  rem Search a few parent directories so users can run this script without needing
  rem to manually specify a relative path.
  for %%P in ("." ".." "..\.." "..\..\.." "..\..\..\.." "..\..\..\..\.." "..\..\..\..\..\.." "..\..\..\..\..\..\..") do (
    if not defined CERT_FILE if exist "%%~fP\aero-test.cer" set "CERT_FILE=%%~fP\aero-test.cer"
  )
)
if not defined CERT_FILE (
  if exist "aerogpu_test.cer" set "CERT_FILE=aerogpu_test.cer"
)

if not defined CERT_FILE (
  echo [ERROR] No certificate file specified and no aero-test.cer/aerogpu_test.cer found in: "%SCRIPT_DIR%"
  call :usage
  goto :Fail
)

if not exist "%CERT_FILE%" (
  echo [ERROR] Certificate file not found: "%CERT_FILE%"
  goto :Fail
)

rem Admin check (net session requires elevation)
net session >nul 2>&1
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] trust_test_cert.cmd must be run as Administrator.
  goto :Fail
)

if "%NO_BCDEDIT%"=="0" (
  echo [INFO] Enabling test-signing mode...
  "%BCDEDIT%" /set testsigning on >nul 2>&1
  if not "%ERRORLEVEL%"=="0" (
    echo [WARN] bcdedit failed. If you are on a host with Secure Boot enabled, test-signing may be blocked.
    echo [WARN] On a Win7 VM, bcdedit should succeed.
  ) else (
    echo [OK] Test-signing mode enabled. Reboot is required before Windows enforces this setting.
  )
)

echo [INFO] Importing certificate into Trusted Root and Trusted Publishers...

"%CERTUTIL%" -f -addstore "Root" "%CERT_FILE%" >nul
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] certutil failed to add certificate to Root store.
  goto :Fail
)

"%CERTUTIL%" -f -addstore "TrustedPublisher" "%CERT_FILE%" >nul
if not "%ERRORLEVEL%"=="0" (
  echo [ERROR] certutil failed to add certificate to TrustedPublisher store.
  goto :Fail
)

echo [OK] Certificate imported:
echo       "%CERT_FILE%"
echo [NOTE] If you enabled test-signing mode, reboot Windows before installing the driver.
popd >nul
exit /b 0

:usage
echo Usage:
echo   trust_test_cert.cmd [cert.cer] [--no-bcdedit]
echo.
echo If cert.cer is omitted, this script will try:
echo   - aero-test.cer
echo   - aerogpu_test.cer
exit /b 0

:Fail
popd >nul
exit /b 1

