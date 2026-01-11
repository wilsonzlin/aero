@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d11_texture] Building shaders...

where fxc >nul 2>nul
if errorlevel 1 (
  echo [d3d11_texture] ERROR: fxc.exe not found on PATH. Install DirectX SDK ^(June 2010^) and add fxc to PATH.
  exit /b 1
)

fxc /nologo /T vs_4_0_level_9_1 /E vs_main /Fo "%OUTDIR%\\d3d11_texture_vs.cso" "%~dp0texture.hlsl"
if errorlevel 1 exit /b 1
fxc /nologo /T ps_4_0_level_9_1 /E ps_main /Fo "%OUTDIR%\\d3d11_texture_ps.cso" "%~dp0texture.hlsl"
if errorlevel 1 exit /b 1

echo [d3d11_texture] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d11_texture.exe" user32.lib gdi32.lib d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d11_texture] OK: %OUTDIR%\\d3d11_texture.exe
exit /b 0

