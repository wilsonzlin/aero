@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem AeroGPU Win7 build orchestrator (KMD+UMD via MSBuild + WDK 10 toolset)
rem
rem Usage:
rem   build_all.cmd                 -> build fre+chk, x86+x64
rem   build_all.cmd fre             -> build fre only, x86+x64
rem   build_all.cmd chk x86         -> build chk only, x86 only
rem   build_all.cmd all x64         -> build fre+chk, x64 KMD + x86/x64 UMDs (needed for Win7 x64)
rem
rem Args are order-insensitive:
rem   build_all.cmd x64 fre         -> same as: build_all.cmd fre x64
rem -----------------------------------------------------------------------------

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%.") do set "SCRIPT_DIR=%%~fI"

for %%I in ("%SCRIPT_DIR%\..") do set "AEROGPU_ROOT=%%~fI"
set "KMD_DIR=%AEROGPU_ROOT%\kmd"
set "KMD_PROJ=%AEROGPU_ROOT%\aerogpu_kmd.vcxproj"
set "UMD_DIR=%AEROGPU_ROOT%\umd"
set "UMD_D3D9_DIR=%AEROGPU_ROOT%\umd\d3d9"
set "UMD_D3D9_PROJ=%UMD_D3D9_DIR%\aerogpu_d3d9_umd.vcxproj"
set "UMD_D3D10_11_DIR=%AEROGPU_ROOT%\umd\d3d10_11"
set "UMD_D3D10_11_SLN=%UMD_D3D10_11_DIR%\aerogpu_d3d10_11.sln"

set "OUT_ROOT=%SCRIPT_DIR%\out"

rem Optional WDK 7.1 root (used only for D3D10/11 UMD DDI headers).
rem If not found, the D3D10/11 UMD builds in the repo-local/stub header mode.
set "WDKROOT="
if defined WINDDK set "WDKROOT=%WINDDK%"
if not defined WDKROOT if defined WDK_ROOT set "WDKROOT=%WDK_ROOT%"
if not defined WDKROOT (
  if exist "C:\WinDDK\7600.16385.1\inc\ddk\d3d10umddi.h" set "WDKROOT=C:\WinDDK\7600.16385.1"
  if exist "C:\WinDDK\7600.16385.1\inc\api\d3d10umddi.h" set "WDKROOT=C:\WinDDK\7600.16385.1"
)

set "D3D10_11_WDK_MSBUILD_ARGS="
if defined WDKROOT (
  set "D3D10_11_WDK_MSBUILD_ARGS=/p:AeroGpuUseWdkHeaders=1 /p:AeroGpuWdkRoot=""%WDKROOT%"""
)

set "VARIANTS=fre chk"
set "ARCHES=x86 x64"

if /i "%~1"=="--help" call :usage & exit /b 0
if /i "%~1"=="-h" call :usage & exit /b 0
if /i "%~1"=="/?" call :usage & exit /b 0

set "HAVE_VARIANT_ARG=0"
set "HAVE_ARCH_ARG=0"
call :apply_arg "%~1" || exit /b 1
call :apply_arg "%~2" || exit /b 1

set "HAVE_X64=0"
echo %ARCHES% | findstr /i "x64" >nul 2>nul && set "HAVE_X64=1"
if not exist "%KMD_DIR%" (
  echo ERROR: Expected KMD directory not found: "%KMD_DIR%"
  echo        The KMD task should populate drivers\aerogpu\kmd\
  exit /b 1
)
if not exist "%KMD_PROJ%" (
  echo ERROR: Expected KMD MSBuild project not found:
  echo        "%KMD_PROJ%"
  exit /b 1
)
if not exist "%UMD_DIR%" (
  echo ERROR: Expected UMD directory not found: "%AEROGPU_ROOT%\umd"
  echo        The UMD task should populate drivers\aerogpu\umd\
  exit /b 1
)

if not exist "%UMD_D3D9_PROJ%" (
  echo ERROR: Expected D3D9 UMD project not found:
  echo        "%UMD_D3D9_PROJ%"
  exit /b 1
)

set "HAVE_D3D10_11=0"
if exist "%UMD_D3D10_11_SLN%" (
  set "HAVE_D3D10_11=1"
)
if "%HAVE_D3D10_11%"=="1" (
  rem The D3D10/11 UMD is compiled against the Win7-era D3D DDI headers.
  rem Provide WDKROOT (typically WDK 7.1: C:\WinDDK\7600.16385.1) so MSBuild
  rem can find d3d10umddi.h/d3d11umddi.h.
  call :ensure_wdk_root
  if errorlevel 1 exit /b 1
)
if "%HAVE_D3D10_11%"=="0" (
  echo NOTE: Optional D3D10/11 UMD not found; skipping:
  echo       "%UMD_D3D10_11_SLN%"
)

echo Output dir: "%OUT_ROOT%"
echo.

for %%V in (%VARIANTS%) do (
  echo ===========================================================================
  echo Building WIN7 %%V
  echo ===========================================================================

  rem KMD: build only the requested arches.
  for %%A in (%ARCHES%) do (
    echo [KMD] %%A
    call "%SCRIPT_DIR%\build_umd.cmd" "%%V" "%%A" "%KMD_PROJ%" "%OUT_ROOT%\win7\%%A\%%V\kmd" "%OUT_ROOT%\win7\%%A\%%V\kmd\obj" "aerogpu.sys"
    if errorlevel 1 exit /b 1
  )

  rem UMDs: for Win7 x64 installs, we need both x64 + WOW64 (x86) UMDs.
  echo [UMD] x86
  call :build_umd %%V x86
  if errorlevel 1 exit /b 1

  if "%HAVE_X64%"=="1" (
    echo [UMD] x64
    call :build_umd %%V x64
    if errorlevel 1 exit /b 1
  )

  echo.
)

