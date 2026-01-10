@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem stage_packaging_win7.cmd
rem
rem Copies built AeroGPU binaries from drivers/aerogpu/build/out/ into the
rem Win7 packaging folder (drivers/aerogpu/packaging/win7/) so you can run:
rem   sign_test.cmd
rem   install.cmd
rem
rem Usage (flexible):
rem   stage_packaging_win7.cmd                  -> fre x64
rem   stage_packaging_win7.cmd fre x64          -> fre x64
rem   stage_packaging_win7.cmd chk x86          -> chk x86
rem   stage_packaging_win7.cmd x64 fre          -> fre x64
rem   stage_packaging_win7.cmd x86 chk          -> chk x86
rem -----------------------------------------------------------------------------

set "ARG1=%~1"
set "ARG2=%~2"

set "VARIANT=fre"
set "ARCH=x64"

if /i "%ARG1%"=="fre" set "VARIANT=fre" & if not "%ARG2%"=="" set "ARCH=%ARG2%"
if /i "%ARG1%"=="chk" set "VARIANT=chk" & if not "%ARG2%"=="" set "ARCH=%ARG2%"
if /i "%ARG1%"=="x86" set "ARCH=x86" & if not "%ARG2%"=="" set "VARIANT=%ARG2%"
if /i "%ARG1%"=="x64" set "ARCH=x64" & if not "%ARG2%"=="" set "VARIANT=%ARG2%"

if /i not "%VARIANT%"=="fre" if /i not "%VARIANT%"=="chk" (
  echo ERROR: Unknown variant "%VARIANT%" (expected fre or chk)
  exit /b 1
)
if /i not "%ARCH%"=="x86" if /i not "%ARCH%"=="x64" (
  echo ERROR: Unknown arch "%ARCH%" (expected x86 or x64)
  exit /b 1
)

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%.") do set "SCRIPT_DIR=%%~fI"
for %%I in ("%SCRIPT_DIR%\..") do set "AEROGPU_ROOT=%%~fI"

set "OUT_ROOT=%SCRIPT_DIR%\out\win7"
set "PKG_DIR=%AEROGPU_ROOT%\packaging\win7"

set "KMD_SYS=%OUT_ROOT%\%ARCH%\%VARIANT%\kmd\aerogpu.sys"
set "UMD_X86_DIR=%OUT_ROOT%\x86\%VARIANT%\umd"
set "UMD_X64_DIR=%OUT_ROOT%\x64\%VARIANT%\umd"

if not exist "%PKG_DIR%" (
  echo ERROR: Packaging directory not found: "%PKG_DIR%"
  exit /b 1
)

if not exist "%KMD_SYS%" (
  echo ERROR: KMD output not found (did you run build_all.cmd?): "%KMD_SYS%"
  exit /b 1
)

if not exist "%UMD_X86_DIR%\aerogpu_d3d9.dll" (
  echo ERROR: D3D9 x86 UMD not found: "%UMD_X86_DIR%\aerogpu_d3d9.dll"
  exit /b 1
)

if /i "%ARCH%"=="x64" (
  if not exist "%UMD_X64_DIR%\aerogpu_d3d9_x64.dll" (
    echo ERROR: D3D9 x64 UMD not found: "%UMD_X64_DIR%\aerogpu_d3d9_x64.dll"
    exit /b 1
  )
)

echo Staging AeroGPU package (WIN7 %VARIANT% %ARCH%)
echo   from: "%OUT_ROOT%"
echo   to:   "%PKG_DIR%"
echo.

rem Clear existing binaries/cats so the package folder is always consistent.
del /f /q "%PKG_DIR%\aerogpu.sys" >nul 2>nul
del /f /q "%PKG_DIR%\aerogpu_d3d9.dll" "%PKG_DIR%\aerogpu_d3d9_x64.dll" >nul 2>nul
del /f /q "%PKG_DIR%\aerogpu_d3d10.dll" "%PKG_DIR%\aerogpu_d3d10_x64.dll" >nul 2>nul
del /f /q "%PKG_DIR%\aerogpu.cat" "%PKG_DIR%\aerogpu_dx11.cat" >nul 2>nul

copy /y "%KMD_SYS%" "%PKG_DIR%\" >nul

copy /y "%UMD_X86_DIR%\aerogpu_d3d9.dll" "%PKG_DIR%\" >nul
if exist "%UMD_X86_DIR%\aerogpu_d3d10.dll" (
  if /i "%ARCH%"=="x64" (
    if exist "%UMD_X64_DIR%\aerogpu_d3d10_x64.dll" (
      copy /y "%UMD_X86_DIR%\aerogpu_d3d10.dll" "%PKG_DIR%\" >nul
    ) else (
      echo NOTE: Skipping optional aerogpu_d3d10.dll because aerogpu_d3d10_x64.dll was not found.
    )
  ) else (
    copy /y "%UMD_X86_DIR%\aerogpu_d3d10.dll" "%PKG_DIR%\" >nul
  )
)

if /i "%ARCH%"=="x64" (
  copy /y "%UMD_X64_DIR%\aerogpu_d3d9_x64.dll" "%PKG_DIR%\" >nul
  if exist "%UMD_X64_DIR%\aerogpu_d3d10_x64.dll" (
    if exist "%UMD_X86_DIR%\aerogpu_d3d10.dll" (
      copy /y "%UMD_X64_DIR%\aerogpu_d3d10_x64.dll" "%PKG_DIR%\" >nul
    )
  )
)

echo OK: staged binaries.
echo Next (in a Win7 VM, as Administrator):
echo   cd drivers\\aerogpu\\packaging\\win7
echo   sign_test.cmd
echo   install.cmd
exit /b 0

