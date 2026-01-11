@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d11_swapchain_rotate_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d11_swapchain_rotate_sanity.exe" user32.lib gdi32.lib d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d11_swapchain_rotate_sanity] OK: %OUTDIR%\\d3d11_swapchain_rotate_sanity.exe
exit /b 0

