@echo off
setlocal enabledelayedexpansion

REM ============================================================================
REM SetupComplete.cmd template (Windows 7)
REM Runs near the end of Windows Setup as LocalSystem.
REM ============================================================================

REM Directory where slipstreamed files are expected to be copied by $OEM$:
set "AERO_ROOT=%SystemDrive%\Aero"
set "AERO_CERT_PATH=%AERO_ROOT%\certs\{{AERO_CERT_FILENAME}}"
set "AERO_DRIVER_DIR=%AERO_ROOT%\drivers"
set "AERO_SIGNING_MODE={{AERO_SIGNING_MODE}}"

echo [Aero] SetupComplete starting...
echo [Aero] AERO_ROOT="%AERO_ROOT%"
echo [Aero] AERO_CERT_PATH="%AERO_CERT_PATH%"
echo [Aero] AERO_DRIVER_DIR="%AERO_DRIVER_DIR%"
echo [Aero] AERO_SIGNING_MODE="%AERO_SIGNING_MODE%"

REM --- Install certificate (Root + TrustedPublisher) ---
if exist "%AERO_CERT_PATH%" (
  echo [Aero] Installing test root cert...
  certutil -addstore -f Root "%AERO_CERT_PATH%"
  certutil -addstore -f TrustedPublisher "%AERO_CERT_PATH%"
) else (
  echo [Aero] WARNING: cert not found at "%AERO_CERT_PATH%"
)

REM --- Configure boot signing policy for future boots ---
if /I "%AERO_SIGNING_MODE%"=="testsigning" (
  echo [Aero] Enabling testsigning...
  bcdedit /set {current} testsigning on
  bcdedit /set {current} nointegritychecks off
) else if /I "%AERO_SIGNING_MODE%"=="nointegritychecks" (
  echo [Aero] Enabling nointegritychecks...
  bcdedit /set {current} testsigning off
  bcdedit /set {current} nointegritychecks on
) else (
  echo [Aero] NOTE: Unknown AERO_SIGNING_MODE="%AERO_SIGNING_MODE%"; not changing BCD.
)

REM --- Install drivers (best-effort) ---
if exist "%AERO_DRIVER_DIR%" (
  echo [Aero] Installing drivers via pnputil...
  for /r "%AERO_DRIVER_DIR%" %%F in (*.inf) do (
    echo [Aero] pnputil -i -a "%%F"
    pnputil -i -a "%%F"
  )
) else (
  echo [Aero] NOTE: driver dir not found at "%AERO_DRIVER_DIR%"
)

echo [Aero] SetupComplete finished.
exit /b 0
