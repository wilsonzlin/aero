@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [readback_sanity] Building shaders...

where fxc >nul 2>nul
if errorlevel 1 (
  echo [readback_sanity] ERROR: fxc.exe not found on PATH. Install DirectX SDK ^(June 2010^) and add fxc to PATH.
  exit /b 1
)

fxc /nologo /T vs_4_0_level_9_1 /E vs_main /Fo "%OUTDIR%\\readback_sanity_vs.cso" "%~dp0pattern.hlsl"
if errorlevel 1 exit /b 1
fxc /nologo /T ps_4_0_level_9_1 /E ps_main /Fo "%OUTDIR%\\readback_sanity_ps.cso" "%~dp0pattern.hlsl"
if errorlevel 1 exit /b 1

echo [readback_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\readback_sanity.exe" d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [readback_sanity] OK: %OUTDIR%\\readback_sanity.exe
exit /b 0

