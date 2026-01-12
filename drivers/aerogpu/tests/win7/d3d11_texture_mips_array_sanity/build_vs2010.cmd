@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d11_texture_mips_array_sanity] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d11_texture_mips_array_sanity.exe" d3d11.lib dxgi.lib
if errorlevel 1 exit /b 1

echo [d3d11_texture_mips_array_sanity] OK: %OUTDIR%\\d3d11_texture_mips_array_sanity.exe
exit /b 0

