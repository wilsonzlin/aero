@echo off
setlocal

set "OUTDIR=%~dp0..\\bin"
if not exist "%OUTDIR%" mkdir "%OUTDIR%"

echo [vblank_wait_pacing] Building...

cl /nologo /W4 /EHsc /O2 /MT "%~dp0main.cpp" ^
  /link /OUT:"%OUTDIR%\\vblank_wait_pacing.exe" user32.lib gdi32.lib
if errorlevel 1 exit /b 1

echo [vblank_wait_pacing] OK: %OUTDIR%\\vblank_wait_pacing.exe
exit /b 0

