@echo off
setlocal enableextensions enabledelayedexpansion

rem -----------------------------------------------------------------------------
rem verify_umd_registration.cmd
rem
rem Quick sanity check for Win7/WDDM 1.1 UMD registration + WOW64 placement.
rem
rem This script is intended to be run inside a Win7 VM after installing the
rem AeroGPU driver (aerogpu.inf or aerogpu_dx11.inf).
rem -----------------------------------------------------------------------------

set "FAIL=0"

set "REQUIRE_DX11=0"
if /i "%~1"=="dx11" set "REQUIRE_DX11=1"
if /i "%~1"=="--dx11" set "REQUIRE_DX11=1"
if /i "%~1"=="aerogpu_dx11.inf" set "REQUIRE_DX11=1"
if /i "%~1"=="aerogpu_dx11" set "REQUIRE_DX11=1"

if /i "%~1"=="--help" goto :usage
if /i "%~1"=="-h" goto :usage
if /i "%~1"=="/?" goto :usage
if not "%~1"=="" (
  if not "%REQUIRE_DX11%"=="1" (
    echo ERROR: Unknown argument: %~1
    goto :usage_fail
  )
)

set "SYS32=%SystemRoot%\System32"
rem Access real System32 when running under WoW64 (32-bit cmd.exe on 64-bit Windows).
if defined PROCESSOR_ARCHITEW6432 set "SYS32=%SystemRoot%\Sysnative"
set "REGEXE=%SYS32%\reg.exe"

echo === AeroGPU UMD registration check (Win7) ===
if "%REQUIRE_DX11%"=="1" (
  echo INFO: Mode=DX11 (require D3D10/11 UMD registration/placement)
) else (
  echo INFO: Mode=D3D9 only (D3D10/11 treated as optional)
)
echo INFO: SystemRoot=%SystemRoot%
echo INFO: PROCESSOR_ARCHITECTURE=%PROCESSOR_ARCHITECTURE%
echo INFO: PROCESSOR_ARCHITEW6432=%PROCESSOR_ARCHITEW6432%
echo.

echo --- File placement ---
set "IS_X64=0"
if /i "%PROCESSOR_ARCHITECTURE%"=="AMD64" set "IS_X64=1"
if defined PROCESSOR_ARCHITEW6432 set "IS_X64=1"

if "%IS_X64%"=="1" (
  echo INFO: Detected x64 Windows
  call :check_file_required "%SYS32%\aerogpu_d3d9_x64.dll"
  call :check_file_required "%SystemRoot%\SysWOW64\aerogpu_d3d9.dll"
  echo.
  if "%REQUIRE_DX11%"=="1" (
    call :check_file_required "%SYS32%\aerogpu_d3d10_x64.dll"
    call :check_file_required "%SystemRoot%\SysWOW64\aerogpu_d3d10.dll"
  ) else (
    echo INFO: Optional files (only if you installed aerogpu_dx11.inf)
    call :check_file_optional "%SYS32%\aerogpu_d3d10_x64.dll"
    call :check_file_optional "%SystemRoot%\SysWOW64\aerogpu_d3d10.dll"
  )
) else (
  echo INFO: Detected x86 Windows
  call :check_file_required "%SYS32%\aerogpu_d3d9.dll"
  echo.
  if "%REQUIRE_DX11%"=="1" (
    call :check_file_required "%SYS32%\aerogpu_d3d10.dll"
  ) else (
    echo INFO: Optional files (only if you installed aerogpu_dx11.inf)
    call :check_file_optional "%SYS32%\aerogpu_d3d10.dll"
  )
)
echo.

echo --- Registry (HKR device key) ---
echo INFO: D3D10/11 values are only present if you installed aerogpu_dx11.inf.
set "CLASSKEY=HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}"
echo INFO: Searching for AeroGPU adapter key under:
echo   %CLASSKEY%
echo.

