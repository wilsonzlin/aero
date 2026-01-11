@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [readback_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\readback_sanity.exe" d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [readback_sanity] OK: %OUTDIR%\\readback_sanity.exe
exit /b 0

