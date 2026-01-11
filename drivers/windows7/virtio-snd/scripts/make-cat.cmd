@rem SPDX-License-Identifier: MIT OR Apache-2.0
@echo off
setlocal EnableExtensions

rem Generates driver catalogs for Windows 7 (x86 + x64) using Inf2Cat.
rem
rem Usage:
rem   make-cat.cmd [contract|legacy|all]
rem
rem Variants:
rem   - contract (default): aero_virtio_snd.inf -> aero_virtio_snd.sys
rem   - legacy:             aero-virtio-snd-legacy.inf -> virtiosnd_legacy.sys
rem   - all:                generate both catalogs (requires both SYS files)
rem
rem Notes:
rem   - Inf2Cat hashes every file referenced by each INF, so the referenced SYS
rem     must exist next to the INF under inf\ before running this script.
rem   - A legacy filename alias INF exists as inf\virtio-snd.inf.disabled.
rem     If you need it, rename it to virtio-snd.inf (but do not ship both INFs).

set SCRIPT_DIR=%~dp0
for %%I in ("%SCRIPT_DIR%..") do set ROOT_DIR=%%~fI
set INF_DIR=%ROOT_DIR%\inf

set CONTRACT_INF=%INF_DIR%\aero_virtio_snd.inf
set CONTRACT_SYS=%INF_DIR%\aero_virtio_snd.sys
set CONTRACT_CAT=%INF_DIR%\aero_virtio_snd.cat

set LEGACY_INF=%INF_DIR%\aero-virtio-snd-legacy.inf
set LEGACY_SYS=%INF_DIR%\virtiosnd_legacy.sys
set LEGACY_CAT=%INF_DIR%\aero-virtio-snd-legacy.cat

set ALIAS_INF=%INF_DIR%\virtio-snd.inf
set ALIAS_INF_DISABLED=%INF_DIR%\virtio-snd.inf.disabled

set VARIANT=%~1
if "%VARIANT%"=="" set VARIANT=contract

if /I "%VARIANT%"=="contract" goto :variant_ok
if /I "%VARIANT%"=="legacy" goto :variant_ok
if /I "%VARIANT%"=="all" goto :variant_ok

echo ERROR: Unknown variant "%VARIANT%".
echo Usage: make-cat.cmd [contract^|legacy^|all]
exit /b 1

:variant_ok

set CONTRACT_INF_SELECTED=
if exist "%CONTRACT_INF%" (
  set CONTRACT_INF_SELECTED=%CONTRACT_INF%
) else if exist "%ALIAS_INF%" (
  set CONTRACT_INF_SELECTED=%ALIAS_INF%
)

if /I "%VARIANT%"=="contract" (
  if "%CONTRACT_INF_SELECTED%"=="" (
    echo ERROR: Contract INF not found. Expected one of:
    echo   - "%CONTRACT_INF%"
    echo   - "%ALIAS_INF%"
    exit /b 1
  )
  if not exist "%CONTRACT_SYS%" (
    echo ERROR: Driver binary not found: "%CONTRACT_SYS%"
    echo        Build the driver and copy aero_virtio_snd.sys into the inf\ directory.
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
  if not exist "%LEGACY_INF%" (
    echo ERROR: Legacy INF not found: "%LEGACY_INF%"
    exit /b 1
  )
  if not exist "%LEGACY_SYS%" (
    echo ERROR: Driver binary not found: "%LEGACY_SYS%"
    echo        Build the driver (Configuration=Legacy) and copy virtiosnd_legacy.sys into the inf\ directory.
    exit /b 1
  )
) else (
  rem all
  if "%CONTRACT_INF_SELECTED%"=="" (
    echo ERROR: Contract INF not found. Expected one of:
    echo   - "%CONTRACT_INF%"
    echo   - "%ALIAS_INF%"
    exit /b 1
  )
  if not exist "%LEGACY_INF%" (
    echo ERROR: Legacy INF not found: "%LEGACY_INF%"
    exit /b 1
  )
  if not exist "%CONTRACT_SYS%" (
    echo ERROR: Driver binary not found: "%CONTRACT_SYS%"
    exit /b 1
  )
  if not exist "%LEGACY_SYS%" (
    echo ERROR: Driver binary not found: "%LEGACY_SYS%"
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
if exist "%LEGACY_CAT%" del /f /q "%LEGACY_CAT%" >nul 2>nul

echo.
echo == Inf2Cat (Windows 7 x86 + x64) ==
echo Variant: "%VARIANT%"
echo Temp dir: "%TMP_DIR%"
echo.

rem Stage only the selected payload into a temp dir so Inf2Cat doesn't require
rem all optional package files to be present under inf\.
if /I "%VARIANT%"=="contract" (
  copy /y "%CONTRACT_SYS%" "%TMP_DIR%\aero_virtio_snd.sys" >nul
  for %%I in ("%CONTRACT_INF_SELECTED%") do copy /y "%%~fI" "%TMP_DIR%\%%~nxI" >nul
) else if /I "%VARIANT%"=="legacy" (
  copy /y "%LEGACY_SYS%" "%TMP_DIR%\virtiosnd_legacy.sys" >nul
  copy /y "%LEGACY_INF%" "%TMP_DIR%\aero-virtio-snd-legacy.inf" >nul
) else (
  rem all
  copy /y "%CONTRACT_SYS%" "%TMP_DIR%\aero_virtio_snd.sys" >nul
  copy /y "%LEGACY_SYS%" "%TMP_DIR%\virtiosnd_legacy.sys" >nul
  for %%I in ("%CONTRACT_INF_SELECTED%") do copy /y "%%~fI" "%TMP_DIR%\%%~nxI" >nul
  copy /y "%LEGACY_INF%" "%TMP_DIR%\aero-virtio-snd-legacy.inf" >nul
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
  if not exist "%TMP_DIR%\aero_virtio_snd.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero_virtio_snd.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero_virtio_snd.cat" "%CONTRACT_CAT%" >nul
  )
) else if /I "%VARIANT%"=="legacy" (
  if not exist "%TMP_DIR%\aero-virtio-snd-legacy.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd-legacy.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd-legacy.cat" "%LEGACY_CAT%" >nul
  )
) else (
  rem all
  if not exist "%TMP_DIR%\aero_virtio_snd.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero_virtio_snd.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero_virtio_snd.cat" "%CONTRACT_CAT%" >nul
  )
  if not exist "%TMP_DIR%\aero-virtio-snd-legacy.cat" (
    echo ERROR: Expected catalog not found: "%TMP_DIR%\aero-virtio-snd-legacy.cat"
    set MISSING_CAT=1
  ) else (
    copy /y "%TMP_DIR%\aero-virtio-snd-legacy.cat" "%LEGACY_CAT%" >nul
  )
)

rmdir /s /q "%TMP_DIR%" >nul 2>nul

if "%MISSING_CAT%"=="1" exit /b 1

echo.
echo OK: Created catalog(s):
if exist "%CONTRACT_CAT%" echo   - "%CONTRACT_CAT%"
if exist "%LEGACY_CAT%" echo   - "%LEGACY_CAT%"
exit /b 0
