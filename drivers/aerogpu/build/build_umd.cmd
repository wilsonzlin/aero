@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem build_umd.cmd
rem
rem Builds an MSBuild project/solution and places artifacts in OUTDIR.
rem
rem Args:
rem   %1 VAR    (fre|chk)              -> fre maps to Release, chk maps to Debug
rem   %2 ARCH   (x86|x64)              -> x86 maps to Win32, x64 maps to x64
rem   %3 SLN    (path to .sln or .vcxproj)
rem   %4 OUTDIR (where *.dll/*.pdb are written)
rem   %5 OBJDIR (optional; per-project intermediates)
rem   %6 EXPECTED_OUTPUT (optional; asserts the expected output exists after build)
rem -----------------------------------------------------------------------------

set "VARIANT=%~1"
set "ARCH=%~2"
set "SLN=%~3"
set "OUTDIR=%~4"
set "OBJDIR=%~5"
set "EXPECTED_OUTPUT=%~6"

if "%VARIANT%"=="" exit /b 2
if "%ARCH%"=="" exit /b 2
if "%SLN%"=="" exit /b 2
if "%OUTDIR%"=="" exit /b 2

if not exist "%SLN%" (
  echo ERROR: Build input not found: "%SLN%"
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

mkdir "%OUTDIR%" >nul 2>nul

if not defined OBJDIR set "OBJDIR=%OUTDIR%\obj"

if exist "%OBJDIR%" rmdir /s /q "%OBJDIR%"
mkdir "%OBJDIR%" >nul 2>nul

set "OUTDIR_MSBUILD=%OUTDIR%\"
set "INTDIR_MSBUILD=%OBJDIR%\"

if not "%EXPECTED_OUTPUT%"=="" (
  if exist "%OUTDIR%\%EXPECTED_OUTPUT%" del /f /q "%OUTDIR%\%EXPECTED_OUTPUT%" >nul 2>nul
)

echo [MSBUILD] MSBuild: "%MSBUILD%"
echo [MSBUILD] Config:  %CONFIG%  Platform: %PLATFORM%

"%MSBUILD%" "%SLN%" /m /t:Build ^
  /p:Configuration=%CONFIG% ^
  /p:Platform=%PLATFORM% ^
  /p:OutDir="%OUTDIR_MSBUILD%" ^
  /p:IntDir="%INTDIR_MSBUILD%" ^
  /nologo
if errorlevel 1 (
  echo ERROR: MSBuild failed (%CONFIG% %PLATFORM%).
  exit /b 1
)

if not "%EXPECTED_OUTPUT%"=="" (
  if not exist "%OUTDIR%\%EXPECTED_OUTPUT%" (
    echo ERROR: Build completed but expected output was not produced:
    echo        "%OUTDIR%\%EXPECTED_OUTPUT%"
    exit /b 1
  )
  exit /b 0
)

dir /b "%OUTDIR%\*.dll" >nul 2>nul
if errorlevel 1 (
  echo ERROR: Build completed but no *.dll was produced in:
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
