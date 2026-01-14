@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d9_shader_stage_interop] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d9_shader_stage_interop.exe" user32.lib gdi32.lib d3d9.lib
if errorlevel 1 exit /b 1

echo [d3d9_shader_stage_interop] OK: %OUTDIR%\\d3d9_shader_stage_interop.exe
exit /b 0

