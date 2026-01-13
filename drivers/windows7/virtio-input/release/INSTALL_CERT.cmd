@echo off
setlocal

cd /d "%~dp0"

echo.
echo === Aero virtio-input (Windows 7) certificate install ===
echo.

rem Best-effort elevation check. This intentionally refuses to run when not elevated.
net session >nul 2>&1 && goto :elevated
fltmc >nul 2>&1 && goto :elevated
fsutil dirty query %systemdrive% >nul 2>&1 && goto :elevated

echo ERROR: This script must be run as Administrator.
echo.
echo Right-click INSTALL_CERT.cmd and select "Run as administrator",
echo or run it from an elevated Command Prompt.
exit /b 1

:elevated

if not exist "aero-virtio-input-test.cer" (
  echo ERROR: aero-virtio-input-test.cer was not found in:
  echo   %CD%
  echo.
  echo Copy aero-virtio-input-test.cer into this folder and re-run.
  exit /b 1
)

where certutil >nul 2>&1
if not "%errorlevel%"=="0" (
  echo ERROR: certutil.exe not found in PATH.
  exit /b 1
)

echo Installing the certificate into LocalMachine stores:
echo   - Root
echo   - TrustedPublisher
echo.

echo Running:
echo   certutil -addstore -f Root aero-virtio-input-test.cer
certutil -addstore -f Root aero-virtio-input-test.cer
set "RC=%errorlevel%"
if not "%RC%"=="0" (
  echo ERROR: certutil failed with exit code %RC% while installing into Root.
  exit /b %RC%
)

echo.
echo Running:
echo   certutil -addstore -f TrustedPublisher aero-virtio-input-test.cer
certutil -addstore -f TrustedPublisher aero-virtio-input-test.cer
set "RC=%errorlevel%"
if not "%RC%"=="0" (
  echo ERROR: certutil failed with exit code %RC% while installing into TrustedPublisher.
  exit /b %RC%
)

echo.
echo OK: Certificate installed.
echo.
echo Next: run INSTALL_DRIVER.cmd to install the driver package.
echo.

endlocal
exit /b 0

