@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d9ex_fixedfunc_texture_stage_state] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d9ex_fixedfunc_texture_stage_state.exe" user32.lib gdi32.lib d3d9.lib
if errorlevel 1 exit /b 1

echo [d3d9ex_fixedfunc_texture_stage_state] OK: %OUTDIR%\\d3d9ex_fixedfunc_texture_stage_state.exe
exit /b 0