set "AEROGPU_KEY="
for /f "delims=" %%K in ('"%REGEXE%" query "%CLASSKEY%" /s /f "AeroGPU Display Adapter" /d 2^>nul ^| findstr /i /r "^HKEY_"') do (
  set "AEROGPU_KEY=%%K"
  goto :found_key
)

:found_key
if not defined AEROGPU_KEY (
  echo ERROR: Could not locate the AeroGPU display adapter registry key.
  echo ERROR: Ensure the AeroGPU driver is installed and Device Manager shows "AeroGPU Display Adapter".
  exit /b 1
)

echo INFO: Found adapter key:
echo   %AEROGPU_KEY%

call :query_value InstalledDisplayDrivers
call :query_value InstalledDisplayDriversWow
call :query_value UserModeDriverName
call :query_value UserModeDriverNameWow
call :query_value FeatureScore

echo.
echo --- Validation ---
if "%IS_X64%"=="1" (
  call :require_value InstalledDisplayDrivers REG_MULTI_SZ aerogpu_d3d9_x64
  call :require_value InstalledDisplayDriversWow REG_MULTI_SZ aerogpu_d3d9
  if "%REQUIRE_DX11%"=="1" (
    call :require_value UserModeDriverName REG_SZ aerogpu_d3d10_x64.dll
    call :require_value UserModeDriverNameWow REG_SZ aerogpu_d3d10.dll
  )
) else (
  call :require_value InstalledDisplayDrivers REG_MULTI_SZ aerogpu_d3d9
  if "%REQUIRE_DX11%"=="1" (
    call :require_value UserModeDriverName REG_SZ aerogpu_d3d10.dll
  )
)

echo.
if "%FAIL%"=="0" (
  echo OK
  exit /b 0
) else (
  echo FAIL
  exit /b 1
)

rem -----------------------------------------------------------------------------
:usage
echo Usage:
echo   verify_umd_registration.cmd           ^(D3D9-only checks; D3D10/11 optional^)
echo   verify_umd_registration.cmd dx11      ^(require D3D10/11 registration/placement^)
echo.
exit /b 0

:usage_fail
call :usage
exit /b 1

rem -----------------------------------------------------------------------------
:check_file_required
set "P=%~1"
if exist "%P%" (
  echo OK:   %P%
) else (
  echo MISS (required): %P%
  set "FAIL=1"
)
exit /b 0

rem -----------------------------------------------------------------------------
:check_file_optional
set "P=%~1"
if exist "%P%" (
  echo OK:   %P%
) else (
  echo MISS (optional): %P%
)
exit /b 0

rem -----------------------------------------------------------------------------
:query_value
set "NAME=%~1"
echo.
echo [%NAME%]
"%REGEXE%" query "%AEROGPU_KEY%" /v %NAME% 2>nul
if errorlevel 1 (
  echo (not present)
)
exit /b 0

rem -----------------------------------------------------------------------------
:require_value
set "NAME=%~1"
set "TYPE=%~2"
set "EXPECT=%~3"

set "OK=1"

"%REGEXE%" query "%AEROGPU_KEY%" /v %NAME% 2>nul | findstr /i /l /c:"%TYPE%" >nul
if errorlevel 1 set "OK=0"

set "EXPECT_RE=!EXPECT:.=[.]!"
rem Match the expected value as a full token at end-of-line (avoid false positives
rem like "aerogpu_d3d9" matching "aerogpu_d3d9_x64", or accidental ".dll" suffixes
rem on InstalledDisplayDrivers values).
"%REGEXE%" query "%AEROGPU_KEY%" /v %NAME% 2>nul | findstr /i /r /c:"%EXPECT_RE%[ ]*$" >nul
if errorlevel 1 set "OK=0"

if "%OK%"=="1" (
  echo OK:   %NAME% (%TYPE%) == "%EXPECT%"
) else (
  echo ERROR: %NAME% missing or does not match expected type/data (%TYPE%, "%EXPECT%")
  set "FAIL=1"
)
exit /b 0
