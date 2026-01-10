@echo off
setlocal
echo Enabling Windows test-signing mode...
echo.
echo This must be run from an elevated Administrator command prompt.
echo.
bcdedit /set testsigning on
if errorlevel 1 (
  echo.
  echo Failed to run bcdedit. Are you running as Administrator?
  exit /b 1
)
echo.
echo Test-signing enabled. Rebooting...
shutdown /r /t 0

