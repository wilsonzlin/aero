@echo off
setlocal enableextensions enabledelayedexpansion

rem Install AeroGPU driver package (Windows 7 SP1)
rem
rem Usage:
rem   install.cmd [inf]
rem
rem Examples:
rem   install.cmd
rem   install.cmd aerogpu_dx11.inf

set "SCRIPT_DIR=%~dp0"
rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
set "SYS32=%SystemRoot%\System32"
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"
set "PNPUTIL=%SYS32%\pnputil.exe"
pushd "%SCRIPT_DIR%" >nul

set "INF=%~1"
if "%INF%"=="" set "INF=aerogpu.inf"

if not exist "%INF%" (
  echo [ERROR] INF not found: "%SCRIPT_DIR%%INF%"
  popd >nul
  exit /b 1
)

echo [INFO] Installing driver package via pnputil...
echo [INFO]   INF: %INF%

if not exist "%PNPUTIL%" (
  echo [ERROR] pnputil.exe not found at "%PNPUTIL%".
  popd >nul
  exit /b 1
)

"%PNPUTIL%" -i -a "%INF%"
set "PNP_ERR=%ERRORLEVEL%"

if not "%PNP_ERR%"=="0" (
  echo [WARN] pnputil returned errorlevel %PNP_ERR%.
  echo [WARN] If this is a signature error, ensure you ran sign_test.cmd and rebooted with test-signing enabled.
  echo [WARN] Attempting devcon fallback (if available)...

  call :FindDevcon
  if defined DEVCON (
    call :ExtractHwidFromInf "%INF%"
    if not defined AEROGPU_HWID (
      echo [ERROR] Could not extract PCI\VEN_... HWID from "%INF%".
      popd >nul
      exit /b %PNP_ERR%
    )

    echo [INFO] devcon: "%DEVCON%"
    echo [INFO] updating device HWID: %AEROGPU_HWID%
    "%DEVCON%" /r update "%INF%" "%AEROGPU_HWID%"
    set "PNP_ERR=%ERRORLEVEL%"
  ) else (
    echo [WARN] devcon.exe not found on PATH or alongside scripts.
  )
)

if not "%PNP_ERR%"=="0" (
  echo [ERROR] Install failed (errorlevel %PNP_ERR%).
  popd >nul
  exit /b %PNP_ERR%
)

echo [OK] Driver package installed.
echo [NOTE] A reboot is usually required after first install/update of a display driver.
popd >nul
exit /b 0

rem -----------------------------------------------------------------
rem Helper: find devcon.exe (either next to scripts, or on PATH)
rem -----------------------------------------------------------------
:FindDevcon
set "DEVCON="
if exist "%SCRIPT_DIR%devcon.exe" (
  set "DEVCON=%SCRIPT_DIR%devcon.exe"
  exit /b 0
)

for /f "delims=" %%P in ('where devcon.exe 2^>nul') do (
  set "DEVCON=%%P"
  goto :eof
)
exit /b 0

rem -----------------------------------------------------------------
rem Helper: extract first PCI HWID from INF (for devcon update)
rem -----------------------------------------------------------------
:ExtractHwidFromInf
set "AEROGPU_HWID="
for /f "usebackq tokens=1,* delims=," %%A in (`findstr /i /r ",[ ]*PCI\\VEN_[0-9A-F][0-9A-F][0-9A-F][0-9A-F].*DEV_[0-9A-F][0-9A-F][0-9A-F][0-9A-F]" "%~1"`) do (
  set "AEROGPU_HWID=%%B"
  rem Trim surrounding spaces
  for /f "tokens=* delims= " %%X in ("!AEROGPU_HWID!") do set "AEROGPU_HWID=%%X"
  goto :eof
)
exit /b 0
