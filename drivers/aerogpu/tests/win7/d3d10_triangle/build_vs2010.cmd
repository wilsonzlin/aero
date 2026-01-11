@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d10_triangle] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d10_triangle.exe" user32.lib gdi32.lib d3d10.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d10_triangle] OK: %OUTDIR%\\d3d10_triangle.exe
exit /b 0
