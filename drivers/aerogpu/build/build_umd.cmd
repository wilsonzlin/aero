@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem build_umd.cmd
rem
rem Builds the AeroGPU D3D10/11 UMD using MSBuild and places artifacts in OUTDIR.
rem
rem Args:
rem   %1 VAR    (fre|chk)              -> fre maps to Release, chk maps to Debug
rem   %2 ARCH   (x86|x64)              -> x86 maps to Win32, x64 maps to x64
rem   %3 SLN    (path to .sln)
rem   %4 OUTDIR (where *.dll/*.pdb are written)
rem -----------------------------------------------------------------------------

set "VARIANT=%~1"
set "ARCH=%~2"
set "SLN=%~3"
set "OUTDIR=%~4"

if "%VARIANT%"=="" exit /b 2
if "%ARCH%"=="" exit /b 2
if "%SLN%"=="" exit /b 2
if "%OUTDIR%"=="" exit /b 2

if not exist "%SLN%" (
  echo ERROR: UMD solution not found: "%SLN%"
  exit /b 1
)

set "CONFIG="
if /i "%VARIANT%"=="fre" set "CONFIG=Release"
if /i "%VARIANT%"=="chk" set "CONFIG=Debug"
if not defined CONFIG (
  echo ERROR: Unknown build variant "%VARIANT%" (expected fre or chk)
  exit /b 1
)

set "PLATFORM="
if /i "%ARCH%"=="x86" set "PLATFORM=Win32"
if /i "%ARCH%"=="x64" set "PLATFORM=x64"
if not defined PLATFORM (
  echo ERROR: Unknown arch "%ARCH%" (expected x86 or x64)
  exit /b 1
)

call :find_msbuild
if errorlevel 1 exit /b 1

if exist "%OUTDIR%" rmdir /s /q "%OUTDIR%"
mkdir "%OUTDIR%" >nul 2>nul
mkdir "%OUTDIR%\obj" >nul 2>nul

set "OUTDIR_MSBUILD=%OUTDIR%\"
set "INTDIR_MSBUILD=%OUTDIR%\obj\"

echo [UMD] MSBuild: "%MSBUILD%"
echo [UMD] Config:  %CONFIG%  Platform: %PLATFORM%

"%MSBUILD%" "%SLN%" /m /t:Build ^
  /p:Configuration=%CONFIG% ^
  /p:Platform=%PLATFORM% ^
  /p:OutDir="%OUTDIR_MSBUILD%" ^
  /p:IntDir="%INTDIR_MSBUILD%" ^
  /nologo
if errorlevel 1 (
  echo ERROR: MSBuild failed for UMD (%CONFIG% %PLATFORM%).
  exit /b 1
)

dir /b "%OUTDIR%\*.dll" >nul 2>nul
if errorlevel 1 (
  echo ERROR: UMD build completed but no *.dll was produced in:
  echo        "%OUTDIR%"
  exit /b 1
)

exit /b 0

rem -----------------------------------------------------------------------------
:find_msbuild
set "MSBUILD="

for /f "delims=" %%M in ('where msbuild.exe 2^>nul') do (
  set "MSBUILD=%%~fM"
  goto :msbuild_found
)

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" (
  for /f "delims=" %%M in ('"%VSWHERE%" -latest -products * -requires Microsoft.Component.MSBuild -find MSBuild\**\Bin\MSBuild.exe 2^>nul') do (
    set "MSBUILD=%%~fM"
    goto :msbuild_found
  )
)

:msbuild_found
if not defined MSBUILD (
  echo ERROR: msbuild.exe not found.
  echo        Install Visual Studio (or Build Tools) with MSBuild + C++ workload,
  echo        or run this script from a "Developer Command Prompt" where msbuild is on PATH.
  exit /b 1
)

exit /b 0
