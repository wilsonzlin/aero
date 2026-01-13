@echo off
setlocal

REM build_vs2010.cmd
REM
REM Build hidtest.exe using MSVC cl.exe from a Visual Studio/Windows SDK
REM developer prompt. Optionally copy the resulting hidtest.exe into an output
REM directory (e.g. a built driver package folder) for manual testing.

if /I "%~1"=="/?" goto :usage
if /I "%~1"=="-h" goto :usage
if /I "%~1"=="--help" goto :usage

REM Resolve script dir (always ends with a backslash).
set "SRCDIR=%~dp0"

REM Optional: copy destination directory.
set "COPYDIR=%~1"

REM Where we place build artifacts.
set "OUTDIR=%SRCDIR%bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

REM Fail early with a clear error if cl.exe isn't available on PATH.
where cl.exe >nul 2>nul
if errorlevel 1 (
  echo [hidtest] ERROR: cl.exe not found.
  echo [hidtest]        Run this script from a Visual Studio / Windows SDK Developer Command Prompt.
  echo [hidtest]        For example: "Visual Studio 2010 Command Prompt" or "x64 Native Tools Command Prompt".
  exit /b 1
)

echo [hidtest] Building...

cl /nologo /W4 /O2 /MT /D_CRT_SECURE_NO_WARNINGS ^
  /Fo"%OUTDIR%\main.obj" "%SRCDIR%main.c" ^
  /link /OUT:"%OUTDIR%\hidtest.exe" setupapi.lib hid.lib
if errorlevel 1 exit /b 1

echo [hidtest] OK: %OUTDIR%\hidtest.exe

if defined COPYDIR (
  REM Create destination directory if needed.
  if not exist "%COPYDIR%" (
    mkdir "%COPYDIR%"
    if errorlevel 1 (
      echo [hidtest] ERROR: Failed to create output directory: "%COPYDIR%"
      exit /b 1
    )
  )

  echo [hidtest] Copying to: "%COPYDIR%\hidtest.exe"
  copy /Y "%OUTDIR%\hidtest.exe" "%COPYDIR%\hidtest.exe" >nul
  if errorlevel 1 (
    echo [hidtest] ERROR: Failed to copy hidtest.exe to: "%COPYDIR%"
    exit /b 1
  )
)

exit /b 0

:usage
echo Usage: %~nx0 [copy_dir]
echo.
echo Builds hidtest.exe with MSVC cl.exe.
echo.
echo copy_dir (optional):
echo   If provided, hidtest.exe is copied into this directory. This is useful for
echo   dropping the tool next to the packaged virtio-input driver files for easy
echo   transfer into a Windows 7 guest.
echo.
echo Examples:
echo   %~nx0
echo   %~nx0 out\packages\windows7\virtio-input\x64
exit /b 2

