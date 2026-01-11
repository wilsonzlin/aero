@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d10_1_triangle] Building shaders...

where fxc >nul 2>nul
if errorlevel 1 (
  echo [d3d10_1_triangle] ERROR: fxc.exe not found on PATH. Install DirectX SDK ^(June 2010^) and add fxc to PATH.
  exit /b 1
)

fxc /nologo /T vs_4_0 /E vs_main /Fo "%OUTDIR%\\d3d10_1_triangle_vs.cso" "%~dp0triangle.hlsl"
if errorlevel 1 exit /b 1
fxc /nologo /T ps_4_0 /E ps_main /Fo "%OUTDIR%\\d3d10_1_triangle_ps.cso" "%~dp0triangle.hlsl"
if errorlevel 1 exit /b 1

echo [d3d10_1_triangle] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d10_1_triangle.exe" user32.lib gdi32.lib d3d10_1.lib d3d10.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d10_1_triangle] OK: %OUTDIR%\\d3d10_1_triangle.exe
exit /b 0

