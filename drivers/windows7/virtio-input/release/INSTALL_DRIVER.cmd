@echo off
setlocal EnableDelayedExpansion

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

rem Determine the package arch suffix (x86/amd64) from the running OS.
set "ARCH=%PROCESSOR_ARCHITECTURE%"
if /I "%PROCESSOR_ARCHITEW6432%"=="AMD64" set "ARCH=AMD64"
if /I "%ARCH%"=="AMD64" (
  set "PACK_ARCH=amd64"
) else (
  set "PACK_ARCH=x86"
)

set "INF=aero_virtio_input.inf"
if not exist "%INF%" set "INF=aero_virtio_input-%PACK_ARCH%.inf"
if exist "%INF%" goto :have_inf

echo ERROR: Could not find an INF file to install in:
echo   %CD%
echo.
echo Expected one of:
echo   - aero_virtio_input.inf
echo   - aero_virtio_input-x86.inf
echo   - aero_virtio_input-amd64.inf
echo.
echo Make sure you extracted the driver ZIP and are running this script from that folder.
exit /b 1

:have_inf

where pnputil >nul 2>&1
if not "%errorlevel%"=="0" (
  echo ERROR: pnputil.exe not found in PATH.
  echo This script is intended to be run on a Windows 7 guest.
  exit /b 1
)

echo Running:
echo   pnputil -i -a "%INF%"
echo.

pnputil -i -a "%INF%"
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

set "TABLET_INF=aero_virtio_tablet.inf"
if not exist "%TABLET_INF%" set "TABLET_INF=aero_virtio_tablet-%PACK_ARCH%.inf"

if exist "%TABLET_INF%" (
  echo Running:
  echo   pnputil -i -a "%TABLET_INF%"
  echo.

  pnputil -i -a "%TABLET_INF%"
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
) else (
  echo NOTE: aero_virtio_tablet*.inf not found; tablet / absolute-pointer virtio-input devices will not bind.
  echo.
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

