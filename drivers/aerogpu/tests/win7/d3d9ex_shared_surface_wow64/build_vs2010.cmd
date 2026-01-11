@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d9ex_shared_surface_wow64] Building x86 producer + x64 consumer...

set "VSVARSALL="
if defined VS100COMNTOOLS set "VSVARSALL=%VS100COMNTOOLS%..\\..\\VC\\vcvarsall.bat"
if not exist "%VSVARSALL%" if defined VCINSTALLDIR set "VSVARSALL=%VCINSTALLDIR%vcvarsall.bat"
if not exist "%VSVARSALL%" (
  echo [d3d9ex_shared_surface_wow64] ERROR: could not locate vcvarsall.bat. Run from a VS2010 toolchain environment.
  exit /b 1
)

set "PRODUCER_OUT=%OUTDIR%\\d3d9ex_shared_surface_wow64.exe"
set "CONSUMER_OUT=%OUTDIR%\\d3d9ex_shared_surface_wow64_consumer_x64.exe"

rem Build the 32-bit producer (runs under WOW64 on Win7 x64).
call "%VSVARSALL%" x86 >nul
if errorlevel 1 exit /b 1
cl /nologo /W4 /EHsc /O2 /MT "%~dp0producer_main.cpp" ^
  /link /OUT:"%PRODUCER_OUT%" user32.lib gdi32.lib d3d9.lib
if errorlevel 1 exit /b 1

rem Build the 64-bit consumer (opened by the producer via CreateProcess + DuplicateHandle).
rem Use x86-hosted cross tools so this also builds from a 32-bit Win7 guest/toolchain.
call "%VSVARSALL%" x86_amd64 >nul
if errorlevel 1 (
  rem Fall back to native amd64 tools (requires a 64-bit host OS).
  call "%VSVARSALL%" amd64 >nul
  if errorlevel 1 exit /b 1
)
cl /nologo /W4 /EHsc /O2 /MT "%~dp0consumer_main.cpp" ^
  /link /OUT:"%CONSUMER_OUT%" user32.lib gdi32.lib d3d9.lib
if errorlevel 1 exit /b 1

echo [d3d9ex_shared_surface_wow64] OK: %PRODUCER_OUT%
echo [d3d9ex_shared_surface_wow64] OK: %CONSUMER_OUT%
exit /b 0
