@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d10_shared_surface_ipc] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d10_shared_surface_ipc.exe" d3d10.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d10_shared_surface_ipc] OK: %OUTDIR%\\d3d10_shared_surface_ipc.exe
exit /b 0

