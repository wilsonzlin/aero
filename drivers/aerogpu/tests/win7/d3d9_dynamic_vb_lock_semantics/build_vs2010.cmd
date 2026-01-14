@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d9_dynamic_vb_lock_semantics] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d9_dynamic_vb_lock_semantics.exe" user32.lib gdi32.lib d3d9.lib
if errorlevel 1 exit /b 1

echo [d3d9_dynamic_vb_lock_semantics] OK: %OUTDIR%\\d3d9_dynamic_vb_lock_semantics.exe
exit /b 0

