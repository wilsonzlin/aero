@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Generates driver catalogs for Windows 7 (x86 + x64) using Inf2Cat.
rem
rem Usage:
rem   make-cat.cmd [contract|legacy|all]
rem
rem Variants:
rem   - contract (default): aero-virtio-snd.inf (+ optional virtio-snd.inf alias) -> virtiosnd.sys
rem   - legacy:             aero-virtio-snd-legacy.inf -> virtiosnd_legacy.sys
rem   - all:                generate all catalogs (requires both SYS files; alias INF optional)
rem
rem Notes:
rem   - Inf2Cat hashes every file referenced by each INF, so the referenced SYS
rem     must exist next to the INF under inf\ before running this script.
rem   - virtio-snd.inf is a legacy filename alias checked in as virtio-snd.inf.disabled.
rem     Rename it to virtio-snd.inf if you need the alias.

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf

set CONTRACT_INF=%INF_DIR%\aero-virtio-snd.inf
set CONTRACT_CAT=%INF_DIR%\aero-virtio-snd.cat
set CONTRACT_SYS=%INF_DIR%\virtiosnd.sys

set TRANS_INF=%INF_DIR%\aero-virtio-snd-legacy.inf
set TRANS_CAT=%INF_DIR%\aero-virtio-snd-legacy.cat
set TRANS_SYS=%INF_DIR%\virtiosnd_legacy.sys

set ALIAS_INF=%INF_DIR%\virtio-snd.inf
set ALIAS_INF_DISABLED=%INF_DIR%\virtio-snd.inf.disabled
set ALIAS_CAT=%INF_DIR%\virtio-snd.cat

set VARIANT=%~1
if "%VARIANT%"=="" set VARIANT=contract

if /I "%VARIANT%"=="contract" goto :variant_ok
if /I "%VARIANT%"=="legacy" goto :variant_ok
if /I "%VARIANT%"=="all" goto :variant_ok

echo ERROR: Unknown variant "%VARIANT%".
echo Usage: make-cat.cmd [contract^|legacy^|all]
exit /b 1

:variant_ok

if /I "%VARIANT%"=="contract" (
  if not exist "%CONTRACT_INF%" (
    echo ERROR: Contract INF not found: "%CONTRACT_INF%"
    exit /b 1
  )
  if not exist "%CONTRACT_SYS%" (
    echo ERROR: Driver binary not found: "%CONTRACT_SYS%"
    echo        Build the driver and copy virtiosnd.sys into the inf\ directory.
    exit /b 1
  )
  if not exist "%ALIAS_INF%" if exist "%ALIAS_INF_DISABLED%" (
    rem Friendly hint only; the alias INF is optional.
    echo NOTE: Alias INF is disabled by default. To enable, rename:
    echo         "%ALIAS_INF_DISABLED%"
    echo       to:
    echo         "%ALIAS_INF%"
  )
) else if /I "%VARIANT%"=="legacy" (
  if not exist "%TRANS_INF%" (
    echo ERROR: Legacy INF not found: "%TRANS_INF%"
    exit /b 1
  )
  if not exist "%TRANS_SYS%" (
    echo ERROR: Driver binary not found: "%TRANS_SYS%"
    echo        Build the driver (Configuration=Legacy) and copy virtiosnd_legacy.sys into the inf\ directory.
    exit /b 1
  )
) else (
  rem all
  if not exist "%CONTRACT_INF%" (
    echo ERROR: Contract INF not found: "%CONTRACT_INF%"
    exit /b 1
  )
  if not exist "%TRANS_INF%" (
    echo ERROR: Legacy INF not found: "%TRANS_INF%"
    exit /b 1
  )
  if not exist "%CONTRACT_SYS%" (
    echo ERROR: Driver binary not found: "%CONTRACT_SYS%"
    exit /b 1
  )
  if not exist "%TRANS_SYS%" (
    echo ERROR: Driver binary not found: "%TRANS_SYS%"
    exit /b 1
  )
)

where Inf2Cat.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: Inf2Cat.exe not found in PATH.
  echo        Install WinDDK 7600 or WDK 10 and run this from a WDK Developer Command Prompt.
  exit /b 1
)

set TMP_DIR=%TEMP%\aero-virtio-snd-inf2cat-%RANDOM%%RANDOM%
mkdir "%TMP_DIR%" >nul 2>nul
if errorlevel 1 (
  echo ERROR: Failed to create temp directory: "%TMP_DIR%"
  exit /b 1
)

