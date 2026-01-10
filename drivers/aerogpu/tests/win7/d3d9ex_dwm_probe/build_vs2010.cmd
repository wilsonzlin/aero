@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [d3d9ex_dwm_probe] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\d3d9ex_dwm_probe.exe" user32.lib dwmapi.lib
if errorlevel 1 exit /b 1

echo [d3d9ex_dwm_probe] OK: %OUTDIR%\\d3d9ex_dwm_probe.exe
exit /b 0

