@echo off
setlocal

cd /d "%~dp0"

echo.
echo === Aero virtio-input (Windows 7) driver install ===
echo.

rem Best-effort elevation check (pnputil requires admin to install drivers).
net session >nul 2>&1 && goto :elevated
fltmc >nul 2>&1 && goto :elevated
fsutil dirty query %systemdrive% >nul 2>&1 && goto :elevated

echo ERROR: This script must be run as Administrator.
echo.
echo Right-click INSTALL_DRIVER.cmd and select "Run as administrator",
echo or run it from an elevated Command Prompt.
exit /b 1

:elevated

if not exist "aero_virtio_input.inf" (
  echo ERROR: aero_virtio_input.inf was not found in:
  echo   %CD%
  echo.
  echo Make sure you extracted the driver ZIP and are running this script from that folder.
  exit /b 1
)

where pnputil >nul 2>&1
if not "%errorlevel%"=="0" (
  echo ERROR: pnputil.exe not found in PATH.
  echo This script is intended to be run on a Windows 7 guest.
  exit /b 1
)

echo Running:
echo   pnputil -i -a aero_virtio_input.inf
echo.

pnputil -i -a aero_virtio_input.inf
set "RC=%errorlevel%"
echo.

if not "%RC%"=="0" (
  echo ERROR: pnputil failed with exit code %RC%.
  echo.
  echo Tips:
  echo   - Run from an elevated Command Prompt (Run as administrator).
  echo   - If the package is test-signed, run INSTALL_CERT.cmd first.
  echo   - Windows 7 x64 test-signed drivers also require Test Signing mode:
  echo       bcdedit /set testsigning on
  echo     then reboot.
  exit /b %RC%
)

echo OK: pnputil completed.
echo.
echo Next steps:
echo   1^) Reboot Windows (recommended after first install).
echo   2^) Open Device Manager and verify the virtio-input device is using this driver.
echo      - Category: Human Interface Devices
echo      - Driver file: aero_virtio_input.sys
echo.

endlocal
exit /b 0

