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

set "VARIANT=fre"
set "ARCH=x64"

if /i "%~1"=="--help" call :usage & exit /b 0
if /i "%~1"=="-h" call :usage & exit /b 0
if /i "%~1"=="/?" call :usage & exit /b 0

set "HAVE_VARIANT_ARG=0"
set "HAVE_ARCH_ARG=0"
call :apply_arg "%~1" || exit /b 1
call :apply_arg "%~2" || exit /b 1

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
if exist "%PKG_DIR%\aerogpu_d3d10.dll" (
  if /i "%ARCH%"=="x64" (
    if exist "%PKG_DIR%\aerogpu_d3d10_x64.dll" (
      echo   install.cmd aerogpu_dx11.inf
    ) else (
      echo   install.cmd
    )
  ) else (
    echo   install.cmd aerogpu_dx11.inf
  )
) else (
  echo   install.cmd
)
exit /b 0

:usage
echo Usage:
echo   stage_packaging_win7.cmd            ^(defaults to fre x64^)
echo   stage_packaging_win7.cmd fre x64
echo   stage_packaging_win7.cmd chk x86
echo.
echo Args are order-insensitive:
echo   stage_packaging_win7.cmd x64 fre
echo   stage_packaging_win7.cmd x86 chk
exit /b 0

:apply_arg
set "ARG=%~1"
if "%ARG%"=="" exit /b 0

if /i "%ARG%"=="fre" (
  if "%HAVE_VARIANT_ARG%"=="1" goto :dup_arg
  set "VARIANT=fre"
  set "HAVE_VARIANT_ARG=1"
  exit /b 0
)
if /i "%ARG%"=="chk" (
  if "%HAVE_VARIANT_ARG%"=="1" goto :dup_arg
  set "VARIANT=chk"
  set "HAVE_VARIANT_ARG=1"
  exit /b 0
)

if /i "%ARG%"=="x86" (
  if "%HAVE_ARCH_ARG%"=="1" goto :dup_arg
  set "ARCH=x86"
  set "HAVE_ARCH_ARG=1"
  exit /b 0
)
if /i "%ARG%"=="x64" (
  if "%HAVE_ARCH_ARG%"=="1" goto :dup_arg
  set "ARCH=x64"
  set "HAVE_ARCH_ARG=1"
  exit /b 0
)

echo ERROR: Unknown argument "%ARG%"
call :usage
exit /b 1

:dup_arg
echo ERROR: Conflicting arguments; only one build variant (fre/chk) and one arch (x86/x64) may be specified.
call :usage
exit /b 1