rem Delete any existing catalogs first so the post-Inf2Cat existence checks
rem cannot be satisfied by stale artifacts from a previous run.
if exist "%CONTRACT_CAT%" del /f /q "%CONTRACT_CAT%" >nul 2>nul
if exist "%TRANS_CAT%" del /f /q "%TRANS_CAT%" >nul 2>nul
if exist "%ALIAS_CAT%" del /f /q "%ALIAS_CAT%" >nul 2>nul

echo.
echo == Inf2Cat (Windows 7 x86 + x64) ==
echo Variant: "%VARIANT%"
echo Temp dir: "%TMP_DIR%"
echo.

rem Stage only the selected payload into a temp dir so Inf2Cat doesn't require
rem all optional package files to be present under inf\.
if /I "%VARIANT%"=="contract" (
  copy /y "%CONTRACT_SYS%" "%TMP_DIR%\virtiosnd.sys" >nul
  copy /y "%CONTRACT_INF%" "%TMP_DIR%\aero-virtio-snd.inf" >nul
  if exist "%ALIAS_INF%" (
    copy /y "%ALIAS_INF%" "%TMP_DIR%\virtio-snd.inf" >nul
  )
) else if /I "%VARIANT%"=="legacy" (
  copy /y "%TRANS_SYS%" "%TMP_DIR%\virtiosnd_legacy.sys" >nul
  copy /y "%TRANS_INF%" "%TMP_DIR%\aero-virtio-snd-legacy.inf" >nul
) else (
  rem all
  copy /y "%CONTRACT_SYS%" "%TMP_DIR%\virtiosnd.sys" >nul
  copy /y "%TRANS_SYS%" "%TMP_DIR%\virtiosnd_legacy.sys" >nul
  copy /y "%CONTRACT_INF%" "%TMP_DIR%\aero-virtio-snd.inf" >nul
  copy /y "%TRANS_INF%" "%TMP_DIR%\aero-virtio-snd-legacy.inf" >nul
  if exist "%ALIAS_INF%" (
    copy /y "%ALIAS_INF%" "%TMP_DIR%\virtio-snd.inf" >nul
  )
)

Inf2Cat.exe /driver:"%TMP_DIR%" /os:7_X86,7_X64 /verbose
if errorlevel 1 (
  echo.
  echo ERROR: Inf2Cat failed.
  echo NOTE: Ensure the selected SYS/INF files are present and consistent.
  rmdir /s /q "%TMP_DIR%" >nul 2>nul
  exit /b 1
)

rem Copy generated catalog(s) back into inf\.
set MISSING_CAT=0

if /I "%VARIANT%"=="contract" (
  if not exist "%TMP_DIR%\aero-virtio-snd.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd.cat" "%CONTRACT_CAT%" >nul
  )
  if exist "%ALIAS_INF%" (
    if not exist "%TMP_DIR%\virtio-snd.cat" (
      echo ERROR: Expected catalog not found: "%TMP_DIR%\virtio-snd.cat"
      set MISSING_CAT=1
    ) else (
      copy /y "%TMP_DIR%\virtio-snd.cat" "%ALIAS_CAT%" >nul
    )
  )
) else if /I "%VARIANT%"=="legacy" (
  if not exist "%TMP_DIR%\aero-virtio-snd-legacy.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd-legacy.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd-legacy.cat" "%TRANS_CAT%" >nul
  )
) else (
  rem all
  if not exist "%TMP_DIR%\aero-virtio-snd.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd.cat" "%CONTRACT_CAT%" >nul
  )
  if not exist "%TMP_DIR%\aero-virtio-snd-legacy.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd-legacy.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd-legacy.cat" "%TRANS_CAT%" >nul
  )
  if exist "%ALIAS_INF%" (
    if not exist "%TMP_DIR%\virtio-snd.cat" (
      echo ERROR: Expected catalog not found: "%TMP_DIR%\virtio-snd.cat"
      set MISSING_CAT=1
    ) else (
      copy /y "%TMP_DIR%\virtio-snd.cat" "%ALIAS_CAT%" >nul
    )
  )
)

rmdir /s /q "%TMP_DIR%" >nul 2>nul

if "%MISSING_CAT%"=="1" exit /b 1

echo.
echo OK: Created catalog(s):
if exist "%CONTRACT_CAT%" echo   - "%CONTRACT_CAT%"
if exist "%TRANS_CAT%" echo   - "%TRANS_CAT%"
if exist "%ALIAS_CAT%" echo   - "%ALIAS_CAT%"
exit /b 0

