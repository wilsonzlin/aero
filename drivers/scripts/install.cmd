@echo off
setlocal EnableExtensions

rem Installs Aero Win7 virtio driver pack using pnputil.
rem Must be run as Administrator.

set "ROOT=%~dp0"
set "ARCH=%PROCESSOR_ARCHITECTURE%"
if /I "%PROCESSOR_ARCHITEW6432%"=="AMD64" set "ARCH=AMD64"

if /I "%ARCH%"=="AMD64" (
  set "PACK_ARCH=amd64"
) else (
  set "PACK_ARCH=x86"
)

set "DRIVER_ROOT=%ROOT%win7\%PACK_ARCH%"

if not exist "%DRIVER_ROOT%\" (
  echo Expected driver directory not found: "%DRIVER_ROOT%"
  echo Ensure you extracted the driver pack ZIP and are running install.cmd from its root.
  exit /b 1
)

echo Installing Aero Win7 virtio drivers for architecture: %PACK_ARCH%
echo Driver root: "%DRIVER_ROOT%"
echo.

for /r "%DRIVER_ROOT%" %%F in (*.inf) do (
  echo pnputil -i -a "%%F"
  pnputil -i -a "%%F"
)

echo.
echo Done. You may need to reboot for all devices to start.
pause