echo Done.
exit /b 0

:usage
echo Usage:
echo   build_all.cmd                 ^(build fre+chk, x86+x64^)
echo   build_all.cmd fre             ^(fre only, x86+x64^)
echo   build_all.cmd chk x86         ^(chk only, x86 only^)
echo   build_all.cmd all x64         ^(fre+chk, x64 KMD + x86/x64 UMDs^)
echo.
echo Args are order-insensitive:
echo   build_all.cmd x64 fre         ^(same as: build_all.cmd fre x64^)
exit /b 0

:apply_arg
set "ARG=%~1"
if "%ARG%"=="" exit /b 0

if /i "%ARG%"=="fre" (
  if "%HAVE_VARIANT_ARG%"=="1" goto :dup_arg
  set "VARIANTS=fre"
  set "HAVE_VARIANT_ARG=1"
  exit /b 0
)
if /i "%ARG%"=="chk" (
  if "%HAVE_VARIANT_ARG%"=="1" goto :dup_arg
  set "VARIANTS=chk"
  set "HAVE_VARIANT_ARG=1"
  exit /b 0
)
if /i "%ARG%"=="all" (
  if "%HAVE_VARIANT_ARG%"=="1" goto :dup_arg
  set "VARIANTS=fre chk"
  set "HAVE_VARIANT_ARG=1"
  exit /b 0
)

if /i "%ARG%"=="x86" (
  if "%HAVE_ARCH_ARG%"=="1" goto :dup_arg
  set "ARCHES=x86"
  set "HAVE_ARCH_ARG=1"
  exit /b 0
)
if /i "%ARG%"=="x64" (
  if "%HAVE_ARCH_ARG%"=="1" goto :dup_arg
  set "ARCHES=x64"
  set "HAVE_ARCH_ARG=1"
  exit /b 0
)

echo ERROR: Unknown argument "%ARG%"
call :usage
exit /b 1

:dup_arg
echo ERROR: Conflicting arguments; only one build variant (fre/chk/all) and one arch (x86/x64) may be specified.
call :usage
exit /b 1

:build_umd
setlocal EnableExtensions EnableDelayedExpansion

set "VARIANT=%~1"
set "ARCH=%~2"

set "UMD_OUT_DIR=%OUT_ROOT%\win7\%ARCH%\%VARIANT%\umd"
if exist "!UMD_OUT_DIR!" rmdir /s /q "!UMD_OUT_DIR!"
mkdir "!UMD_OUT_DIR!" >nul 2>nul

set "D3D9_DLL="
if /i "%ARCH%"=="x86" set "D3D9_DLL=aerogpu_d3d9.dll"
if /i "%ARCH%"=="x64" set "D3D9_DLL=aerogpu_d3d9_x64.dll"

call "%SCRIPT_DIR%\build_umd.cmd" "%VARIANT%" "%ARCH%" "%UMD_D3D9_PROJ%" "!UMD_OUT_DIR!" "!UMD_OUT_DIR!\obj\d3d9" "!D3D9_DLL!"
if errorlevel 1 (
  endlocal & exit /b 1
)

  if "%HAVE_D3D10_11%"=="1" (
    set "D3D10_DLL="
    if /i "%ARCH%"=="x86" set "D3D10_DLL=aerogpu_d3d10.dll"
    if /i "%ARCH%"=="x64" set "D3D10_DLL=aerogpu_d3d10_x64.dll"

    call "%SCRIPT_DIR%\build_umd.cmd" "%VARIANT%" "%ARCH%" "%UMD_D3D10_11_SLN%" "!UMD_OUT_DIR!" "!UMD_OUT_DIR!\obj\d3d10_11" "!D3D10_DLL!" !D3D10_11_WDK_MSBUILD_ARGS!
    if errorlevel 1 (
      endlocal & exit /b 1
    )
  )

endlocal & exit /b 0

rem -----------------------------------------------------------------------------
rem :ensure_wdk_root
rem
rem The D3D10/11 UMD builds against Win7-era D3D DDI headers (d3d10umddi.h /
rem d3d11umddi.h). These are most readily available via WDK 7.1 (7600).
rem Set WDKROOT (and WINDDK for compatibility) if not already configured.
rem -----------------------------------------------------------------------------
:ensure_wdk_root
if defined WDKROOT goto :wdk_ok
if defined WINDDK set "WDKROOT=%WINDDK%" & goto :wdk_ok
if defined WDK_ROOT set "WDKROOT=%WDK_ROOT%" & goto :wdk_ok

if exist "C:\WinDDK\7600.16385.1\inc\api\d3d10umddi.h" (
  set "WDKROOT=C:\WinDDK\7600.16385.1"
  goto :wdk_ok
)

echo ERROR: WDKROOT not set and WDK 7.1 headers were not found.
echo        Set WDKROOT (or WINDDK) to your WDK 7.1 root, e.g.:
echo          set WDKROOT=C:\WinDDK\7600.16385.1
exit /b 1

:wdk_ok
if not defined WINDDK set "WINDDK=%WDKROOT%"
exit /b 0


