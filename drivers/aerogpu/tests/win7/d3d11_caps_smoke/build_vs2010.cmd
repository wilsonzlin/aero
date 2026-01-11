@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d11_caps_smoke] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d11_caps_smoke.exe" d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d11_caps_smoke] OK: %OUTDIR%\\d3d11_caps_smoke.exe
exit /b 0

