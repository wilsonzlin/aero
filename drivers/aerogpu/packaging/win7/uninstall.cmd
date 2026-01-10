@echo off
setlocal enableextensions enabledelayedexpansion

rem Uninstall AeroGPU driver package (Windows 7 SP1)
rem
rem Notes:
rem   - Display driver packages may be in use. If pnputil -d fails with "in use",
rem     reboot into Safe Mode, or first switch the device back to a different
rem     driver (e.g. Standard VGA) and then rerun uninstall.cmd.

set "SCRIPT_DIR=%~dp0"
rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"
set "PNPUTIL=%SYS32%\pnputil.exe"
pushd "%SCRIPT_DIR%" >nul

set "PROVIDER=AeroGPU"

if not exist "%PNPUTIL%" (
  echo [ERROR] pnputil.exe not found at "%PNPUTIL%".
  popd >nul
  exit /b 1
)

echo [INFO] Searching for installed driver packages with provider "%PROVIDER%"...

set "TMP=%TEMP%\aerogpu_pnputil_%RANDOM%.txt"
"%PNPUTIL%" -e > "%TMP%"

set "CURRENT_OEM="
set "DELETED_ANY=0"

for /f "usebackq delims=" %%L in ("%TMP%") do (
  set "LINE=%%L"

  echo !LINE! | findstr /i /c:"Published name" >nul
  if not errorlevel 1 (
    for /f "tokens=1,* delims=:" %%A in ("!LINE!") do (
      set "CURRENT_OEM=%%B"
      for /f "tokens=* delims= " %%X in ("!CURRENT_OEM!") do set "CURRENT_OEM=%%X"
    )
  )

  echo !LINE! | findstr /i /c:"Driver package provider" >nul
  if not errorlevel 1 (
    echo !LINE! | findstr /i /c:"%PROVIDER%" >nul
    if not errorlevel 1 (
      if defined CURRENT_OEM (
        echo [INFO] Deleting driver package: !CURRENT_OEM!
        "%PNPUTIL%" -d !CURRENT_OEM!
        if "%ERRORLEVEL%"=="0" set "DELETED_ANY=1"
      )
    )
  )
)

del "%TMP%" >nul 2>&1

if "%DELETED_ANY%"=="0" (
  echo [WARN] No matching AeroGPU driver packages found (or deletion failed).
  echo [WARN] If the driver is currently active, pnputil may refuse to delete it.
  popd >nul
  exit /b 1
)

echo [OK] Driver package(s) deleted.
echo [NOTE] You may need to reboot for the system to fully revert the active display driver.
popd >nul
exit /b 0
