@echo off
setlocal EnableExtensions EnableDelayedExpansion

rem -----------------------------------------------------------------------------
rem build_one.cmd
rem
rem Runs a single WDK BUILD invocation (setenv + build) and copies output artifacts
rem into a deterministic out/ folder.
rem
rem Args:
rem   %1 WDKROOT (e.g. C:\WinDDK\7600.16385.1)
rem   %2 OS     (WIN7)
rem   %3 VAR    (fre|chk)
rem   %4 ARCH   (x86|x64)
rem   %5 SRCDIR (component dir containing "sources" or "dirs")
rem   %6 OUTDIR (where artifacts get copied)
rem   %7 BINEXT (sys|dll)
rem
rem Special:
rem   build_one.cmd --selftest
rem -----------------------------------------------------------------------------

if /i "%~1"=="--selftest" (
  rem A tiny check that cmd parsing isn't broken (common with incorrect line endings).
  exit /b 0
)

set "WDKROOT=%~1"
set "TARGET_OS=%~2"
set "VARIANT=%~3"
set "ARCH=%~4"
set "SRCDIR=%~5"
set "OUTDIR=%~6"
set "BINEXT=%~7"

if "%WDKROOT%"=="" exit /b 2
if "%TARGET_OS%"=="" exit /b 2
if "%VARIANT%"=="" exit /b 2
if "%ARCH%"=="" exit /b 2
if "%SRCDIR%"=="" exit /b 2
if "%OUTDIR%"=="" exit /b 2
if "%BINEXT%"=="" exit /b 2

set "SETENV=%WDKROOT%\bin\setenv.cmd"
if not exist "%SETENV%" set "SETENV=%WDKROOT%\bin\setenv.bat"
if not exist "%SETENV%" (
  echo ERROR: Could not find setenv.cmd/.bat under "%WDKROOT%\bin"
  exit /b 1
)

if not exist "%SRCDIR%" (
  echo ERROR: Source dir does not exist: "%SRCDIR%"
  exit /b 1
)

if not exist "%SRCDIR%\sources" if not exist "%SRCDIR%\dirs" (
  echo ERROR: "%SRCDIR%" does not contain a WDK BUILD entrypoint ("sources" or "dirs").
  exit /b 1
)

set "OBJARCH="
if /i "%ARCH%"=="x86" set "OBJARCH=x86"
if /i "%ARCH%"=="x64" set "OBJARCH=amd64"
if not defined OBJARCH (
  echo ERROR: Unknown arch "%ARCH%" (expected x86 or x64)
  exit /b 1
)

set "OBJDIR=obj%VARIANT%_win7_%OBJARCH%"

pushd "%SRCDIR%" >nul

call "%SETENV%" "%WDKROOT%" %VARIANT% %ARCH% %TARGET_OS%
if errorlevel 1 (
  echo ERROR: setenv failed for %VARIANT% %ARCH% %TARGET_OS%
  popd >nul
  exit /b 1
)

cd /d "%SRCDIR%"

rem -cZ: clean + create log + build (common, deterministic-ish invocation)
build -cZ
if errorlevel 1 (
  echo ERROR: build.exe failed in "%SRCDIR%" for %VARIANT% %ARCH% %TARGET_OS%
  popd >nul
  exit /b 1
)

if not exist "%OBJDIR%" (
  echo ERROR: Expected obj directory not found after build: "%SRCDIR%\%OBJDIR%"
  popd >nul
  exit /b 1
)

if exist "%OUTDIR%" rmdir /s /q "%OUTDIR%"
mkdir "%OUTDIR%" >nul 2>nul

rem Copy all produced binaries of the requested type, plus matching PDB/MAP/LIB if present.
for /r "%OBJDIR%" %%F in (*.%BINEXT%) do (
  copy /y "%%~fF" "%OUTDIR%\" >nul
  if exist "%%~dpnF.pdb" copy /y "%%~dpnF.pdb" "%OUTDIR%\" >nul
  if exist "%%~dpnF.map" copy /y "%%~dpnF.map" "%OUTDIR%\" >nul
  if exist "%%~dpnF.lib" copy /y "%%~dpnF.lib" "%OUTDIR%\" >nul
)

dir /b "%OUTDIR%\*.%BINEXT%" >nul 2>nul
if errorlevel 1 (
  echo ERROR: Build completed but no *.%BINEXT% artifacts were found.
  echo        Check TARGETTYPE/TARGETNAME in "%SRCDIR%\sources".
  popd >nul
  exit /b 1
)

popd >nul
exit /b 0
