@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem AeroGPU Win7 build orchestrator (KMD via WDK 7.1 BUILD + UMD via MSBuild)
rem
rem Usage:
rem   build_all.cmd                 -> build fre+chk, x86+x64
rem   build_all.cmd fre             -> build fre only, x86+x64
rem   build_all.cmd chk x86         -> build chk only, x86 only
rem   build_all.cmd all x64         -> build fre+chk, x64 only
rem -----------------------------------------------------------------------------

set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%.") do set "SCRIPT_DIR=%%~fI"

for %%I in ("%SCRIPT_DIR%\..") do set "AEROGPU_ROOT=%%~fI"
set "KMD_DIR=%AEROGPU_ROOT%\kmd"
set "UMD_DIR=%AEROGPU_ROOT%\umd"
set "UMD_D3D9_DIR=%AEROGPU_ROOT%\umd\d3d9"
set "UMD_D3D9_PROJ=%UMD_D3D9_DIR%\aerogpu_d3d9_umd.vcxproj"
set "UMD_D3D10_11_DIR=%AEROGPU_ROOT%\umd\d3d10_11"
set "UMD_D3D10_11_SLN=%UMD_D3D10_11_DIR%\aerogpu_d3d10_11.sln"

set "OUT_ROOT=%SCRIPT_DIR%\out"

set "VARIANTS=fre chk"
set "ARCHES=x86 x64"

if /i "%~1"=="fre" set "VARIANTS=fre"
if /i "%~1"=="chk" set "VARIANTS=chk"
if /i "%~1"=="all" set "VARIANTS=fre chk"

if /i "%~2"=="x86" set "ARCHES=x86"
if /i "%~2"=="x64" set "ARCHES=x64"

call "%SCRIPT_DIR%\build_one.cmd" --selftest >nul 2>nul
if errorlevel 1 (
  echo ERROR: build_one.cmd self-test failed. Check line endings / repo checkout settings.
  exit /b 1
)

call :find_wdk_root
if errorlevel 1 exit /b 1

if not exist "%KMD_DIR%" (
  echo ERROR: Expected KMD directory not found: "%KMD_DIR%"
  echo        The KMD task should populate drivers\aerogpu\kmd\
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
if "%HAVE_D3D10_11%"=="0" (
  echo NOTE: Optional D3D10/11 UMD not found; skipping:
  echo       "%UMD_D3D10_11_SLN%"
)

echo Using WDK: "%WDKROOT%"
echo Output dir: "%OUT_ROOT%"
echo.

for %%V in (%VARIANTS%) do (
  for %%A in (%ARCHES%) do (
    echo ===========================================================================
    echo Building WIN7 %%V %%A
    echo ===========================================================================

    call "%SCRIPT_DIR%\build_one.cmd" "%WDKROOT%" WIN7 %%V %%A "%KMD_DIR%" "%OUT_ROOT%\win7\%%A\%%V\kmd" sys
    if errorlevel 1 exit /b 1

    set "UMD_OUT_DIR=%OUT_ROOT%\win7\%%A\%%V\umd"
    if exist "!UMD_OUT_DIR!" rmdir /s /q "!UMD_OUT_DIR!"
    mkdir "!UMD_OUT_DIR!" >nul 2>nul

    set "D3D9_DLL="
    if /i "%%A"=="x86" set "D3D9_DLL=aerogpu_d3d9.dll"
    if /i "%%A"=="x64" set "D3D9_DLL=aerogpu_d3d9_x64.dll"

    call "%SCRIPT_DIR%\build_umd.cmd" %%V %%A "%UMD_D3D9_PROJ%" "!UMD_OUT_DIR!" "!UMD_OUT_DIR!\obj\d3d9" "!D3D9_DLL!"
    if errorlevel 1 exit /b 1

    if "%HAVE_D3D10_11%"=="1" (
      set "D3D10_DLL="
      if /i "%%A"=="x86" set "D3D10_DLL=aerogpu_d3d10.dll"
      if /i "%%A"=="x64" set "D3D10_DLL=aerogpu_d3d10_x64.dll"

      call "%SCRIPT_DIR%\build_umd.cmd" %%V %%A "%UMD_D3D10_11_SLN%" "!UMD_OUT_DIR!" "!UMD_OUT_DIR!\obj\d3d10_11" "!D3D10_DLL!"
      if errorlevel 1 exit /b 1
    )

    echo.
  )
)

echo Done.
exit /b 0

:find_wdk_root
set "WDKROOT="

if defined WINDDK set "WDKROOT=%WINDDK%"
if not defined WDKROOT if defined WDK_ROOT set "WDKROOT=%WDK_ROOT%"

if not defined WDKROOT (
  if exist "C:\WinDDK\7600.16385.1\bin\setenv.cmd" set "WDKROOT=C:\WinDDK\7600.16385.1"
  if exist "C:\WinDDK\7600.16385.1\bin\setenv.bat" set "WDKROOT=C:\WinDDK\7600.16385.1"
)

if not defined WDKROOT (
  echo ERROR: Could not locate WDK 7.1.
  echo        Set WINDDK to your WDK root, e.g.:
  echo          set WINDDK=C:\WinDDK\7600.16385.1
  exit /b 1
)

if not exist "%WDKROOT%\bin\setenv.cmd" if not exist "%WDKROOT%\bin\setenv.bat" (
  echo ERROR: WDK root does not look valid (missing bin\setenv.cmd/.bat):
  echo        "%WDKROOT%"
  exit /b 1
)

exit /b 0

