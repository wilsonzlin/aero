@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [dxgi_swapchain_probe] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\dxgi_swapchain_probe.exe" user32.lib gdi32.lib dxgi.lib d3d11.lib d3d10.lib d3d10_1.lib
if errorlevel 1 exit /b 1

echo [dxgi_swapchain_probe] OK: %OUTDIR%\\dxgi_swapchain_probe.exe
exit /b 0

