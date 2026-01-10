@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [dwm_flush_pacing] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\dwm_flush_pacing.exe" user32.lib dwmapi.lib
if errorlevel 1 exit /b 1

echo [dwm_flush_pacing] OK: %OUTDIR%\\dwm_flush_pacing.exe
exit /b 0

