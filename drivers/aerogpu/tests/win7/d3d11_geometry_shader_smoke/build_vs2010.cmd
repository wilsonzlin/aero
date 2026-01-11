@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d11_geometry_shader_smoke] Building shaders...

where fxc >nul 2>nul
if errorlevel 1 (
  echo [d3d11_geometry_shader_smoke] ERROR: fxc.exe not found on PATH. Install DirectX SDK ^(June 2010^) and add fxc to PATH.
  exit /b 1
)

fxc /nologo /T vs_4_0 /E vs_main /Fo "%OUTDIR%\\d3d11_geometry_shader_smoke_vs.cso" "%~dp0gs.hlsl"
if errorlevel 1 exit /b 1
fxc /nologo /T gs_4_0 /E gs_main /Fo "%OUTDIR%\\d3d11_geometry_shader_smoke_gs.cso" "%~dp0gs.hlsl"
if errorlevel 1 exit /b 1
fxc /nologo /T ps_4_0 /E ps_main /Fo "%OUTDIR%\\d3d11_geometry_shader_smoke_ps.cso" "%~dp0gs.hlsl"
if errorlevel 1 exit /b 1

echo [d3d11_geometry_shader_smoke] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d11_geometry_shader_smoke.exe" d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d11_geometry_shader_smoke] OK: %OUTDIR%\\d3d11_geometry_shader_smoke.exe
exit /b 0

